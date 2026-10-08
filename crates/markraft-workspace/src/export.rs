//! A note written out for reading elsewhere: a web page, a page to print, rich
//! text for the clipboard.
//!
//! Every form starts from the committed document, never from the view's screen
//! caches. Formulas are typeset again and pictures are read from disk, so an
//! export shows what the file says, including parts scrolled out of view.
//!
//! A [`Profile`] names a reader; what the rules act on is the [`Target`] it
//! stands for: what that reader can show. A new reader is a new set of
//! capabilities, not a new case in every rule.

mod html;
mod media;
mod style;

use crate::locale::I18n;
use markraft_core::Node;
use markraft_core::kind::equations::EquationIndex;
use markraft_core::projection::Projection;
use media::FormulaStyle;
use std::path::PathBuf;
use style::{Palette, Styling};

/// Who reads the export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// A standalone web page with its own stylesheet, following the reader's
    /// light or dark appearance.
    Page,
    /// A page laid out for paper: light, with blocks kept whole across pages.
    Print,
    /// A fragment for the clipboard, styled inline, since the application it
    /// is pasted into keeps no stylesheet.
    RichText,
}

/// A complete document around the export, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wrap {
    /// A page for a screen, following the reader's appearance.
    Screen,
    /// A page for paper.
    Paper,
    /// A fragment, to be placed into something else.
    Fragment,
}

/// How a task item's box survives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskBoxes {
    /// Disabled checkboxes, in list items.
    Inputs,
    /// ☐ and ☑, in list items, for a reader that drops form controls.
    Characters,
}

/// How a callout is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Callouts {
    /// An aside with a heading.
    Aside,
    /// A quote with a heading.
    Quote,
}

/// What a reader can show, which is all the rules ask.
#[derive(Clone, Debug)]
struct Target {
    wrap: Wrap,
    styling: Styling,
    formulas: FormulaStyle,
    tasks: TaskBoxes,
    callouts: Callouts,
}

impl Profile {
    fn target(self) -> Target {
        let light = Palette::light();
        match self {
            Profile::Page | Profile::Print => Target {
                wrap: if self == Profile::Page {
                    Wrap::Screen
                } else {
                    Wrap::Paper
                },
                styling: Styling::Classes,
                formulas: FormulaStyle::Vector,
                tasks: TaskBoxes::Inputs,
                callouts: Callouts::Aside,
            },
            Profile::RichText => Target {
                wrap: Wrap::Fragment,
                formulas: FormulaStyle::Raster { color: light.ink },
                styling: Styling::Inline(Box::new(light)),
                tasks: TaskBoxes::Characters,
                callouts: Callouts::Quote,
            },
        }
    }
}

/// What an export needs to know about the note beyond its document.
#[derive(Clone)]
pub struct Options {
    /// The page title, usually the note's title.
    pub title: String,
    /// The note's directory, which relative picture paths start from.
    pub base: Option<PathBuf>,
    /// The note's `typora-root-url`, which absolute picture paths start from.
    pub image_root: markraft_media::ImageRoot,
    /// Whether pictures on the web are linked; otherwise their alt text stands in.
    pub remote_images: bool,
    /// Host-owned attachments captured with the document, keyed by source URI.
    pub assets: std::collections::HashMap<String, markraft_notes::Asset>,
    /// Whether standalone display formulas are numbered.
    pub auto_number_equations: bool,
    /// The interface language, for callout titles and the page's `lang`.
    pub i18n: I18n,
}

/// `doc` as HTML for `profile`: a complete page for [`Profile::Page`] and
/// [`Profile::Print`], a fragment otherwise.
pub fn render(doc: &Node, profile: Profile, options: &Options) -> String {
    let schema = crate::doc::schema();
    let projection = Projection::of(doc, schema);
    let equations = EquationIndex::build(
        &projection,
        crate::doc::types(),
        options.auto_number_equations,
    );
    let target = profile.target();
    let wrap = target.wrap;
    let body = html::serializer(target, options, equations).serialize(doc);
    match wrap {
        Wrap::Screen => page(&body, false, options),
        Wrap::Paper => page(&body, true, options),
        Wrap::Fragment => body,
    }
}

fn page(body: &str, paper: bool, options: &Options) -> String {
    let scheme = if paper { "light" } else { "light dark" };
    format!(
        "<!doctype html>\n<html lang=\"{lang}\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"color-scheme\" content=\"{scheme}\">\n\
         <meta name=\"generator\" content=\"Markraft\">\n\
         <title>{title}</title>\n<style>\n{css}</style>\n</head>\n<body>\n\
         <main class=\"note\">\n{body}\n</main>\n</body>\n</html>\n",
        lang = markraft_commonmark::html::escape_attr(options.i18n.locale()),
        title = markraft_commonmark::html::escape_text(&options.title),
        css = style::stylesheet(paper),
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
