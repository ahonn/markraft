//! The serializer rules that turn the document into HTML someone reads, where
//! the clipboard's rules write HTML the editor reads back.
//!
//! Every rule asks the [`Target`] what the reader can show, and asks
//! [`Styling`](super::style::Styling) how an element looks; none asks which
//! reader it is.

use super::media::{Picture, Pictures};
use super::style::Role;
use super::{Callouts, Options, Target, TaskBoxes};
use crate::locale::I18n;
use markraft_commonmark::HtmlSerializer;
use markraft_commonmark::html::{HtmlNodeRule, HtmlState};
use markraft_commonmark::schema as md;
use markraft_commonmark::table::{Alignment, alignments_of};
use markraft_core::kind::equations::EquationIndex;
use markraft_core::{Mark, Node};
use std::sync::Arc;

/// The size formulas are typeset at, matching the page's body text.
const FORMULA_SIZE: f32 = 16.;

struct Context {
    target: Target,
    equations: EquationIndex,
    pictures: Pictures,
    i18n: I18n,
}

impl Context {
    /// The attributes `role` adds to its element for this reader.
    fn attrs(&self, role: Role) -> String {
        self.target.styling.attrs(role)
    }

    /// A callout's heading, by the editor's own rule.
    fn callout_title(&self, kind: &str, title: &str) -> String {
        markraft_gpui::callout_heading(kind, title, |kind| {
            let key = format!("editor.callout-{kind}");
            let named = self.i18n.text(&key);
            (named != key && named != I18n::english().text(&key)).then_some(named)
        })
    }
}

pub(super) fn serializer(
    target: Target,
    options: &Options,
    equations: EquationIndex,
) -> HtmlSerializer {
    let context = Arc::new(Context {
        target,
        equations,
        pictures: Pictures {
            base: options.base.clone(),
            root: options.image_root.clone(),
            remote: options.remote_images,
            assets: options.assets.clone(),
        },
        i18n: options.i18n.clone(),
    });
    let rule = |f: fn(&Context, &mut HtmlState<'_>, &Node, Option<&Node>)| -> HtmlNodeRule {
        let context = context.clone();
        Arc::new(move |state, node, parent| f(&context, state, node, parent))
    };
    let serializer = HtmlSerializer::commonmark(
        crate::doc::schema(),
        &markraft_commonmark::HouseStyleHandle::default(),
    )
    .with_node_rule(md::CODE_BLOCK, rule(code_block))
    .with_node_rule(md::IMAGE, rule(image))
    .with_node_rule(md::BLOCKQUOTE, rule(blockquote))
    .with_node_rule(md::WIKI_LINK, rule(wiki_link))
    .with_node_rule(md::RAW_BLOCK, rule(raw_block))
    .with_node_rule(md::RAW_INLINE, rule(|_, _, _, _| {}))
    .with_node_rule(md::TASK_ITEM, rule(task_item))
    .with_node_rule(md::BULLET_LIST, rule(bullet_list))
    .with_node_rule(md::ORDERED_LIST, rule(ordered_list))
    .with_node_rule(md::TABLE, rule(table))
    .with_node_rule(md::FOOTNOTE_DEFINITION, rule(footnote));
    let math = context.clone();
    serializer.with_atom_rule(
        md::MATH,
        Arc::new(move |state, mark, tex, position| formula(&math, state, mark, tex, position)),
    )
}

fn attr<'a>(node: &'a Node, name: &str) -> &'a str {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

fn flag(node: &Node, name: &str) -> bool {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn code_block(context: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let text: String = node.children().filter_map(Node::text).collect();
    let language = attr(node, "language");
    state.write(&format!("<pre{}><code", context.attrs(Role::CodeBlock)));
    if !language.is_empty() {
        state.attr("class", &format!("language-{language}"));
    }
    state.write(">");
    let lines = markraft_syntax::highlight(&text, language);
    // Spans that do not cover the text exactly, as when the highlighter
    // fails on a line, give way to the plain text, as they do in the editor.
    let covers = lines
        .iter()
        .zip(text.split('\n'))
        .all(|(spans, line)| spans.iter().map(|span| span.len).sum::<usize>() == line.len());
    if !covers || lines.len() != text.split('\n').count() {
        state.text(&text);
        state.write("</code></pre>");
        return;
    }
    for (index, (line, spans)) in text.split('\n').zip(lines.iter()).enumerate() {
        if index > 0 {
            state.write("\n");
        }
        let mut at = 0;
        for span in spans {
            let part = &line[at..at + span.len];
            at += span.len;
            let role = Role::Code(span.tone);
            if context.target.styling.wraps(role) {
                state.write(&format!("<span{}>", context.attrs(role)));
                state.text(part);
                state.write("</span>");
            } else {
                state.text(part);
            }
        }
    }
    state.write("</code></pre>");
}

fn formula(
    context: &Context,
    state: &mut HtmlState<'_>,
    mark: &Mark,
    tex: &str,
    position: Option<usize>,
) {
    let display = mark
        .attrs
        .get(md::MATH_DISPLAY_ATTR)
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    // Outside the document, as in a rendered HTML block, a formula has no
    // number to look up and is typeset as written.
    let equation = position.and_then(|position| context.equations.at_position(position));
    // A formula with a problem is shown as its source, as the editor shows it.
    let source = match equation {
        Some(equation) if equation.diagnostic.is_some() => None,
        Some(equation) => Some(equation.render_source.as_str()),
        None => Some(tex),
    };
    let style = context.target.formulas;
    let Some(body) =
        source.and_then(|source| super::media::formula(source, display, FORMULA_SIZE, style))
    else {
        state.write("<code>");
        state.text(tex.trim());
        state.write("</code>");
        return;
    };
    let tag = equation
        .and_then(|equation| equation.tag.as_deref())
        .and_then(|tag| super::media::formula(tag, false, FORMULA_SIZE, style));
    let role = if display {
        Role::DisplayFormula
    } else {
        Role::Formula
    };
    let attrs = context.attrs(role);
    if attrs.is_empty() && tag.is_none() {
        state.write(&body);
        return;
    }
    state.write(&format!("<span{attrs}>"));
    state.write(&body);
    if let Some(tag) = tag {
        let tag_attrs = context.attrs(Role::EquationTag);
        if tag_attrs.is_empty() {
            // Without a stylesheet to place it, the number follows the formula.
            state.write("&nbsp;&nbsp;");
            state.write(&tag);
        } else {
            state.write(&format!("<span{tag_attrs}>{tag}</span>"));
        }
    }
    state.write("</span>");
}

fn image(context: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let alt = attr(node, "alt");
    let src = match context.pictures.picture(attr(node, "src")) {
        Picture::Embedded(uri) => uri,
        Picture::Linked(url) => url,
        Picture::Unavailable => {
            state.text(alt);
            return;
        }
    };
    state.write("<img");
    state.attr("src", &src);
    state.attr("alt", alt);
    state.attr("title", attr(node, "title"));
    state.attr("width", attr(node, "width"));
    state.attr("height", attr(node, "height"));
    state.write(">");
}

fn blockquote(context: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let kind = attr(node, "callout");
    if kind.is_empty() {
        state.write("<blockquote>\n");
        state.render_content(node);
        state.write("\n</blockquote>");
        return;
    }
    let title = context.callout_title(kind, attr(node, "title"));
    let tone = markraft_gpui::callout_tone(kind);
    let element = match context.target.callouts {
        Callouts::Aside => "aside",
        Callouts::Quote => "blockquote",
    };
    state.write(&format!(
        "<{element}{}>\n<p{}>",
        context.attrs(Role::Callout(tone)),
        context.attrs(Role::CalloutTitle(tone))
    ));
    state.text(&title);
    state.write("</p>\n");
    state.render_content(node);
    state.write(&format!("\n</{element}>"));
}

/// A wiki link reads as its label without pointing anywhere. An embed of a
/// picture (`![[pic.png]]`) is that picture, as the editor draws it; one that
/// finds no picture, a note included, reads as the name it was written with.
fn wiki_link(context: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let target = attr(node, "target");
    if flag(node, "embed") {
        let src = match context.pictures.picture(target.trim()) {
            Picture::Embedded(src) | Picture::Linked(src) => Some(src),
            Picture::Unavailable => None,
        };
        if let Some(src) = src {
            state.write("<img");
            state.attr("src", &src);
            state.attr("alt", target.trim());
            state.write(">");
            return;
        }
    }
    let alias = attr(node, "alias");
    let label = if alias.is_empty() { target } else { alias };
    if context.target.styling.wraps(Role::WikiLink) {
        state.write(&format!("<span{}>", context.attrs(Role::WikiLink)));
        state.text(label);
        state.write("</span>");
    } else {
        state.text(label);
    }
}

/// An HTML block as the editor draws it once the caret leaves, or as its source
/// where the editor keeps showing the source.
fn raw_block(context: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let source: String = node.children().filter_map(Node::text).collect();
    let Some(rendered) =
        markraft_commonmark::html::render_html_block(crate::doc::schema(), &source)
    else {
        state.write(&format!("<pre{}>", context.attrs(Role::RawSource)));
        state.text(&source);
        state.write("</pre>");
        return;
    };
    for (index, block) in rendered.doc.children().enumerate() {
        if index > 0 {
            state.write("\n");
        }
        let align = match rendered.aligns.get(index) {
            Some(markraft_core::kind::Align::Center) => Some("center"),
            Some(markraft_core::kind::Align::End) => Some("right"),
            _ => None,
        };
        if let Some(align) = align {
            state.write(&format!("<div style=\"text-align:{align}\">"));
        }
        state.render_detached(block, Some(&rendered.doc));
        if align.is_some() {
            state.write("</div>");
        }
    }
}

/// A task item's box, then its first paragraph on the same line.
fn task_item(context: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let done = flag(node, markraft_core::kind::TASK_CHECKED_ATTR);
    let tasks = context.target.tasks;
    state.write(&format!("<li{}>", context.attrs(Role::Task { done })));
    match tasks {
        TaskBoxes::Inputs => state.write(if done {
            "<input type=\"checkbox\" disabled checked>"
        } else {
            "<input type=\"checkbox\" disabled>"
        }),
        TaskBoxes::Characters => state.write(if done { "☑ " } else { "☐ " }),
    }
    let paragraph = crate::doc::schema().node_id(md::PARAGRAPH);
    let inline_first = node
        .first_child()
        .is_some_and(|first| Some(first.type_id()) == paragraph);
    if inline_first {
        state.within_child(node, 0, |state, first| state.render_inline(first));
    }
    for index in usize::from(inline_first)..node.child_count() {
        state.write("\n");
        state.render_child(node, index);
    }
    state.write("</li>");
}

fn bullet_list(_: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    state.write("<ul>\n");
    state.render_content(node);
    state.write("\n</ul>");
}

fn ordered_list(_: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    state.write("<ol");
    let start = node
        .attrs()
        .get("start")
        .and_then(|value| value.as_int())
        .unwrap_or(1);
    if start != 1 {
        state.attr("start", &start.to_string());
    }
    state.write(">\n");
    state.render_content(node);
    state.write("\n</ol>");
}

/// The header row in its `<thead>`, each cell aligned as its column says.
fn table(context: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let alignments = alignments_of(node);
    state.write(&format!("<table{}>\n<thead>\n", context.attrs(Role::Table)));
    if node.child_count() > 0 {
        state.within_child(node, 0, |state, header| {
            table_row(context, state, header, &alignments, true)
        });
        state.write("\n");
    }
    state.write("</thead>");
    if node.child_count() > 1 {
        state.write("\n<tbody>\n");
        for index in 1..node.child_count() {
            if index > 1 {
                state.write("\n");
            }
            state.within_child(node, index, |state, row| {
                table_row(context, state, row, &alignments, false)
            });
        }
        state.write("\n</tbody>");
    }
    state.write("\n</table>");
}

/// One row, written while it is the node being rendered.
fn table_row(
    context: &Context,
    state: &mut HtmlState<'_>,
    row: &Node,
    alignments: &[Alignment],
    header: bool,
) {
    let (tag, role) = if header {
        ("th", Role::TableHeader)
    } else {
        ("td", Role::TableCell)
    };
    state.write("<tr>");
    for index in 0..row.child_count() {
        state.write(&format!("<{tag}{}", context.attrs(role)));
        let alignment = alignments.get(index).copied().unwrap_or_default();
        if alignment != Alignment::None {
            state.attr("align", alignment.name());
        }
        state.write(">");
        state.within_child(row, index, |state, cell| state.render_inline(cell));
        state.write(&format!("</{tag}>"));
    }
    state.write("</tr>");
}

fn footnote(context: &Context, state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let label = attr(node, md::FOOTNOTE_LABEL_ATTR);
    state.write(&format!("<div{}", context.attrs(Role::Footnote)));
    state.attr("id", &format!("fn-{label}"));
    state.write(&format!("><span{}>", context.attrs(Role::FootnoteLabel)));
    state.text(label);
    state.write("</span>\n");
    state.render_content(node);
    state.write("\n</div>");
}
