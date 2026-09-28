//! Daily notes: which file holds a given day, which day a file holds, and what a
//! new day's note starts with.
//!
//! A day's note lives at `folder/<the day in the date format>.md`. The format is a
//! Moment.js pattern, so a `/` in it files the notes into folders by year or month,
//! and a folder that another tool also keeps daily notes in names them the same way.
//! Formatting and reading back are one table of tokens: a file is a daily note only
//! when formatting the day it reads as spells its path again exactly.
//!
//! Nothing here touches the disk except [`read_obsidian`], which only reads.

use chrono::{Datelike, Days, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

pub const DEFAULT_FORMAT: &str = "YYYY-MM-DD";
/// What `{{time}}` writes when the template names no format.
const DEFAULT_TIME_FORMAT: &str = "HH:mm";

/// Where daily notes go and what they start as, kept per notes folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DailySettings {
    /// Relative to the notes folder; empty is the folder itself.
    pub folder: PathBuf,
    pub format: String,
    /// A note relative to the notes folder whose text a new day starts from.
    pub template: Option<PathBuf>,
}

impl Default for DailySettings {
    fn default() -> Self {
        Self {
            folder: PathBuf::new(),
            format: DEFAULT_FORMAT.to_owned(),
            template: None,
        }
    }
}

impl DailySettings {
    fn pattern(&self) -> Pattern {
        Pattern::new(if self.format.trim().is_empty() {
            DEFAULT_FORMAT
        } else {
            &self.format
        })
    }

    /// The note for `day`, relative to the notes folder.
    pub fn path_for(&self, day: NaiveDate, locale: DateLocale) -> PathBuf {
        let mut path = self.folder.clone();
        let name = self.pattern().format(day.and_time(NaiveTime::MIN), locale);
        for segment in name.split('/') {
            path.push(segment);
        }
        path.set_extension("md");
        path
    }

    /// The day a note at `relative` (to the notes folder) stands for, if it is one.
    ///
    /// The whole path under the daily folder is read first. A note that another tool
    /// filed elsewhere under that folder still counts when its file name alone reads
    /// as the format's last segment.
    pub fn day_of(&self, relative: &Path, locale: DateLocale) -> Option<NaiveDate> {
        let under = relative.strip_prefix(&self.folder).ok()?;
        let extension = under.extension()?.to_str()?;
        if !extension.eq_ignore_ascii_case("md") && !extension.eq_ignore_ascii_case("markdown") {
            return None;
        }
        let stem = under.with_extension("");
        let mut segments = Vec::new();
        for component in stem.components() {
            let Component::Normal(segment) = component else {
                return None;
            };
            segments.push(segment.to_str()?);
        }
        let pattern = self.pattern();
        let joined = segments.join("/");
        for locale in locale.with_english() {
            if let Some(day) = pattern.parse(&joined, locale) {
                return Some(day);
            }
        }
        let last = Pattern::new(self.format_or_default().rsplit('/').next()?);
        let name = segments.last()?;
        locale
            .with_english()
            .into_iter()
            .find_map(|locale| last.parse(name, locale))
    }

    fn format_or_default(&self) -> &str {
        if self.format.trim().is_empty() {
            DEFAULT_FORMAT
        } else {
            &self.format
        }
    }

    /// What a new note for `day`, created at `now`, starts with: the template's text
    /// with its date and time placeholders filled in.
    pub fn expand(
        &self,
        template: &str,
        day: NaiveDate,
        now: NaiveDateTime,
        locale: DateLocale,
    ) -> String {
        let name_pattern = Pattern::new(self.format_or_default().rsplit('/').next().unwrap_or(""));
        let at = day.and_time(now.time());
        let name = |day: NaiveDate| name_pattern.format(day.and_time(NaiveTime::MIN), locale);
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                out.push_str(&rest[start..]);
                return out;
            };
            let inner = &after[..end];
            match placeholder(inner, at, now, &name, locale) {
                Some(value) => out.push_str(&value),
                None => {
                    out.push_str("{{");
                    out.push_str(inner);
                    out.push_str("}}");
                }
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        out
    }
}

/// One `{{…}}`: `date`, `title`, `time`, `yesterday`, `tomorrow`, or `date` / `time`
/// with an offset such as `+1d` and a format after a colon. Anything else is left as
/// it was written.
fn placeholder(
    inner: &str,
    at: NaiveDateTime,
    now: NaiveDateTime,
    name: &impl Fn(NaiveDate) -> String,
    locale: DateLocale,
) -> Option<String> {
    let trimmed = inner.trim();
    let (head, format) = match trimmed.split_once(':') {
        Some((head, format)) => (head.trim(), Some(format.trim())),
        None => (trimmed, None),
    };
    let lower = head.to_ascii_lowercase();
    match (lower.as_str(), format) {
        ("title", None) => return Some(name(at.date())),
        ("yesterday", None) => return Some(name(at.date().pred_opt()?)),
        ("tomorrow", None) => return Some(name(at.date().succ_opt()?)),
        _ => {}
    }
    let (base, offset) = if let Some(offset) = lower.strip_prefix("date") {
        (at, offset)
    } else if let Some(offset) = lower.strip_prefix("time") {
        (now, offset)
    } else {
        return None;
    };
    // The unit keeps its case: `M` is a month and `m` a minute.
    let offset_raw = head[head.len() - offset.len()..].trim();
    let moment = if offset_raw.is_empty() {
        base
    } else {
        shift(base, offset_raw)?
    };
    let is_time = lower.starts_with("time");
    Some(match format {
        Some(format) => Pattern::new(format).format(moment, locale),
        None if is_time => Pattern::new(DEFAULT_TIME_FORMAT).format(moment, locale),
        None => name(moment.date()),
    })
}

/// `+1d`, `-2w`, `+3M`: a signed count and one unit.
fn shift(at: NaiveDateTime, offset: &str) -> Option<NaiveDateTime> {
    let (sign, rest) = match offset.as_bytes().first()? {
        b'+' => (1i64, &offset[1..]),
        b'-' => (-1i64, &offset[1..]),
        _ => return None,
    };
    let unit = rest.chars().last()?;
    let count: i64 = rest[..rest.len() - unit.len_utf8()].trim().parse().ok()?;
    let count = sign * count;
    let months = |months: i64| {
        let magnitude = Months::new(u32::try_from(months.unsigned_abs()).ok()?);
        if months >= 0 {
            at.checked_add_months(magnitude)
        } else {
            at.checked_sub_months(magnitude)
        }
    };
    match unit {
        'y' | 'Y' => months(count * 12),
        'Q' | 'q' => months(count * 3),
        'M' => months(count),
        'w' | 'W' => at.checked_add_signed(Duration::try_weeks(count)?),
        'd' | 'D' => at.checked_add_signed(Duration::try_days(count)?),
        'h' | 'H' => at.checked_add_signed(Duration::try_hours(count)?),
        'm' => at.checked_add_signed(Duration::try_minutes(count)?),
        's' => at.checked_add_signed(Duration::try_seconds(count)?),
        _ => None,
    }
}

/// Why a date format cannot name daily notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatProblem {
    Empty,
    /// Two days get the same name, or a name does not read back as its day.
    NotOneDay,
    /// A name is not one a file can have, or one a wiki link could reach.
    UnsafeName,
}

impl FormatProblem {
    pub fn message_key(self) -> &'static str {
        match self {
            FormatProblem::Empty => "settings.daily-format-empty",
            FormatProblem::NotOneDay => "settings.daily-format-ambiguous",
            FormatProblem::UnsafeName => "settings.daily-format-unsafe",
        }
    }
}

/// Whether `format` gives every day a file of its own that reads back as that day.
pub fn validate_format(format: &str, locale: DateLocale) -> Result<(), FormatProblem> {
    if format.trim().is_empty() {
        return Err(FormatProblem::Empty);
    }
    let settings = DailySettings {
        format: format.to_owned(),
        ..DailySettings::default()
    };
    let pattern = settings.pattern();
    // Two whole years, one of them leap, and the turn of a year whose first week
    // starts in the one before.
    let first = NaiveDate::from_ymd_opt(2024, 1, 1).expect("a valid date");
    for offset in 0..(366 + 365 + 7) {
        let day = first + Days::new(offset);
        let name = pattern.format(day.and_time(NaiveTime::MIN), locale);
        for segment in name.split('/') {
            let unsafe_segment = segment.trim().is_empty()
                || segment.starts_with('.')
                || segment.chars().any(|c| {
                    matches!(c, ':' | '\\' | '[' | ']' | '#' | '^' | '|') || c.is_control()
                });
            if unsafe_segment {
                return Err(FormatProblem::UnsafeName);
            }
        }
        if pattern.parse(&name, locale) != Some(day) {
            return Err(FormatProblem::NotOneDay);
        }
    }
    Ok(())
}

/// The language month and weekday names are written in, as Moment.js spells them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateLocale {
    English,
    SimplifiedChinese,
    TraditionalChinese,
}

impl DateLocale {
    /// From a BCP 47 language as the system reports it.
    pub fn from_language(language: &str) -> Self {
        let lower = language.to_ascii_lowercase();
        if lower.starts_with("zh-hant")
            || lower.starts_with("zh-tw")
            || lower.starts_with("zh-hk")
            || lower.starts_with("zh-mo")
        {
            DateLocale::TraditionalChinese
        } else if lower.starts_with("zh") {
            DateLocale::SimplifiedChinese
        } else {
            DateLocale::English
        }
    }

    /// Itself, then English: names another tool wrote in English still read back.
    fn with_english(self) -> Vec<DateLocale> {
        if self == DateLocale::English {
            vec![self]
        } else {
            vec![self, DateLocale::English]
        }
    }

    fn months(self) -> [&'static str; 12] {
        match self {
            DateLocale::English => [
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ],
            _ => [
                "一月",
                "二月",
                "三月",
                "四月",
                "五月",
                "六月",
                "七月",
                "八月",
                "九月",
                "十月",
                "十一月",
                "十二月",
            ],
        }
    }

    fn months_short(self) -> [&'static str; 12] {
        match self {
            DateLocale::English => [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ],
            _ => [
                "1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月",
                "12月",
            ],
        }
    }

    /// Sunday first, as Moment.js numbers them.
    fn weekdays(self) -> [&'static str; 7] {
        match self {
            DateLocale::English => [
                "Sunday",
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
            ],
            _ => [
                "星期日",
                "星期一",
                "星期二",
                "星期三",
                "星期四",
                "星期五",
                "星期六",
            ],
        }
    }

    fn weekdays_short(self) -> [&'static str; 7] {
        match self {
            DateLocale::English => ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
            DateLocale::SimplifiedChinese => {
                ["周日", "周一", "周二", "周三", "周四", "周五", "周六"]
            }
            DateLocale::TraditionalChinese => {
                ["週日", "週一", "週二", "週三", "週四", "週五", "週六"]
            }
        }
    }

    fn weekdays_min(self) -> [&'static str; 7] {
        match self {
            DateLocale::English => ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"],
            _ => ["日", "一", "二", "三", "四", "五", "六"],
        }
    }

    /// The first day of the week (0 is Sunday) and the January day always in week 1.
    fn week_rule(self) -> (u32, u32) {
        match self {
            DateLocale::SimplifiedChinese => (1, 4),
            DateLocale::English | DateLocale::TraditionalChinese => (0, 6),
        }
    }

    fn meridiem(self, hour: u32, minute: u32, upper: bool) -> String {
        match self {
            DateLocale::English => match (hour < 12, upper) {
                (true, true) => "AM",
                (true, false) => "am",
                (false, true) => "PM",
                (false, false) => "pm",
            }
            .to_owned(),
            _ => {
                let at = hour * 100 + minute;
                match at {
                    0..600 => "凌晨",
                    600..900 => "早上",
                    900..1130 => "上午",
                    1130..1230 => "中午",
                    1230..1800 => "下午",
                    _ => "晚上",
                }
                .to_owned()
            }
        }
    }

    fn ordinal(self, number: u32, period: Period) -> String {
        match self {
            DateLocale::English => {
                let suffix = match (number % 100, number % 10) {
                    (11..=13, _) => "th",
                    (_, 1) => "st",
                    (_, 2) => "nd",
                    (_, 3) => "rd",
                    _ => "th",
                };
                format!("{number}{suffix}")
            }
            chinese => {
                let suffix = match period {
                    Period::Day => "日",
                    Period::Month => "月",
                    Period::Week if chinese == DateLocale::SimplifiedChinese => "周",
                    Period::Week => "週",
                    Period::Quarter => "",
                };
                format!("{number}{suffix}")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Period {
    Day,
    Month,
    Week,
    Quarter,
}

/// A Moment.js token this module reads and writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Year4,
    Year2,
    Quarter,
    QuarterOrdinal,
    Month,
    Month2,
    MonthOrdinal,
    MonthShort,
    MonthLong,
    Day,
    Day2,
    DayOrdinal,
    DayOfYear,
    DayOfYear3,
    DayOfYearOrdinal,
    Weekday,
    WeekdayOrdinal,
    WeekdayMin,
    WeekdayShort,
    WeekdayLong,
    LocaleWeekday,
    IsoWeekday,
    Week,
    Week2,
    WeekOrdinal,
    IsoWeek,
    IsoWeek2,
    IsoWeekOrdinal,
    WeekYear4,
    WeekYear2,
    IsoWeekYear4,
    IsoWeekYear2,
    Hour,
    Hour2,
    Hour12,
    Hour12Two,
    Hour24,
    Hour24Two,
    Minute,
    Minute2,
    Second,
    Second2,
    MeridiemUpper,
    MeridiemLower,
}

/// Longest first where one spelling starts another, in Moment.js's own order.
const TOKENS: &[(&str, Token)] = &[
    ("Mo", Token::MonthOrdinal),
    ("MMMM", Token::MonthLong),
    ("MMM", Token::MonthShort),
    ("MM", Token::Month2),
    ("M", Token::Month),
    ("Do", Token::DayOrdinal),
    ("DDDo", Token::DayOfYearOrdinal),
    ("DDDD", Token::DayOfYear3),
    ("DDD", Token::DayOfYear),
    ("DD", Token::Day2),
    ("D", Token::Day),
    ("dddd", Token::WeekdayLong),
    ("ddd", Token::WeekdayShort),
    ("dd", Token::WeekdayMin),
    ("do", Token::WeekdayOrdinal),
    ("d", Token::Weekday),
    ("wo", Token::WeekOrdinal),
    ("ww", Token::Week2),
    ("w", Token::Week),
    ("Wo", Token::IsoWeekOrdinal),
    ("WW", Token::IsoWeek2),
    ("W", Token::IsoWeek),
    ("Qo", Token::QuarterOrdinal),
    ("Q", Token::Quarter),
    ("YYYY", Token::Year4),
    ("YY", Token::Year2),
    ("gggg", Token::WeekYear4),
    ("gg", Token::WeekYear2),
    ("GGGG", Token::IsoWeekYear4),
    ("GG", Token::IsoWeekYear2),
    ("e", Token::LocaleWeekday),
    ("E", Token::IsoWeekday),
    ("a", Token::MeridiemLower),
    ("A", Token::MeridiemUpper),
    ("hh", Token::Hour12Two),
    ("h", Token::Hour12),
    ("HH", Token::Hour2),
    ("H", Token::Hour),
    ("kk", Token::Hour24Two),
    ("k", Token::Hour24),
    ("mm", Token::Minute2),
    ("m", Token::Minute),
    ("ss", Token::Second2),
    ("s", Token::Second),
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Token(Token),
}

/// A Moment.js format, split into its tokens and the text between them.
#[derive(Clone, Debug)]
struct Pattern(Vec<Piece>);

impl Pattern {
    fn new(format: &str) -> Self {
        fn literal(pieces: &mut Vec<Piece>, text: &str) {
            match pieces.last_mut() {
                Some(Piece::Literal(last)) => last.push_str(text),
                _ => pieces.push(Piece::Literal(text.to_owned())),
            }
        }
        let mut pieces: Vec<Piece> = Vec::new();
        let mut rest = format;
        while let Some(first) = rest.chars().next() {
            if first == '['
                && let Some(end) = rest[1..].find(']')
                && !rest[1..1 + end].contains('[')
            {
                literal(&mut pieces, &rest[1..1 + end]);
                rest = &rest[end + 2..];
                continue;
            }
            if first == '\\'
                && let Some(next) = rest[1..].chars().next()
            {
                literal(&mut pieces, &rest[1..1 + next.len_utf8()]);
                rest = &rest[1 + next.len_utf8()..];
                continue;
            }
            if let Some((spelling, token)) = TOKENS
                .iter()
                .find(|(spelling, _)| rest.starts_with(spelling))
            {
                pieces.push(Piece::Token(*token));
                rest = &rest[spelling.len()..];
                continue;
            }
            literal(&mut pieces, &rest[..first.len_utf8()]);
            rest = &rest[first.len_utf8()..];
        }
        Pattern(pieces)
    }

    fn format(&self, at: NaiveDateTime, locale: DateLocale) -> String {
        let mut out = String::new();
        for piece in &self.0 {
            match piece {
                Piece::Literal(text) => out.push_str(text),
                Piece::Token(token) => out.push_str(&write_token(*token, at, locale)),
            }
        }
        out
    }

    /// The day `text` names in this format, strictly: formatting that day (at the time
    /// the text gives, if any) must spell `text` again.
    fn parse(&self, text: &str, locale: DateLocale) -> Option<NaiveDate> {
        let mut fields = Fields::default();
        let mut rest = text;
        for piece in &self.0 {
            match piece {
                Piece::Literal(literal) => rest = rest.strip_prefix(literal.as_str())?,
                Piece::Token(token) => rest = read_token(*token, rest, locale, &mut fields)?,
            }
        }
        if !rest.is_empty() {
            return None;
        }
        let day = fields.day(locale)?;
        let time = NaiveTime::from_hms_opt(fields.hour()?, fields.minute, fields.second)?;
        (self.format(day.and_time(time), locale) == text).then_some(day)
    }
}

fn write_token(token: Token, at: NaiveDateTime, locale: DateLocale) -> String {
    let date = at.date();
    let weekday = date.weekday().num_days_from_sunday();
    let (dow, doy) = locale.week_rule();
    let (week_year, week) = week_of_year(date, dow, doy);
    let iso = date.iso_week();
    let hour12 = match at.hour() % 12 {
        0 => 12,
        hour => hour,
    };
    let quarter = (date.month() - 1) / 3 + 1;
    match token {
        Token::Year4 => format!("{:04}", date.year()),
        Token::Year2 => format!("{:02}", date.year().rem_euclid(100)),
        Token::Quarter => quarter.to_string(),
        Token::QuarterOrdinal => locale.ordinal(quarter, Period::Quarter),
        Token::Month => date.month().to_string(),
        Token::Month2 => format!("{:02}", date.month()),
        Token::MonthOrdinal => locale.ordinal(date.month(), Period::Month),
        Token::MonthShort => locale.months_short()[date.month0() as usize].to_owned(),
        Token::MonthLong => locale.months()[date.month0() as usize].to_owned(),
        Token::Day => date.day().to_string(),
        Token::Day2 => format!("{:02}", date.day()),
        Token::DayOrdinal => locale.ordinal(date.day(), Period::Day),
        Token::DayOfYear => date.ordinal().to_string(),
        Token::DayOfYear3 => format!("{:03}", date.ordinal()),
        Token::DayOfYearOrdinal => locale.ordinal(date.ordinal(), Period::Day),
        Token::Weekday => weekday.to_string(),
        Token::WeekdayOrdinal => locale.ordinal(weekday, Period::Day),
        Token::WeekdayMin => locale.weekdays_min()[weekday as usize].to_owned(),
        Token::WeekdayShort => locale.weekdays_short()[weekday as usize].to_owned(),
        Token::WeekdayLong => locale.weekdays()[weekday as usize].to_owned(),
        Token::LocaleWeekday => ((weekday + 7 - dow) % 7).to_string(),
        Token::IsoWeekday => date.weekday().number_from_monday().to_string(),
        Token::Week => week.to_string(),
        Token::Week2 => format!("{week:02}"),
        Token::WeekOrdinal => locale.ordinal(week, Period::Week),
        Token::IsoWeek => iso.week().to_string(),
        Token::IsoWeek2 => format!("{:02}", iso.week()),
        Token::IsoWeekOrdinal => locale.ordinal(iso.week(), Period::Week),
        Token::WeekYear4 => format!("{week_year:04}"),
        Token::WeekYear2 => format!("{:02}", week_year.rem_euclid(100)),
        Token::IsoWeekYear4 => format!("{:04}", iso.year()),
        Token::IsoWeekYear2 => format!("{:02}", iso.year().rem_euclid(100)),
        Token::Hour => at.hour().to_string(),
        Token::Hour2 => format!("{:02}", at.hour()),
        Token::Hour12 => hour12.to_string(),
        Token::Hour12Two => format!("{hour12:02}"),
        Token::Hour24 => match at.hour() {
            0 => 24,
            hour => hour,
        }
        .to_string(),
        Token::Hour24Two => format!(
            "{:02}",
            match at.hour() {
                0 => 24,
                hour => hour,
            }
        ),
        Token::Minute => at.minute().to_string(),
        Token::Minute2 => format!("{:02}", at.minute()),
        Token::Second => at.second().to_string(),
        Token::Second2 => format!("{:02}", at.second()),
        Token::MeridiemUpper => locale.meridiem(at.hour(), at.minute(), true),
        Token::MeridiemLower => locale.meridiem(at.hour(), at.minute(), false),
    }
}

/// What the tokens read so far say about the day. Checked only by formatting the
/// day they add up to again, so they need not agree with each other here.
#[derive(Default)]
struct Fields {
    year: Option<i32>,
    month: Option<u32>,
    day: Option<u32>,
    day_of_year: Option<u32>,
    weekday: Option<u32>,
    locale_weekday: Option<u32>,
    iso_weekday: Option<u32>,
    week: Option<u32>,
    week_year: Option<i32>,
    iso_week: Option<u32>,
    iso_week_year: Option<i32>,
    hour: Option<u32>,
    hour12: Option<u32>,
    afternoon: Option<bool>,
    minute: u32,
    second: u32,
}

impl Fields {
    fn day(&self, locale: DateLocale) -> Option<NaiveDate> {
        if let (Some(year), Some(month), Some(day)) = (self.year, self.month, self.day) {
            return NaiveDate::from_ymd_opt(year, month, day);
        }
        if let (Some(year), Some(ordinal)) = (self.year, self.day_of_year) {
            return NaiveDate::from_yo_opt(year, ordinal);
        }
        if let (Some(year), Some(week)) = (self.iso_week_year, self.iso_week) {
            let weekday = match self.iso_weekday {
                Some(day) => {
                    chrono::Weekday::try_from(u8::try_from(day.checked_sub(1)?).ok()?).ok()?
                }
                None => match self.weekday {
                    Some(day) => {
                        chrono::Weekday::try_from(u8::try_from((day + 6) % 7).ok()?).ok()?
                    }
                    None => chrono::Weekday::Mon,
                },
            };
            return NaiveDate::from_isoywd_opt(year, week, weekday);
        }
        if let (Some(year), Some(week)) = (self.week_year, self.week) {
            let (dow, doy) = locale.week_rule();
            let offset = match (self.locale_weekday, self.weekday) {
                (Some(day), _) => day,
                (None, Some(day)) => (day + 7 - dow) % 7,
                (None, None) => 0,
            };
            let start = first_week_start(year, dow, doy)?;
            return start
                .checked_add_days(Days::new(u64::from((week.checked_sub(1)?) * 7 + offset)));
        }
        None
    }

    fn hour(&self) -> Option<u32> {
        match (self.hour, self.hour12, self.afternoon) {
            (Some(hour), _, _) => Some(hour % 24),
            (None, Some(hour), Some(afternoon)) => Some(hour % 12 + if afternoon { 12 } else { 0 }),
            (None, Some(hour), None) => Some(hour % 12),
            (None, None, _) => Some(0),
        }
    }
}

/// Up to `max` digits, at least `min`.
fn digits(text: &str, min: usize, max: usize) -> Option<(u32, &str)> {
    let length = text
        .bytes()
        .take(max)
        .take_while(u8::is_ascii_digit)
        .count();
    if length < min {
        return None;
    }
    Some((text[..length].parse().ok()?, &text[length..]))
}

/// The longest of `names` that `text` starts with, as its index.
fn name<'a>(text: &'a str, names: &[&str]) -> Option<(usize, &'a str)> {
    names
        .iter()
        .enumerate()
        .filter(|(_, name)| text.starts_with(**name))
        .max_by_key(|(_, name)| name.len())
        .map(|(index, name)| (index, &text[name.len()..]))
}

/// A number and the suffix `locale` writes after it.
fn ordinal(text: &str, locale: DateLocale, period: Period) -> Option<(u32, &str)> {
    let (number, rest) = digits(text, 1, 3)?;
    let spelled = locale.ordinal(number, period);
    let suffix = &spelled[number.to_string().len()..];
    Some((number, rest.strip_prefix(suffix)?))
}

fn read_token<'a>(
    token: Token,
    text: &'a str,
    locale: DateLocale,
    fields: &mut Fields,
) -> Option<&'a str> {
    let two_digit_year = |year: u32| (if year > 68 { 1900 } else { 2000 }) + year as i32;
    let rest = match token {
        Token::Year4 => {
            let (year, rest) = digits(text, 4, 4)?;
            fields.year = Some(year as i32);
            rest
        }
        Token::Year2 => {
            let (year, rest) = digits(text, 2, 2)?;
            fields.year = Some(two_digit_year(year));
            rest
        }
        Token::Quarter => digits(text, 1, 1)?.1,
        Token::QuarterOrdinal => ordinal(text, locale, Period::Quarter)?.1,
        Token::Month | Token::Month2 | Token::MonthOrdinal => {
            let (month, rest) = match token {
                Token::Month => digits(text, 1, 2)?,
                Token::Month2 => digits(text, 2, 2)?,
                _ => ordinal(text, locale, Period::Month)?,
            };
            fields.month = Some(month);
            rest
        }
        Token::MonthShort | Token::MonthLong => {
            let names = if token == Token::MonthShort {
                locale.months_short()
            } else {
                locale.months()
            };
            let (index, rest) = name(text, &names)?;
            fields.month = Some(index as u32 + 1);
            rest
        }
        Token::Day | Token::Day2 | Token::DayOrdinal => {
            let (day, rest) = match token {
                Token::Day => digits(text, 1, 2)?,
                Token::Day2 => digits(text, 2, 2)?,
                _ => ordinal(text, locale, Period::Day)?,
            };
            fields.day = Some(day);
            rest
        }
        Token::DayOfYear | Token::DayOfYear3 | Token::DayOfYearOrdinal => {
            let (day, rest) = match token {
                Token::DayOfYear => digits(text, 1, 3)?,
                Token::DayOfYear3 => digits(text, 3, 3)?,
                _ => ordinal(text, locale, Period::Day)?,
            };
            fields.day_of_year = Some(day);
            rest
        }
        Token::Weekday | Token::WeekdayOrdinal => {
            let (day, rest) = if token == Token::Weekday {
                digits(text, 1, 1)?
            } else {
                ordinal(text, locale, Period::Day)?
            };
            fields.weekday = Some(day);
            rest
        }
        Token::WeekdayMin | Token::WeekdayShort | Token::WeekdayLong => {
            let names = match token {
                Token::WeekdayMin => locale.weekdays_min(),
                Token::WeekdayShort => locale.weekdays_short(),
                _ => locale.weekdays(),
            };
            let (index, rest) = name(text, &names)?;
            fields.weekday = Some(index as u32);
            rest
        }
        Token::LocaleWeekday => {
            let (day, rest) = digits(text, 1, 1)?;
            fields.locale_weekday = Some(day);
            rest
        }
        Token::IsoWeekday => {
            let (day, rest) = digits(text, 1, 1)?;
            fields.iso_weekday = Some(day);
            rest
        }
        Token::Week | Token::Week2 | Token::WeekOrdinal => {
            let (week, rest) = match token {
                Token::Week => digits(text, 1, 2)?,
                Token::Week2 => digits(text, 2, 2)?,
                _ => ordinal(text, locale, Period::Week)?,
            };
            fields.week = Some(week);
            rest
        }
        Token::IsoWeek | Token::IsoWeek2 | Token::IsoWeekOrdinal => {
            let (week, rest) = match token {
                Token::IsoWeek => digits(text, 1, 2)?,
                Token::IsoWeek2 => digits(text, 2, 2)?,
                _ => ordinal(text, locale, Period::Week)?,
            };
            fields.iso_week = Some(week);
            rest
        }
        Token::WeekYear4 => {
            let (year, rest) = digits(text, 4, 4)?;
            fields.week_year = Some(year as i32);
            rest
        }
        Token::WeekYear2 => {
            let (year, rest) = digits(text, 2, 2)?;
            fields.week_year = Some(two_digit_year(year));
            rest
        }
        Token::IsoWeekYear4 => {
            let (year, rest) = digits(text, 4, 4)?;
            fields.iso_week_year = Some(year as i32);
            rest
        }
        Token::IsoWeekYear2 => {
            let (year, rest) = digits(text, 2, 2)?;
            fields.iso_week_year = Some(two_digit_year(year));
            rest
        }
        Token::Hour | Token::Hour2 | Token::Hour24 | Token::Hour24Two => {
            let (min, max) = if matches!(token, Token::Hour | Token::Hour24) {
                (1, 2)
            } else {
                (2, 2)
            };
            let (hour, rest) = digits(text, min, max)?;
            fields.hour = Some(hour);
            rest
        }
        Token::Hour12 | Token::Hour12Two => {
            let min = if token == Token::Hour12 { 1 } else { 2 };
            let (hour, rest) = digits(text, min, 2)?;
            fields.hour12 = Some(hour);
            rest
        }
        Token::Minute | Token::Minute2 => {
            let min = if token == Token::Minute { 1 } else { 2 };
            let (minute, rest) = digits(text, min, 2)?;
            fields.minute = minute;
            rest
        }
        Token::Second | Token::Second2 => {
            let min = if token == Token::Second { 1 } else { 2 };
            let (second, rest) = digits(text, min, 2)?;
            fields.second = second;
            rest
        }
        Token::MeridiemUpper | Token::MeridiemLower => {
            let upper = token == Token::MeridiemUpper;
            let (morning, afternoon) =
                (locale.meridiem(0, 0, upper), locale.meridiem(13, 0, upper));
            if locale == DateLocale::English {
                if let Some(rest) = text.strip_prefix(morning.as_str()) {
                    fields.afternoon = Some(false);
                    rest
                } else {
                    fields.afternoon = Some(true);
                    text.strip_prefix(afternoon.as_str())?
                }
            } else {
                // The Chinese periods split the day finer than twelve hours; the
                // formatted round trip settles which hour it was.
                let periods = ["凌晨", "早上", "上午", "中午", "下午", "晚上"];
                let (index, rest) = name(text, &periods)?;
                fields.afternoon = Some(index >= 4);
                rest
            }
        }
    };
    Some(rest)
}

/// The locale's week number of `date` and the year that week belongs to, as
/// Moment.js counts them: weeks start on `dow` and week 1 holds January `doy`'s
/// complement (`7 + dow - doy`).
fn week_of_year(date: NaiveDate, dow: u32, doy: u32) -> (i32, u32) {
    let year = date.year();
    let offset = first_week_offset(year, dow, doy);
    let week = (date.ordinal() as i32 - offset - 1).div_euclid(7) + 1;
    if week < 1 {
        let previous = year - 1;
        (previous, (week + weeks_in_year(previous, dow, doy)) as u32)
    } else if week > weeks_in_year(year, dow, doy) {
        (year + 1, (week - weeks_in_year(year, dow, doy)) as u32)
    } else {
        (year, week as u32)
    }
}

fn first_week_offset(year: i32, dow: u32, doy: u32) -> i32 {
    let fwd = 7 + dow as i32 - doy as i32;
    let january = NaiveDate::from_ymd_opt(year, 1, fwd as u32).expect("a day in January");
    let fwdlw = (7 + january.weekday().num_days_from_sunday() as i32 - dow as i32) % 7;
    -fwdlw + fwd - 1
}

fn weeks_in_year(year: i32, dow: u32, doy: u32) -> i32 {
    let days = if NaiveDate::from_ymd_opt(year, 2, 29).is_some() {
        366
    } else {
        365
    };
    (days - first_week_offset(year, dow, doy) + first_week_offset(year + 1, dow, doy)) / 7
}

/// The first day of week 1 of `year`.
fn first_week_start(year: i32, dow: u32, doy: u32) -> Option<NaiveDate> {
    let offset = first_week_offset(year, dow, doy);
    let january_first = NaiveDate::from_ymd_opt(year, 1, 1)?;
    january_first.checked_add_signed(Duration::days(i64::from(offset)))
}

/// The daily note settings a vault's Obsidian configuration holds, as found under
/// `root/.obsidian`. `template` is spelled as Obsidian keeps it: a link target that
/// may lack the folder and the `.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObsidianDaily {
    pub folder: PathBuf,
    pub format: String,
    pub template: Option<String>,
}

/// Read the daily note settings of the Obsidian vault at `root`, if `root` is one.
/// Obsidian writes only the keys that were changed, so a missing file or key is its
/// default. Periodic Notes, when it keeps daily notes, speaks for them instead. A
/// file that cannot be read gives nothing rather than a guess.
pub fn read_obsidian(root: &Path) -> Option<ObsidianDaily> {
    let config = root.join(".obsidian");
    if !config.is_dir() {
        return None;
    }
    let read = |path: PathBuf| -> Option<Option<serde_json::Value>> {
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(value) => Some(Some(value)),
                Err(error) => {
                    log::warn!("{} could not be read: {error}", path.display());
                    None
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(None),
            Err(error) => {
                log::warn!("{} could not be read: {error}", path.display());
                None
            }
        }
    };
    let periodic = read(config.join("plugins/periodic-notes/data.json"))?
        .and_then(|value| value.get("daily").cloned())
        .filter(|daily| daily.get("enabled").and_then(serde_json::Value::as_bool) == Some(true));
    let settings = match periodic {
        Some(daily) => daily,
        None => read(config.join("daily-notes.json"))?.unwrap_or_default(),
    };
    let text = |key: &str| {
        settings
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_owned()
    };
    let folder = PathBuf::from(text("folder").trim_matches('/'));
    if !folder
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let format = match text("format") {
        format if format.is_empty() => DEFAULT_FORMAT.to_owned(),
        format => format,
    };
    let template = text("template");
    let template = template.trim_start_matches('/');
    let template = template.strip_suffix(".md").unwrap_or(template).to_owned();
    Some(ObsidianDaily {
        folder,
        format,
        template: (!template.is_empty()).then_some(template),
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn at(date: NaiveDate, hour: u32, minute: u32, second: u32) -> NaiveDateTime {
        date.and_hms_opt(hour, minute, second).unwrap()
    }

    fn settings(folder: &str, format: &str) -> DailySettings {
        DailySettings {
            folder: PathBuf::from(folder),
            format: format.to_owned(),
            template: None,
        }
    }

    #[test]
    fn a_day_is_filed_under_its_formatted_name() {
        let english = DateLocale::English;
        assert_eq!(
            DailySettings::default().path_for(day(2026, 9, 28), english),
            PathBuf::from("2026-09-28.md")
        );
        assert_eq!(
            settings("Journal", "YYYY/MMMM/YYYY-MMM-DD").path_for(day(2023, 1, 1), english),
            PathBuf::from("Journal/2023/January/2023-Jan-01.md")
        );
        assert_eq!(
            settings("", "YYYY年MM月DD日 dddd")
                .path_for(day(2026, 9, 28), DateLocale::SimplifiedChinese),
            PathBuf::from("2026年09月28日 星期一.md")
        );
    }

    #[test]
    fn a_path_reads_back_as_its_day() {
        let english = DateLocale::English;
        let nested = settings("Journal", "YYYY/YYYY-MM-DD");
        assert_eq!(
            nested.day_of(Path::new("Journal/2026/2026-09-28.md"), english),
            Some(day(2026, 9, 28))
        );
        // Filed elsewhere under the folder, the name alone still says the day.
        assert_eq!(
            nested.day_of(Path::new("Journal/old/2026-09-27.md"), english),
            Some(day(2026, 9, 27))
        );
        assert_eq!(
            nested.day_of(Path::new("Other/2026/2026-09-28.md"), english),
            None
        );
        assert_eq!(
            nested.day_of(Path::new("Journal/2026/notes.md"), english),
            None
        );
        assert_eq!(
            nested.day_of(Path::new("Journal/2026/2026-09-28.txt"), english),
            None
        );
        // Strict: a day that does not exist, or a spelling the format would not
        // write, is not a daily note.
        let plain = DailySettings::default();
        assert_eq!(plain.day_of(Path::new("2026-02-30.md"), english), None);
        assert_eq!(plain.day_of(Path::new("2026-9-28.md"), english), None);
        assert_eq!(
            plain.day_of(Path::new("2026-09-28 draft.md"), english),
            None
        );
    }

    #[test]
    fn names_written_in_english_read_back_under_another_language() {
        let names = settings("", "YYYY-MM-DD dddd");
        assert_eq!(
            names.day_of(
                Path::new("2026-09-28 Monday.md"),
                DateLocale::SimplifiedChinese
            ),
            Some(day(2026, 9, 28))
        );
        assert_eq!(
            names.day_of(
                Path::new("2026-09-28 星期一.md"),
                DateLocale::SimplifiedChinese
            ),
            Some(day(2026, 9, 28))
        );
    }

    #[test]
    fn tokens_format_as_moment_does() {
        let english = DateLocale::English;
        let moment = at(day(2026, 9, 28), 9, 41, 7);
        let cases = [
            ("YYYY-MM-DD", "2026-09-28"),
            ("YY M D", "26 9 28"),
            ("Do MMMM", "28th September"),
            ("MMM Mo", "Sep 9th"),
            ("dddd ddd dd d do", "Monday Mon Mo 1 1st"),
            ("DDD DDDD DDDo", "271 271 271st"),
            ("Q Qo", "3 3rd"),
            ("W WW Wo GGGG", "40 40 40th 2026"),
            ("w ww gggg e E", "40 40 2026 1 1"),
            ("HH:mm:ss h hh A a k", "09:41:07 9 09 AM am 9"),
            ("[Week] W [of] YYYY", "Week 40 of 2026"),
            ("\\[YYYY", "[2026"),
        ];
        for (format, expected) in cases {
            assert_eq!(
                Pattern::new(format).format(moment, english),
                expected,
                "{format}"
            );
        }
        let chinese = DateLocale::SimplifiedChinese;
        assert_eq!(
            Pattern::new("MMMM Do dddd ddd A wo").format(moment, chinese),
            "九月 28日 星期一 周一 上午 40周"
        );
        assert_eq!(
            Pattern::new("ddd").format(moment, DateLocale::TraditionalChinese),
            "週一"
        );
    }

    #[test]
    fn week_numbers_cross_the_turn_of_the_year() {
        let english = DateLocale::English;
        // 2027-01-01 is a Friday: ISO puts it in 2026's week 53, while a Sunday
        // week holding January 1st makes it week 1.
        let new_year = at(day(2027, 1, 1), 0, 0, 0);
        assert_eq!(
            Pattern::new("GGGG-[W]WW").format(new_year, english),
            "2026-W53"
        );
        assert_eq!(
            Pattern::new("gggg-[w]ww").format(new_year, english),
            "2027-w01"
        );
        assert_eq!(
            Pattern::new("gggg-[w]ww").format(new_year, DateLocale::SimplifiedChinese),
            "2026-w53"
        );
        let settings = settings("", "GGGG-[W]WW-E");
        let path = settings.path_for(day(2027, 1, 1), english);
        assert_eq!(path, PathBuf::from("2026-W53-5.md"));
        assert_eq!(settings.day_of(&path, english), Some(day(2027, 1, 1)));
    }

    #[test]
    fn a_format_must_give_each_day_one_safe_name() {
        let english = DateLocale::English;
        for format in [
            "YYYY-MM-DD",
            "YYYY/MM/YYYY-MM-DD",
            "YYYY/MMMM/YYYY-MMM-DD",
            "dddd, MMMM Do YYYY",
            "GGGG-[W]WW-E",
            "gggg-[w]ww-e",
            "YYYY-DDDD",
            "YY.MM.DD",
        ] {
            assert_eq!(validate_format(format, english), Ok(()), "{format}");
        }
        assert_eq!(
            validate_format("YYYY年MM月DD日 dddd", DateLocale::SimplifiedChinese),
            Ok(())
        );
        assert_eq!(validate_format("  ", english), Err(FormatProblem::Empty));
        assert_eq!(
            validate_format("YYYY-MM", english),
            Err(FormatProblem::NotOneDay)
        );
        assert_eq!(
            validate_format("MM-DD", english),
            Err(FormatProblem::NotOneDay)
        );
        assert_eq!(
            validate_format("YYYYMD", english),
            Err(FormatProblem::NotOneDay)
        );
        assert_eq!(
            validate_format("HH:mm", english),
            Err(FormatProblem::UnsafeName)
        );
        assert_eq!(
            validate_format("YYYY/[.]MM-DD", english),
            Err(FormatProblem::UnsafeName)
        );
        assert_eq!(
            validate_format("YYYY//MM-DD", english),
            Err(FormatProblem::UnsafeName)
        );
        assert_eq!(
            validate_format("[#]YYYY-MM-DD", english),
            Err(FormatProblem::UnsafeName)
        );
    }

    #[test]
    fn template_placeholders_fill_in_the_notes_day() {
        let english = DateLocale::English;
        let settings = settings("Journal", "YYYY/YYYY-MM-DD");
        // Written on the 28th at 09:41 for the 27th.
        let now = at(day(2026, 9, 28), 9, 41, 7);
        let template = "---\ndate: {{date}}\n---\n# {{date:dddd, MMMM D}}\n\
            [[{{yesterday}}]] · [[{{tomorrow}}]] · {{title}}\n\
            {{time}} {{time:HH:mm:ss}} {{ DATE }} {{date+7d:YYYY-MM-DD}} {{date-1M:MMMM}}\n\
            {{unknown}} {{date+1x}} {{ open";
        assert_eq!(
            settings.expand(template, day(2026, 9, 27), now, english),
            "---\ndate: 2026-09-27\n---\n# Sunday, September 27\n\
            [[2026-09-26]] · [[2026-09-28]] · 2026-09-27\n\
            09:41 09:41:07 2026-09-27 2026-10-04 August\n\
            {{unknown}} {{date+1x}} {{ open"
        );
        // `m` is a minute and `M` a month, as in Moment.js.
        assert_eq!(
            settings.expand("{{time+5m}} {{date+1M}}", day(2026, 9, 27), now, english),
            "09:46 2026-10-27"
        );
    }

    #[test]
    fn obsidian_settings_fill_in_what_the_vault_left_out() {
        let vault = tempfile::tempdir().unwrap();
        assert_eq!(read_obsidian(vault.path()), None);
        let config = vault.path().join(".obsidian");
        std::fs::create_dir(&config).unwrap();
        let defaults = ObsidianDaily {
            folder: PathBuf::new(),
            format: DEFAULT_FORMAT.to_owned(),
            template: None,
        };
        assert_eq!(read_obsidian(vault.path()), Some(defaults.clone()));
        std::fs::write(config.join("daily-notes.json"), r#"{"folder": ""}"#).unwrap();
        assert_eq!(read_obsidian(vault.path()), Some(defaults));
        std::fs::write(
            config.join("daily-notes.json"),
            r#"{"folder": "/Journal/", "format": "YYYY/YYYY-MM-DD", "template": "Templates/Daily.md", "autorun": true}"#,
        )
        .unwrap();
        let journal = ObsidianDaily {
            folder: PathBuf::from("Journal"),
            format: "YYYY/YYYY-MM-DD".to_owned(),
            template: Some("Templates/Daily".to_owned()),
        };
        assert_eq!(read_obsidian(vault.path()), Some(journal.clone()));
        // Periodic Notes speaks for daily notes only while it keeps them.
        let plugin = config.join("plugins/periodic-notes");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("data.json"),
            r#"{"daily": {"enabled": false, "folder": "Days", "format": "DD-MM-YYYY"}}"#,
        )
        .unwrap();
        assert_eq!(read_obsidian(vault.path()), Some(journal));
        std::fs::write(
            plugin.join("data.json"),
            r#"{"daily": {"enabled": true, "folder": "Days", "format": "DD-MM-YYYY", "template": ""}}"#,
        )
        .unwrap();
        assert_eq!(
            read_obsidian(vault.path()),
            Some(ObsidianDaily {
                folder: PathBuf::from("Days"),
                format: "DD-MM-YYYY".to_owned(),
                template: None,
            })
        );
        // Unreadable, or pointing outside the vault: nothing to offer.
        std::fs::write(plugin.join("data.json"), "{not json").unwrap();
        assert_eq!(read_obsidian(vault.path()), None);
        std::fs::remove_file(plugin.join("data.json")).unwrap();
        std::fs::write(
            config.join("daily-notes.json"),
            r#"{"folder": "../elsewhere"}"#,
        )
        .unwrap();
        assert_eq!(read_obsidian(vault.path()), None);
    }
}
