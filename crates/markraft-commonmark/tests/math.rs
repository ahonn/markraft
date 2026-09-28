//! Display formulas preserve literal TeX across editing and persistence.

use markraft_commonmark::{
    SourceDocument, commonmark_schema, from_markdown, schema as md, to_markdown,
};

#[test]
fn standalone_display_formulas_keep_blank_lines_and_markdown_looking_tex() {
    let schema = commonmark_schema();
    for formula in [
        "$$\n\n$$",
        "$$\n  x = 1\n\n  y = 2\n$$",
        "$$\n# x\n> y\n- z\n---\n$$",
        "$$\n\\begin{aligned}\nx &= 1 \\\\\n\ny &= 2\n\\end{aligned}\n$$",
        "$$\n~~~\nx\n~~~\n$$",
        "$$x\n\nx + 1$$",
        "$$x + 1$$",
    ] {
        let source = SourceDocument::parse(&schema, formula).unwrap();
        assert_eq!(source.document().child_count(), 1, "{formula:?}");
        assert_eq!(
            source.document().child(0).type_id(),
            schema.node_id(md::PARAGRAPH).unwrap()
        );
        assert_eq!(to_markdown(&schema, source.document()), formula);
        assert_eq!(source.render(&schema, source.document()).unwrap(), formula);
        assert_eq!(
            from_markdown(&schema, &to_markdown(&schema, source.document())).unwrap(),
            *source.document()
        );
    }
}

#[test]
fn inline_math_and_distinct_display_spans_keep_existing_paragraph_behavior() {
    let schema = commonmark_schema();
    for text in ["before $x$ after", "$$x$$ and $$y$$", "before\n$$\nx\n$$"] {
        let source = SourceDocument::parse(&schema, text).unwrap();
        assert_eq!(source.document().child_count(), 1);
        assert_eq!(to_markdown(&schema, source.document()), text);
    }
}

#[test]
fn an_unclosed_formula_never_takes_source_from_a_later_container() {
    let schema = commonmark_schema();
    for text in [
        "> $$\n> x\n\noutside\n\n> $$",
        "- $$\n  x\n\noutside\n\n  $$",
    ] {
        let source = SourceDocument::parse(&schema, text).unwrap();
        assert_eq!(source.document().child_count(), 3, "{text:?}");
        assert_eq!(source.render(&schema, source.document()).unwrap(), text);
        let canonical = to_markdown(&schema, source.document());
        assert_eq!(
            from_markdown(&schema, &canonical).unwrap(),
            *source.document()
        );
    }
}

#[test]
fn editing_display_math_preserves_neighboring_source_and_container_prefixes() {
    use markraft_core::commands::{insert_text, run_command};
    use markraft_core::{EditorState, EditorStateConfig, Selection};

    let schema = commonmark_schema();
    for formula in [
        "$$\n  x = 1\n\n# y\n$$",
        "> $$\n> \n>   x = 1\n> $$",
        "- $$\n  \n    x = 1\n  $$",
        "- [ ] $$\n  \n    x = 1\n  $$",
    ] {
        let original = format!("---\nid: keep\n---\n\nBefore\n\n{formula}\n\nAfter\n");
        let source = SourceDocument::parse(&schema, &original).unwrap();
        let mut cursor = None;
        source.document().descendants(&mut |node, pos, _, _| {
            if let Some(text) = node.text()
                && let Some(offset) = text.find("x = 1")
            {
                cursor = Some(pos + text[..offset].chars().count() + 4);
            }
            true
        });
        let state = EditorState::create(
            EditorStateConfig::new(schema.clone())
                .doc(source.document().clone())
                .selection(Selection::cursor(cursor.unwrap()))
                .extensions(markraft_commonmark::commonmark_extensions(&schema)),
        )
        .unwrap();
        let edited = run_command(&state, &insert_text("2")).unwrap().unwrap();
        let saved = source.render(&schema, edited.state().doc()).unwrap();
        assert_eq!(saved, original.replace("x = 1", "x = 21"));
        assert_eq!(
            SourceDocument::parse(&schema, &saved).unwrap().document(),
            edited.state().doc()
        );
    }
}

#[test]
fn display_fences_inside_code_stay_code() {
    let schema = commonmark_schema();
    for text in ["```text\n$$\n\nx\n$$\n```", "    $$\n    x\n    $$"] {
        let document = from_markdown(&schema, text).unwrap();
        assert_eq!(document.child_count(), 1);
        assert_eq!(
            document.child(0).type_id(),
            schema.node_id(md::CODE_BLOCK).unwrap()
        );
    }
}

#[test]
fn neighboring_blocks_and_multiple_formulas_stay_separate() {
    let schema = commonmark_schema();
    let text = "# Before\n\n$$\n\nx\n$$\n\nBetween\n\n$$\ny\n\n$$\n\nAfter";
    let source = SourceDocument::parse(&schema, text).unwrap();
    assert_eq!(source.document().child_count(), 5);
    assert_eq!(to_markdown(&schema, source.document()), text);
    assert_eq!(source.render(&schema, source.document()).unwrap(), text);
}

#[test]
fn display_formulas_in_lists_and_quotes_preserve_their_containers() {
    let schema = commonmark_schema();
    for text in [
        "> $$\n> \n> x = 1\n> $$",
        "- $$\n  \n  x = 1\n  $$",
        "1. $$\n   \n   x = 1\n   $$",
        "> - $$\n>   \n>     x = 1\n>   $$",
        "> - [x] $$\n>   \n>     中文 = α\n>   $$",
    ] {
        let source = SourceDocument::parse(&schema, text).unwrap();
        let saved = source.render(&schema, source.document()).unwrap();
        assert_eq!(saved, text);
        let canonical = to_markdown(&schema, source.document());
        assert_eq!(
            from_markdown(&schema, &canonical).unwrap(),
            *source.document(),
            "{text:?} => {canonical:?}"
        );
        assert!(canonical.contains("$$"));
        assert!(!canonical.contains("\\$"));
    }
}

#[test]
fn a_formula_never_closes_inside_a_code_or_html_block() {
    let schema = commonmark_schema();
    let code = schema.node_id(md::CODE_BLOCK).unwrap();
    let heading = schema.node_id(md::HEADING).unwrap();
    for text in [
        "$$\n```sh\necho $$\n```\n\n# Title\n\nPara",
        "$$\n<div>\ncost $$\n</div>\n\n# Title",
    ] {
        let source = SourceDocument::parse(&schema, text).unwrap();
        let doc = source.document();
        let kinds: Vec<_> = (0..doc.child_count())
            .map(|index| doc.child(index).type_id())
            .collect();
        assert!(kinds.contains(&heading), "{text:?}: {kinds:?}");
        if text.contains("```") {
            assert!(kinds.contains(&code), "{text:?}: {kinds:?}");
        }
        assert_eq!(source.render(&schema, doc).unwrap(), text);
        assert_eq!(
            from_markdown(&schema, &to_markdown(&schema, doc)).unwrap(),
            *doc
        );
    }
}

#[test]
fn formula_lines_that_look_like_setext_underlines_stay_tex() {
    let schema = commonmark_schema();
    for formula in [
        "$$\nx\n---\n$$",
        "$$\nx\n===\n$$",
        "$$\n---\n$$",
        "- $$\n  x\n  ---\n  $$",
    ] {
        let source = SourceDocument::parse(&schema, formula).unwrap();
        assert_eq!(source.document().child_count(), 1, "{formula:?}");
        assert_eq!(to_markdown(&schema, source.document()), formula);
        assert_eq!(
            from_markdown(&schema, &to_markdown(&schema, source.document())).unwrap(),
            *source.document(),
            "{formula:?}"
        );
    }
    let paragraph = schema.node_id(md::PARAGRAPH).unwrap();
    let source = SourceDocument::parse(&schema, "$$\nx\n---\n$$").unwrap();
    assert_eq!(source.document().child(0).type_id(), paragraph);
}

#[test]
fn an_unclosed_formula_never_pairs_with_the_opening_fence_of_a_later_one() {
    let schema = commonmark_schema();
    for text in ["$$\n\n# A\n\ntext\n\n$$\nx\n$$", "$$\n\ntext\n\n$$\nx\n$$"] {
        let source = SourceDocument::parse(&schema, text).unwrap();
        let doc = source.document();
        let last = doc.child(doc.child_count() - 1);
        let mut body = String::new();
        last.descendants(&mut |node, _, _, _| {
            if let Some(text) = node.text() {
                body.push_str(text);
            }
            true
        });
        // Literal line breaks are atoms, so only the text runs remain.
        assert_eq!(body, "$$x$$", "{text:?}");
        assert_eq!(source.render(&schema, doc).unwrap(), text);
        assert_eq!(to_markdown(&schema, doc), text);
    }
}
