//! How exported HTML looks: the editor's palette, and the one place that
//! decides whether an element is styled by a class or by inline style.
//!
//! Rules name what an element is ([`Role`]); [`Styling`] turns that into
//! attributes for the reader at hand. A page carries a stylesheet, so classes
//! do; pasted rich text keeps no stylesheet, so the same roles become inline
//! styles.

use markraft_gpui::{CalloutTone, EditorStyle};
use markraft_syntax::Tone as CodeTone;

/// The editor's colours, as CSS values.
#[derive(Clone, Debug)]
pub(super) struct Palette {
    background: String,
    text: String,
    muted: String,
    link: String,
    code_background: String,
    inline_code_background: String,
    inline_code_text: String,
    highlight: String,
    rule: String,
    table_header: String,
    callouts: Vec<(CalloutTone, String)>,
    dark: bool,
    /// The body text colour as red, green, blue and alpha, for formulas drawn
    /// as pictures.
    pub(super) ink: [f32; 4],
}

impl Palette {
    pub(super) fn light() -> Palette {
        Palette::of(&EditorStyle::notes(), false)
    }

    pub(super) fn dark() -> Palette {
        Palette::of(&EditorStyle::notes_dark(), true)
    }

    fn of(style: &EditorStyle, dark: bool) -> Palette {
        let ink = gpui::Rgba::from(style.text);
        Palette {
            background: css_color(style.background),
            text: css_color(style.text),
            muted: css_color(style.muted_text),
            link: css_color(style.link),
            code_background: css_color(style.code_background),
            inline_code_background: css_color(style.inline_code_background),
            inline_code_text: css_color(style.inline_code_text),
            highlight: css_color(style.highlight),
            rule: css_color(style.rule),
            table_header: css_color(style.table_header_background),
            callouts: CalloutTone::ALL
                .iter()
                .map(|tone| (*tone, css_color(style.callout_tone(*tone))))
                .collect(),
            dark,
            ink: [ink.r, ink.g, ink.b, ink.a],
        }
    }

    fn callout(&self, tone: CalloutTone) -> &str {
        self.callouts
            .iter()
            .find(|(each, _)| *each == tone)
            .map_or(&self.text, |(_, color)| color)
    }

    fn code(&self, tone: CodeTone) -> String {
        format!("#{:06x}", tone.rgb(self.dark))
    }

    /// The palette as the custom properties the page stylesheet reads.
    fn custom_properties(&self) -> String {
        let mut out = String::new();
        let mut var = |name: &str, value: &str| out.push_str(&format!("  --{name}: {value};\n"));
        var("background", &self.background);
        var("text", &self.text);
        var("muted", &self.muted);
        var("link", &self.link);
        var("code-background", &self.code_background);
        var("inline-code-background", &self.inline_code_background);
        var("inline-code-text", &self.inline_code_text);
        var("highlight", &self.highlight);
        var("rule", &self.rule);
        var("table-header", &self.table_header);
        for (tone, color) in &self.callouts {
            var(&format!("callout-{}", tone.name()), color);
        }
        for tone in CodeTone::ALL {
            var(&format!("code-{}", tone.name()), &self.code(tone));
        }
        out
    }
}

/// `#rrggbb`, or `rgba(…)` for a colour that is not opaque.
fn css_color(color: gpui::Hsla) -> String {
    let rgba = gpui::Rgba::from(color);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    if rgba.a >= 1. {
        format!(
            "#{:02x}{:02x}{:02x}",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b)
        )
    } else {
        format!(
            "rgba({}, {}, {}, {:.3})",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b),
            rgba.a
        )
    }
}

/// What an element is, as far as its look goes.
#[derive(Clone, Copy, Debug)]
pub(super) enum Role {
    CodeBlock,
    /// A run of highlighted code.
    Code(CodeTone),
    Callout(CalloutTone),
    CalloutTitle(CalloutTone),
    Table,
    TableCell,
    TableHeader,
    /// A task item, done or not.
    Task {
        done: bool,
    },
    WikiLink,
    Formula,
    DisplayFormula,
    EquationTag,
    Footnote,
    FootnoteLabel,
    /// An HTML block shown as its source.
    RawSource,
}

/// How a reader takes styling.
#[derive(Clone, Debug)]
pub(super) enum Styling {
    /// Classes the page's stylesheet styles.
    Classes,
    /// Inline styles in this palette, for a reader that keeps no stylesheet.
    Inline(Box<Palette>),
}

impl Styling {
    /// The attributes `role` adds to its element, with a leading space, or an
    /// empty string where this reader takes none.
    pub(super) fn attrs(&self, role: Role) -> String {
        match self {
            Styling::Classes => {
                class(role).map_or_else(String::new, |class| format!(" class=\"{class}\""))
            }
            Styling::Inline(palette) => inline(palette, role)
                .map_or_else(String::new, |style| format!(" style=\"{style}\"")),
        }
    }

    /// Whether a role that exists only to be styled, such as a run of
    /// highlighted code, needs an element at all.
    pub(super) fn wraps(&self, role: Role) -> bool {
        !self.attrs(role).is_empty()
    }
}

fn class(role: Role) -> Option<String> {
    Some(match role {
        Role::Code(CodeTone::Text) => return None,
        Role::Code(tone) => format!("code-{}", tone.name()),
        Role::Callout(tone) => format!("callout callout-{}", tone.name()),
        Role::CalloutTitle(_) => "callout-title".into(),
        Role::Task { done: true } => "task done".into(),
        Role::Task { done: false } => "task".into(),
        Role::WikiLink => "wiki-link".into(),
        Role::Formula => "math".into(),
        Role::DisplayFormula => "math display".into(),
        Role::EquationTag => "math-tag".into(),
        Role::Footnote => "footnote".into(),
        Role::FootnoteLabel => "footnote-label".into(),
        Role::RawSource => "raw".into(),
        // The stylesheet styles these by their tags.
        Role::CodeBlock | Role::Table | Role::TableCell | Role::TableHeader => return None,
    })
}

fn inline(palette: &Palette, role: Role) -> Option<String> {
    Some(match role {
        Role::CodeBlock => format!(
            "background:{};padding:10px 14px;border-radius:6px;\
             font-family:ui-monospace,Menlo,monospace;font-size:13px;line-height:1.45",
            palette.code_background
        ),
        Role::Code(CodeTone::Text) => return None,
        Role::Code(tone) => {
            let italic = if tone.italic() {
                ";font-style:italic"
            } else {
                ""
            };
            format!("color:{}{italic}", palette.code(tone))
        }
        Role::Callout(tone) => format!(
            "margin:0 0 12px;padding-left:12px;border-left:3px solid {}",
            palette.callout(tone)
        ),
        Role::CalloutTitle(tone) => format!("color:{};font-weight:600", palette.callout(tone)),
        Role::Table => "border-collapse:collapse".into(),
        Role::TableCell => format!("border:1px solid {};padding:4px 10px", palette.rule),
        Role::TableHeader => format!(
            "border:1px solid {};padding:4px 10px;background:{}",
            palette.rule, palette.table_header
        ),
        Role::Task { done: true } => format!(
            "list-style:none;color:{};text-decoration:line-through",
            palette.muted
        ),
        Role::Task { done: false } => "list-style:none".into(),
        Role::WikiLink => format!("color:{}", palette.link),
        Role::DisplayFormula => "display:block;text-align:center".into(),
        Role::Formula | Role::EquationTag => return None,
        Role::Footnote => format!("font-size:0.9em;color:{}", palette.muted),
        Role::FootnoteLabel => "font-weight:600;margin-right:0.4em".into(),
        Role::RawSource => format!("color:{}", palette.muted),
    })
}

/// The page stylesheet: the light palette, and the dark one under the reader's
/// dark appearance unless the page is for paper, which is always light and keeps
/// blocks whole across pages.
pub(super) fn stylesheet(paper: bool) -> String {
    let mut css = format!(":root {{\n{}}}\n", Palette::light().custom_properties());
    if !paper {
        css.push_str(&format!(
            "@media (prefers-color-scheme: dark) {{\n:root {{\n{}}}\n}}\n",
            Palette::dark().custom_properties()
        ));
    }
    css.push_str(LAYOUT);
    if paper {
        css.push_str(PRINT);
    }
    for tone in CalloutTone::ALL {
        let name = tone.name();
        css.push_str(&format!(
            ".callout-{name} {{ --callout: var(--callout-{name}); }}\n"
        ));
    }
    for tone in CodeTone::ALL {
        let name = tone.name();
        css.push_str(&format!(".code-{name} {{ color: var(--code-{name}); }}\n"));
    }
    css.push_str(".code-comment { font-style: italic; }\n");
    css
}

const LAYOUT: &str = r#"* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body {
  margin: 0;
  background: var(--background);
  color: var(--text);
  font: 16px/1.6 -apple-system, BlinkMacSystemFont, "Helvetica Neue", "PingFang SC",
    "Hiragino Sans", "Apple SD Gothic Neo", sans-serif;
  overflow-wrap: break-word;
}
.note { max-width: 46rem; margin: 0 auto; padding: 3rem 1.5rem; }
.note > :first-child { margin-top: 0; }
h1, h2, h3, h4, h5, h6 { margin: 1.6em 0 0.5em; line-height: 1.3; font-weight: 600; }
h1 { font-size: 1.85em; }
h2 { font-size: 1.5em; }
h3 { font-size: 1.28em; }
h4 { font-size: 1.14em; }
h5 { font-size: 1.07em; }
h6 { font-size: 1em; }
p, ul, ol, pre, table, blockquote, .footnote { margin: 0 0 0.75em; }
ul, ol { padding-left: 1.6em; }
li > p { margin: 0; }
li > ul, li > ol { margin: 0.25em 0 0; }
li + li { margin-top: 0.25em; }
a, .wiki-link { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }
code {
  font: 0.88em ui-monospace, SFMono-Regular, Menlo, monospace;
  background: var(--inline-code-background);
  color: var(--inline-code-text);
  padding: 0.12em 0.35em;
  border-radius: 4px;
}
pre {
  background: var(--code-background);
  padding: 0.8em 1em;
  border-radius: 6px;
  overflow-x: auto;
  line-height: 1.45;
}
pre code { background: none; padding: 0; color: var(--text); font-size: 0.85em; }
mark { background: var(--highlight); color: inherit; }
hr { border: 0; border-top: 1px solid var(--rule); margin: 1.5em 0; }
blockquote { margin-left: 0; padding-left: 1em; border-left: 3px solid var(--rule); color: var(--muted); }
blockquote > :last-child { margin-bottom: 0; }
aside.callout { margin: 0 0 0.75em; padding-left: 1em; border-left: 3px solid var(--callout); }
aside.callout > :last-child { margin-bottom: 0; }
.callout-title { color: var(--callout); font-weight: 600; margin-bottom: 0.25em; }
table { border-collapse: collapse; display: block; max-width: 100%; overflow-x: auto; }
th, td { border: 1px solid var(--rule); padding: 0.3em 0.75em; }
th { background: var(--table-header); }
img { max-width: 100%; height: auto; }
li.task { list-style: none; }
li.task > input { margin: 0 0.45em 0 -1.35em; vertical-align: -0.1em; }
li.task.done { color: var(--muted); text-decoration: line-through; }
.math.display { display: block; position: relative; text-align: center; margin: 0.5em 0; }
.math-tag { position: absolute; right: 0; top: 50%; transform: translateY(-50%); }
.footnote { font-size: 0.9em; color: var(--muted); }
.footnote-label { font-weight: 600; margin-right: 0.4em; }
.raw { color: var(--muted); }
"#;

const PRINT: &str = r#"body { background: #fff; }
.note { max-width: none; padding: 0; }
* { -webkit-print-color-adjust: exact; print-color-adjust: exact; }
pre, table, img, blockquote, aside.callout, .math.display { break-inside: avoid; }
h1, h2, h3, h4, h5, h6 { break-after: avoid; }
pre { white-space: pre-wrap; }
"#;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn a_role_uses_a_class_or_inline_style() {
        let role = Role::Task { done: true };
        assert_eq!(Styling::Classes.attrs(role), " class=\"task done\"");
        let inline = Styling::Inline(Box::new(Palette::light())).attrs(role);
        assert!(
            inline.starts_with(" style=\"list-style:none;color:#"),
            "{inline}"
        );
        assert!(!Styling::Classes.wraps(Role::Code(CodeTone::Text)));
        assert!(Styling::Classes.wraps(Role::Code(CodeTone::Keyword)));
    }

    #[test]
    fn inline_colours_are_the_editors_own() {
        let palette = Palette::light();
        let style = EditorStyle::notes();
        let pre = Styling::Inline(Box::new(palette.clone())).attrs(Role::CodeBlock);
        assert!(pre.contains(&css_color(style.code_background)), "{pre}");
        let cell = Styling::Inline(Box::new(palette)).attrs(Role::TableCell);
        assert!(cell.contains(&css_color(style.rule)), "{cell}");
    }

    #[test]
    fn paper_is_light_only() {
        assert!(stylesheet(false).contains("prefers-color-scheme: dark"));
        let paper = stylesheet(true);
        assert!(!paper.contains("prefers-color-scheme"));
        assert!(paper.contains("break-inside: avoid"));
    }
}
