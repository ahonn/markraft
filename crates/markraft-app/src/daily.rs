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
    German,
    Spanish,
    French,
    Japanese,
    Korean,
    BrazilianPortuguese,
    SimplifiedChinese,
    TraditionalChinese,
}

impl DateLocale {
    /// The languages names can be spelled in, for matching against the system's.
    /// English is first: it is what any other language falls back to.
    pub const LANGUAGES: [&'static str; 9] = [
        "en", "de", "es", "fr", "ja", "ko", "pt-BR", "zh-Hans", "zh-Hant",
    ];

    /// From a BCP 47 language as the system reports it.
    pub fn from_language(language: &str) -> Self {
        let lower = language.to_ascii_lowercase();
        match lower.split('-').next().unwrap_or_default() {
            "de" => DateLocale::German,
            "es" => DateLocale::Spanish,
            "fr" => DateLocale::French,
            "ja" => DateLocale::Japanese,
            "ko" => DateLocale::Korean,
            "pt" => DateLocale::BrazilianPortuguese,
            "zh" if ["zh-hant", "zh-tw", "zh-hk", "zh-mo"]
                .iter()
                .any(|traditional| lower.starts_with(traditional)) =>
            {
                DateLocale::TraditionalChinese
            }
            "zh" => DateLocale::SimplifiedChinese,
            _ => DateLocale::English,
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
            DateLocale::German => [
                "Januar",
                "Februar",
                "März",
                "April",
                "Mai",
                "Juni",
                "Juli",
                "August",
                "September",
                "Oktober",
                "November",
                "Dezember",
            ],
            DateLocale::Spanish => [
                "enero",
                "febrero",
                "marzo",
                "abril",
                "mayo",
                "junio",
                "julio",
                "agosto",
                "septiembre",
                "octubre",
                "noviembre",
                "diciembre",
            ],
            DateLocale::French => [
                "janvier",
                "février",
                "mars",
                "avril",
                "mai",
                "juin",
                "juillet",
                "août",
                "septembre",
                "octobre",
                "novembre",
                "décembre",
            ],
            DateLocale::BrazilianPortuguese => [
                "janeiro",
                "fevereiro",
                "março",
                "abril",
                "maio",
                "junho",
                "julho",
                "agosto",
                "setembro",
                "outubro",
                "novembro",
                "dezembro",
            ],
            DateLocale::Japanese | DateLocale::Korean => self.months_short(false),
            DateLocale::SimplifiedChinese | DateLocale::TraditionalChinese => [
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

    /// `in_dashes` is whether the format holds `-MMM-`, where Spanish drops the
    /// period it otherwise ends a short month with.
    fn months_short(self, in_dashes: bool) -> [&'static str; 12] {
        match self {
            DateLocale::English => [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ],
            DateLocale::German => [
                "Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sep.", "Okt.",
                "Nov.", "Dez.",
            ],
            DateLocale::Spanish if in_dashes => [
                "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic",
            ],
            DateLocale::Spanish => [
                "ene.", "feb.", "mar.", "abr.", "may.", "jun.", "jul.", "ago.", "sep.", "oct.",
                "nov.", "dic.",
            ],
            DateLocale::French => [
                "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.",
                "nov.", "déc.",
            ],
            DateLocale::BrazilianPortuguese => [
                "jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez",
            ],
            DateLocale::Korean => [
                "1월", "2월", "3월", "4월", "5월", "6월", "7월", "8월", "9월", "10월", "11월",
                "12월",
            ],
            DateLocale::Japanese
            | DateLocale::SimplifiedChinese
            | DateLocale::TraditionalChinese => [
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
            DateLocale::German => [
                "Sonntag",
                "Montag",
                "Dienstag",
                "Mittwoch",
                "Donnerstag",
                "Freitag",
                "Samstag",
            ],
            DateLocale::Spanish => [
                "domingo",
                "lunes",
                "martes",
                "miércoles",
                "jueves",
                "viernes",
                "sábado",
            ],
            DateLocale::French => [
                "dimanche", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi",
            ],
            DateLocale::Japanese => [
                "日曜日",
                "月曜日",
                "火曜日",
                "水曜日",
                "木曜日",
                "金曜日",
                "土曜日",
            ],
            DateLocale::Korean => [
                "일요일",
                "월요일",
                "화요일",
                "수요일",
                "목요일",
                "금요일",
                "토요일",
            ],
            DateLocale::BrazilianPortuguese => [
                "domingo",
                "segunda-feira",
                "terça-feira",
                "quarta-feira",
                "quinta-feira",
                "sexta-feira",
                "sábado",
            ],
            DateLocale::SimplifiedChinese | DateLocale::TraditionalChinese => [
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
            DateLocale::German => ["So.", "Mo.", "Di.", "Mi.", "Do.", "Fr.", "Sa."],
            DateLocale::Spanish => ["dom.", "lun.", "mar.", "mié.", "jue.", "vie.", "sáb."],
            DateLocale::French => ["dim.", "lun.", "mar.", "mer.", "jeu.", "ven.", "sam."],
            DateLocale::BrazilianPortuguese => ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"],
            DateLocale::Japanese | DateLocale::Korean => self.weekdays_min(),
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
            DateLocale::German => ["So", "Mo", "Di", "Mi", "Do", "Fr", "Sa"],
            DateLocale::Spanish => ["do", "lu", "ma", "mi", "ju", "vi", "sá"],
            DateLocale::French => ["di", "lu", "ma", "me", "je", "ve", "sa"],
            DateLocale::BrazilianPortuguese => ["do", "2ª", "3ª", "4ª", "5ª", "6ª", "sá"],
            DateLocale::Japanese => ["日", "月", "火", "水", "木", "金", "土"],
            DateLocale::Korean => ["일", "월", "화", "수", "목", "금", "토"],
            DateLocale::SimplifiedChinese | DateLocale::TraditionalChinese => {
                ["日", "一", "二", "三", "四", "五", "六"]
            }
        }
    }

    /// The first day of the week (0 is Sunday) and Moment.js's `doy`: week 1 always
    /// holds January `7 + dow - doy`.
    fn week_rule(self) -> (u32, u32) {
        match self {
            DateLocale::German
            | DateLocale::Spanish
            | DateLocale::French
            | DateLocale::SimplifiedChinese => (1, 4),
            DateLocale::English
            | DateLocale::Japanese
            | DateLocale::Korean
            | DateLocale::BrazilianPortuguese
            | DateLocale::TraditionalChinese => (0, 6),
        }
    }

    /// The names this language splits the day into, in order, each with whether it
    /// falls after noon.
    fn day_periods(self, upper: bool) -> &'static [(&'static str, bool)] {
        match self {
            DateLocale::Japanese => &[("午前", false), ("午後", true)],
            DateLocale::Korean => &[("오전", false), ("오후", true)],
            DateLocale::SimplifiedChinese | DateLocale::TraditionalChinese => &[
                ("凌晨", false),
                ("早上", false),
                ("上午", false),
                ("中午", false),
                ("下午", true),
                ("晚上", true),
            ],
            _ if upper => &[("AM", false), ("PM", true)],
            _ => &[("am", false), ("pm", true)],
        }
    }

    fn meridiem(self, hour: u32, minute: u32, upper: bool) -> &'static str {
        let period = match self {
            // The Chinese periods split the day finer than twelve hours.
            DateLocale::SimplifiedChinese | DateLocale::TraditionalChinese => {
                match hour * 100 + minute {
                    0..600 => 0,
                    600..900 => 1,
                    900..1130 => 2,
                    1130..1230 => 3,
                    1230..1800 => 4,
                    _ => 5,
                }
            }
            _ => usize::from(hour >= 12),
        };
        self.day_periods(upper)[period].0
    }

    fn ordinal(self, number: u32, period: Period) -> String {
        let first = number == 1;
        let of_a_day = matches!(period, Period::Day | Period::DayOfYear | Period::Weekday);
        let suffix = match self {
            DateLocale::English => match (number % 100, number % 10) {
                (11..=13, _) => "th",
                (_, 1) => "st",
                (_, 2) => "nd",
                (_, 3) => "rd",
                _ => "th",
            },
            DateLocale::German => ".",
            DateLocale::Spanish | DateLocale::BrazilianPortuguese => "º",
            DateLocale::French => match period {
                Period::Day if first => "er",
                Period::Day => "",
                Period::Week if first => "re",
                _ if first => "er",
                _ => "e",
            },
            DateLocale::Japanese if of_a_day => "日",
            DateLocale::Japanese => "",
            DateLocale::Korean => match period {
                _ if of_a_day => "일",
                Period::Month => "월",
                Period::Week => "주",
                _ => "",
            },
            DateLocale::SimplifiedChinese | DateLocale::TraditionalChinese => match period {
                _ if of_a_day => "日",
                Period::Month => "月",
                Period::Week if self == DateLocale::SimplifiedChinese => "周",
                Period::Week => "週",
                _ => "",
            },
        };
        format!("{number}{suffix}")
    }
}

/// What an ordinal counts; some languages end each differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Period {
    Day,
    DayOfYear,
    Weekday,
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
    /// `MMM` in a format that holds `-MMM-`.
    MonthShortInDashes,
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
        // Moment.js hands a locale the whole format, not the token's surroundings.
        if format.contains("-MMM-") {
            for piece in &mut pieces {
                if *piece == Piece::Token(Token::MonthShort) {
                    *piece = Piece::Token(Token::MonthShortInDashes);
                }
            }
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
        Token::MonthShort | Token::MonthShortInDashes => locale
            .months_short(token == Token::MonthShortInDashes)[date.month0() as usize]
            .to_owned(),
        Token::MonthLong => locale.months()[date.month0() as usize].to_owned(),
        Token::Day => date.day().to_string(),
        Token::Day2 => format!("{:02}", date.day()),
        Token::DayOrdinal => locale.ordinal(date.day(), Period::Day),
        Token::DayOfYear => date.ordinal().to_string(),
        Token::DayOfYear3 => format!("{:03}", date.ordinal()),
        Token::DayOfYearOrdinal => locale.ordinal(date.ordinal(), Period::DayOfYear),
        Token::Weekday => weekday.to_string(),
        Token::WeekdayOrdinal => locale.ordinal(weekday, Period::Weekday),
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
        Token::MeridiemUpper => locale.meridiem(at.hour(), at.minute(), true).to_owned(),
        Token::MeridiemLower => locale.meridiem(at.hour(), at.minute(), false).to_owned(),
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
        Token::MonthShort | Token::MonthShortInDashes | Token::MonthLong => {
            let names = match token {
                Token::MonthLong => locale.months(),
                _ => locale.months_short(token == Token::MonthShortInDashes),
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
                _ => ordinal(text, locale, Period::DayOfYear)?,
            };
            fields.day_of_year = Some(day);
            rest
        }
        Token::Weekday | Token::WeekdayOrdinal => {
            let (day, rest) = if token == Token::Weekday {
                digits(text, 1, 1)?
            } else {
                ordinal(text, locale, Period::Weekday)?
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
            // Where a language splits the day finer than twelve hours, the
            // formatted round trip settles which hour it was.
            let periods = locale.day_periods(token == Token::MeridiemUpper);
            let names: Vec<_> = periods.iter().map(|(name, _)| *name).collect();
            let (index, rest) = name(text, &names)?;
            fields.afternoon = Some(periods[index].1);
            rest
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
    fn every_language_spells_names_as_moment_does() {
        let autumn = at(day(2026, 9, 28), 9, 41, 7);
        let spring = at(day(2026, 3, 1), 15, 5, 0);
        let formats = [
            "MMMM MMM Mo",
            "dddd ddd dd do",
            "Do DDDo Qo",
            "wo Wo w e",
            "h A a",
        ];
        // What Moment.js 2.30.1 writes for `formats` at each of the two moments.
        let cases = [
            (
                DateLocale::German,
                [
                    "September Sep. 9.",
                    "Montag Mo. Mo 1.",
                    "28. 271. 3.",
                    "40. 40. 40 0",
                    "9 AM am",
                ],
                [
                    "März März 3.",
                    "Sonntag So. So 0.",
                    "1. 60. 1.",
                    "9. 9. 9 6",
                    "3 PM pm",
                ],
            ),
            (
                DateLocale::Spanish,
                [
                    "septiembre sep. 9º",
                    "lunes lun. lu 1º",
                    "28º 271º 3º",
                    "40º 40º 40 0",
                    "9 AM am",
                ],
                [
                    "marzo mar. 3º",
                    "domingo dom. do 0º",
                    "1º 60º 1º",
                    "9º 9º 9 6",
                    "3 PM pm",
                ],
            ),
            (
                DateLocale::French,
                [
                    "septembre sept. 9e",
                    "lundi lun. lu 1er",
                    "28 271e 3e",
                    "40e 40e 40 0",
                    "9 AM am",
                ],
                [
                    "mars mars 3e",
                    "dimanche dim. di 0e",
                    "1er 60e 1er",
                    "9e 9e 9 6",
                    "3 PM pm",
                ],
            ),
            (
                DateLocale::Japanese,
                [
                    "9月 9月 9",
                    "月曜日 月 月 1日",
                    "28日 271日 3",
                    "40 40 40 1",
                    "9 午前 午前",
                ],
                [
                    "3月 3月 3",
                    "日曜日 日 日 0日",
                    "1日 60日 1",
                    "10 9 10 0",
                    "3 午後 午後",
                ],
            ),
            (
                DateLocale::Korean,
                [
                    "9월 9월 9월",
                    "월요일 월 월 1일",
                    "28일 271일 3",
                    "40주 40주 40 1",
                    "9 오전 오전",
                ],
                [
                    "3월 3월 3월",
                    "일요일 일 일 0일",
                    "1일 60일 1",
                    "10주 9주 10 0",
                    "3 오후 오후",
                ],
            ),
            (
                DateLocale::BrazilianPortuguese,
                [
                    "setembro set 9º",
                    "segunda-feira seg 2ª 1º",
                    "28º 271º 3º",
                    "40º 40º 40 1",
                    "9 AM am",
                ],
                [
                    "março mar 3º",
                    "domingo dom do 0º",
                    "1º 60º 1º",
                    "10º 9º 10 0",
                    "3 PM pm",
                ],
            ),
        ];
        for (locale, in_autumn, in_spring) in cases {
            for (moment, expected) in [(autumn, in_autumn), (spring, in_spring)] {
                for (format, expected) in formats.iter().zip(expected) {
                    assert_eq!(
                        Pattern::new(format).format(moment, locale),
                        expected,
                        "{locale:?} {format}"
                    );
                }
            }
        }
        // A short month between dashes loses its period in Spanish only.
        for (locale, dashed, spaced) in [
            (DateLocale::Spanish, "2026-sep-28", "28 sep. 2026"),
            (DateLocale::German, "2026-Sep.-28", "28 Sep. 2026"),
            (DateLocale::French, "2026-sept.-28", "28 sept. 2026"),
            (
                DateLocale::BrazilianPortuguese,
                "2026-set-28",
                "28 set 2026",
            ),
            (DateLocale::Japanese, "2026-9月-28", "28 9月 2026"),
        ] {
            assert_eq!(
                Pattern::new("YYYY-MMM-DD").format(autumn, locale),
                dashed,
                "{locale:?}"
            );
            assert_eq!(
                Pattern::new("DD MMM YYYY").format(autumn, locale),
                spaced,
                "{locale:?}"
            );
        }
    }

    #[test]
    fn every_language_reads_back_the_names_it_writes() {
        for language in DateLocale::LANGUAGES {
            let locale = DateLocale::from_language(language);
            for format in [
                "YYYY-MM-DD dddd",
                "YYYY/MMMM/YYYY-MMM-DD",
                "YYYY MMM D ddd",
                "dddd, MMMM Do YYYY",
                "YYYY DDDo",
                "gggg-[w]wo-e",
                "GGGG-Wo-E",
                "YYYY-MM-DD hh A",
            ] {
                assert_eq!(
                    validate_format(format, locale),
                    Ok(()),
                    "{language} {format}"
                );
            }
        }
        let french = settings("", "dddd Do MMMM YYYY");
        assert_eq!(
            french.path_for(day(2026, 3, 1), DateLocale::French),
            PathBuf::from("dimanche 1er mars 2026.md")
        );
        for name in ["dimanche 1er mars 2026.md", "Sunday 1st March 2026.md"] {
            assert_eq!(
                french.day_of(Path::new(name), DateLocale::French),
                Some(day(2026, 3, 1)),
                "{name}"
            );
        }
        // A name the language would not write is not one of its days.
        assert_eq!(
            french.day_of(Path::new("dimanche 1 mars 2026.md"), DateLocale::French),
            None
        );
    }

    #[test]
    fn a_system_language_picks_its_date_language() {
        for (language, expected) in [
            ("en-AU", DateLocale::English),
            ("de", DateLocale::German),
            ("de-AT", DateLocale::German),
            ("es-MX", DateLocale::Spanish),
            ("fr-CA", DateLocale::French),
            ("ja", DateLocale::Japanese),
            ("ko-KR", DateLocale::Korean),
            ("pt-BR", DateLocale::BrazilianPortuguese),
            ("zh-Hans", DateLocale::SimplifiedChinese),
            ("zh-Hant-HK", DateLocale::TraditionalChinese),
            ("zh-TW", DateLocale::TraditionalChinese),
            ("it", DateLocale::English),
        ] {
            assert_eq!(DateLocale::from_language(language), expected, "{language}");
        }
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
        for (locale, expected) in [
            (DateLocale::German, "2026-w53"),
            (DateLocale::Spanish, "2026-w53"),
            (DateLocale::French, "2026-w53"),
            (DateLocale::Japanese, "2027-w01"),
            (DateLocale::Korean, "2027-w01"),
            (DateLocale::BrazilianPortuguese, "2027-w01"),
        ] {
            assert_eq!(
                Pattern::new("gggg-[w]ww").format(new_year, locale),
                expected,
                "{locale:?}"
            );
        }
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
