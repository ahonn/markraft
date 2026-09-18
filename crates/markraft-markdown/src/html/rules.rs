//! HTML parse rules: which element becomes which schema type.
//!
//! The table is ProseMirror's `parseDOM` in this crate's shape — a list of
//! (tag, optional predicate, rule) tried in order, so a narrow rule can sit in
//! front of a general one: a `<li>` holding its own check box is a task item,
//! and every other `<li>` is a plain one.
//!
//! An element the table does not mention passes its children through, which is
//! what keeps a `<font>` or a web component from swallowing the text inside it.

use std::sync::Arc;

use markraft_doc::{Attrs, Schema, attrs};
use scraper::ElementRef;

use crate::schema as md;

/// What a rule is given about the element it is building from.
#[derive(Clone, Copy)]
pub struct HtmlTarget<'a> {
    /// The element.
    pub element: ElementRef<'a>,
    /// The schema the document is being built against.
    pub schema: &'a Schema,
}

impl HtmlTarget<'_> {
    /// The element's tag name, lowercased by the HTML parser already.
    pub fn tag(&self) -> &str {
        self.element.value().name()
    }

    /// An attribute of the element.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.element.value().attr(name)
    }

    /// The element's text content, with nothing collapsed — what a `<pre>`
    /// holds.
    pub fn text(&self) -> String {
        self.element
            .text()
            .collect::<String>()
            .replace("\r\n", "\n")
            .replace('\r', "\n")
    }
}

/// Builds the attributes of the node or mark a rule creates.
pub type HtmlAttrsFn = Arc<dyn for<'a> Fn(HtmlTarget<'a>) -> Attrs + Send + Sync>;
/// Decides whether a rule applies to an element.
pub type HtmlMatchFn = Arc<dyn for<'a> Fn(HtmlTarget<'a>) -> bool + Send + Sync>;

/// Wrap a closure as an [`HtmlAttrsFn`].
pub fn html_attrs_fn<F>(f: F) -> HtmlAttrsFn
where
    F: for<'a> Fn(HtmlTarget<'a>) -> Attrs + Send + Sync + 'static,
{
    Arc::new(f)
}

/// Wrap a closure as an [`HtmlMatchFn`].
pub fn html_match_fn<F>(f: F) -> HtmlMatchFn
where
    F: for<'a> Fn(HtmlTarget<'a>) -> bool + Send + Sync + 'static,
{
    Arc::new(f)
}

fn no_attrs() -> HtmlAttrsFn {
    html_attrs_fn(|_| Attrs::empty())
}

/// What to do with one element.
#[derive(Clone)]
pub enum HtmlRule {
    /// Build a block node and parse the element's children into its content —
    /// as inline content when the schema type takes inline content, as blocks
    /// otherwise, and not at all for a leaf.
    Block {
        /// The schema node type to build.
        node_type: String,
        /// Its attributes.
        attrs: HtmlAttrsFn,
    },
    /// Build a block node whose content is the element's text, taken verbatim.
    TextBlock {
        /// The schema node type to build.
        node_type: String,
        /// Its attributes.
        attrs: HtmlAttrsFn,
    },
    /// Build an inline leaf. The element's children are not visited.
    Atom {
        /// The schema node type to build.
        node_type: String,
        /// Its attributes.
        attrs: HtmlAttrsFn,
    },
    /// Add a mark to everything the element's children produce.
    Mark {
        /// The schema mark type to add.
        mark_type: String,
        /// Its attributes.
        attrs: HtmlAttrsFn,
    },
    /// Build an inline leaf that ends the line — a `<br>`.
    LineBreak {
        /// The schema node type to build.
        node_type: String,
    },
    /// Drop the element and everything under it.
    Ignore,
    /// Render the children in place, with no block boundary of its own.
    Inline,
    /// Render the children in place, but end the block before and after — what
    /// a `<div>` does to the text around it.
    Boundary,
}

impl std::fmt::Debug for HtmlRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            HtmlRule::Block { .. } => "Block",
            HtmlRule::TextBlock { .. } => "TextBlock",
            HtmlRule::Atom { .. } => "Atom",
            HtmlRule::Mark { .. } => "Mark",
            HtmlRule::LineBreak { .. } => "LineBreak",
            HtmlRule::Ignore => "Ignore",
            HtmlRule::Inline => "Inline",
            HtmlRule::Boundary => "Boundary",
        };
        f.write_str(name)
    }
}

impl HtmlRule {
    /// A block node of a fixed type with default attributes.
    pub fn block(node_type: &str) -> HtmlRule {
        HtmlRule::Block {
            node_type: node_type.to_string(),
            attrs: no_attrs(),
        }
    }

    /// A block node of a fixed type with computed attributes.
    pub fn block_with(node_type: &str, attrs: HtmlAttrsFn) -> HtmlRule {
        HtmlRule::Block {
            node_type: node_type.to_string(),
            attrs,
        }
    }

    /// A mark of a fixed type with default attributes.
    pub fn mark(mark_type: &str) -> HtmlRule {
        HtmlRule::Mark {
            mark_type: mark_type.to_string(),
            attrs: no_attrs(),
        }
    }

    /// A mark of a fixed type with computed attributes.
    pub fn mark_with(mark_type: &str, attrs: HtmlAttrsFn) -> HtmlRule {
        HtmlRule::Mark {
            mark_type: mark_type.to_string(),
            attrs,
        }
    }
}

/// An ordered table of [`HtmlRule`]s.
#[derive(Clone)]
pub struct HtmlRules {
    rules: Vec<(String, Option<HtmlMatchFn>, HtmlRule)>,
    fallback: HtmlRule,
}

impl std::fmt::Debug for HtmlRules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tags: Vec<&str> = self.rules.iter().map(|(tag, _, _)| tag.as_str()).collect();
        f.debug_struct("HtmlRules")
            .field("tags", &tags)
            .field("fallback", &self.fallback)
            .finish()
    }
}

impl Default for HtmlRules {
    fn default() -> Self {
        HtmlRules::new()
    }
}

impl HtmlRules {
    /// A table whose fallback passes an unknown element's children through.
    pub fn new() -> HtmlRules {
        HtmlRules {
            rules: Vec::new(),
            fallback: HtmlRule::Inline,
        }
    }

    /// Append a rule for every element with this tag.
    pub fn with(mut self, tag: &str, rule: HtmlRule) -> HtmlRules {
        self.rules.push((tag.to_string(), None, rule));
        self
    }

    /// Append a rule for the elements with this tag that `matches` accepts.
    ///
    /// Rules are tried in the order they were added, so a narrow rule has to be
    /// added before the general one it refines.
    pub fn matching(mut self, tag: &str, matches: HtmlMatchFn, rule: HtmlRule) -> HtmlRules {
        self.rules.push((tag.to_string(), Some(matches), rule));
        self
    }

    /// Append the same rule for several tags.
    pub fn with_all(mut self, tags: &[&str], rule: HtmlRule) -> HtmlRules {
        for tag in tags {
            self = self.with(tag, rule.clone());
        }
        self
    }

    /// Replace the rule used for elements the table does not mention.
    pub fn fallback(mut self, rule: HtmlRule) -> HtmlRules {
        self.fallback = rule;
        self
    }

    /// The first rule that applies to `target`, or the fallback.
    pub fn rule(&self, target: HtmlTarget<'_>) -> &HtmlRule {
        let tag = target.tag();
        self.rules
            .iter()
            .find(|(name, matches, _)| {
                name == tag && matches.as_ref().is_none_or(|test| test(target))
            })
            .map(|(_, _, rule)| rule)
            .unwrap_or(&self.fallback)
    }
}

/// Whether the element owns a checked or unchecked task box.
///
/// Only a check box this item owns counts: one inside a nested list belongs to
/// that list's item instead.
fn task_state(target: HtmlTarget<'_>) -> Option<bool> {
    if target.attr("data-type") == Some("taskItem") {
        return Some(target.attr("data-checked") == Some("true"));
    }
    let item = target.element;
    item.descendants()
        .filter_map(ElementRef::wrap)
        .find(|element| {
            element.value().name() == "input"
                && element.value().attr("type") == Some("checkbox")
                && element
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .find(|ancestor| ancestor.value().name() == "li")
                    .is_some_and(|ancestor| ancestor.id() == item.id())
        })
        .map(|box_| box_.value().attr("checked").is_some())
}

/// Whether a list renders without `<p>` wrappers.
///
/// HTML has no element for it, so Markraft's own output says so and every other
/// writer's is taken as tight, which is what a reader shows.
fn is_tight(target: HtmlTarget<'_>) -> bool {
    target.attr("data-tight") != Some("false")
}

/// The rule table for the CommonMark/GFM preset.
pub fn commonmark_html_rules() -> HtmlRules {
    let mut rules = HtmlRules::new()
        .with_all(
            &[
                "script", "style", "head", "template", "noscript", "title", "meta", "link",
            ],
            HtmlRule::Ignore,
        )
        .with("input", HtmlRule::Ignore)
        .with("p", HtmlRule::block(md::PARAGRAPH))
        .with("blockquote", HtmlRule::block(md::BLOCKQUOTE))
        .with("hr", HtmlRule::block(md::HORIZONTAL_RULE))
        // Before the general `<pre>`: a block whose source the model does not
        // interpret is written as one, and has to come back as one.
        .matching(
            "pre",
            html_match_fn(|target| target.attr("data-type") == Some("rawBlock")),
            HtmlRule::block_with(
                md::RAW_BLOCK,
                html_attrs_fn(|target| {
                    let text = target.text();
                    let source = text.strip_suffix('\n').unwrap_or(&text).to_string();
                    attrs! {"source" => source}
                }),
            ),
        )
        .with(
            "pre",
            HtmlRule::TextBlock {
                node_type: md::CODE_BLOCK.to_string(),
                attrs: html_attrs_fn(|target| {
                    let language = target
                        .element
                        .descendants()
                        .filter_map(ElementRef::wrap)
                        .find(|element| element.value().name() == "code")
                        .and_then(|code| code.value().attr("class"))
                        .and_then(|classes| {
                            classes
                                .split_whitespace()
                                .find_map(|class| class.strip_prefix("language-"))
                        })
                        .unwrap_or("")
                        .to_string();
                    let fence = target.attr("data-fence").unwrap_or("`").to_string();
                    let length: i64 = target
                        .attr("data-fence-length")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(3);
                    attrs! {
                        "language" => language,
                        "fence_char" => fence,
                        "fence_length" => length,
                    }
                }),
            },
        )
        .with(
            "ul",
            HtmlRule::block_with(
                md::BULLET_LIST,
                html_attrs_fn(|target| {
                    let bullet = target.attr("data-bullet").unwrap_or("-").to_string();
                    attrs! {"bullet_char" => bullet, "tight" => is_tight(target)}
                }),
            ),
        )
        .with(
            "ol",
            HtmlRule::block_with(
                md::ORDERED_LIST,
                html_attrs_fn(|target| {
                    let start: i64 = target
                        .attr("start")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1);
                    let delimiter = target.attr("data-delimiter").unwrap_or(".").to_string();
                    attrs! {
                        "start" => start,
                        "delimiter" => delimiter,
                        "tight" => is_tight(target),
                    }
                }),
            ),
        )
        .matching(
            "li",
            html_match_fn(|target| task_state(target).is_some()),
            HtmlRule::block_with(
                md::TASK_ITEM,
                html_attrs_fn(|target| attrs! {"checked" => task_state(target).unwrap_or(false)}),
            ),
        )
        .with("li", HtmlRule::block(md::LIST_ITEM))
        .with(
            "img",
            HtmlRule::Atom {
                node_type: md::IMAGE.to_string(),
                attrs: html_attrs_fn(|target| {
                    attrs! {
                        "src" => target.attr("src").unwrap_or_default().to_string(),
                        "alt" => target.attr("alt").unwrap_or_default().to_string(),
                        "title" => target.attr("title").unwrap_or_default().to_string(),
                    }
                }),
            },
        )
        .with(
            "br",
            HtmlRule::LineBreak {
                node_type: md::HARD_BREAK.to_string(),
            },
        )
        .matching(
            "a",
            html_match_fn(|target| target.attr("href").is_some()),
            HtmlRule::mark_with(
                md::LINK,
                html_attrs_fn(|target| {
                    attrs! {
                        "href" => target.attr("href").unwrap_or_default().to_string(),
                        "title" => target.attr("title").unwrap_or_default().to_string(),
                    }
                }),
            ),
        )
        .with_all(&["strong", "b"], HtmlRule::mark(md::STRONG))
        .with_all(&["em", "i"], HtmlRule::mark(md::EM))
        .with_all(&["s", "del", "strike"], HtmlRule::mark(md::STRIKETHROUGH))
        .with_all(&["u", "ins"], HtmlRule::mark(md::UNDERLINE))
        .with("code", HtmlRule::mark(md::CODE))
        .with_all(
            &[
                "div",
                "section",
                "article",
                "main",
                "header",
                "footer",
                "figure",
                "figcaption",
                "tr",
                "dt",
                "dd",
            ],
            HtmlRule::Boundary,
        );
    for level in 1..=6 {
        rules = rules.with(
            &format!("h{level}"),
            HtmlRule::block_with(
                md::HEADING,
                html_attrs_fn(move |target| {
                    let level: i64 = target.tag()[1..].parse().unwrap_or(1);
                    attrs! {"level" => level.clamp(1, 6)}
                }),
            ),
        );
    }
    rules
}
