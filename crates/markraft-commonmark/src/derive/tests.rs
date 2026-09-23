use super::*;

/// A compact spelling of what [`derive`] said about `text`, character offsets
/// throughout: `strong(0..6)` for a style, `link(0..9 "/u" "")` for a link,
/// `0..2@0` for a concealed run under span id 0 (`="&"` when it displays
/// something), `br(3)` for a hard break.
fn read(kind: BlockKind, text: &str) -> String {
    read_with(kind, text, &DeriveContext::new())
}

fn read_with(kind: BlockKind, text: &str, ctx: &DeriveContext) -> String {
    let derived = derive(kind, text, ctx);
    let mut out = Vec::new();
    for span in &derived.styles {
        let range = &span.range;
        out.push(match &span.style {
            Style::Link { href, title } => format!("link({range:?} {href:?} {title:?})"),
            style => format!("{}({range:?})", style.mark_name()),
        });
    }
    for conceal in &derived.conceals {
        let display = if conceal.display.is_empty() {
            String::new()
        } else {
            format!("={:?}", conceal.display)
        };
        out.push(format!("{:?}@{}{display}", conceal.range, conceal.span));
    }
    for at in &derived.hard_breaks {
        out.push(format!("br({at})"));
    }
    out.join(" ")
}

fn para(text: &str) -> String {
    read(BlockKind::Paragraph, text)
}

/// What a reader sees: concealed runs replaced by what they display.
fn visible(kind: BlockKind, text: &str) -> String {
    let derived = derive(kind, text, &DeriveContext::new());
    let mut out = String::new();
    for (index, character) in text.chars().enumerate() {
        match derived.conceal_at(index) {
            Some(conceal) if conceal.range.start == index => out.push_str(&conceal.display),
            Some(_) => {}
            None => out.push(character),
        }
    }
    out
}

// -- delimiters -----------------------------------------------------------

#[test]
fn a_style_covers_its_delimiters_and_conceals_them_as_one_span() {
    assert_eq!(para("**ab**"), "strong(0..6) 0..2@0 4..6@0");
    assert_eq!(
        para("~~s~~ ~t~"),
        "strikethrough(0..5) strikethrough(6..9) 0..2@0 3..5@0 6..7@1 8..9@1"
    );
    assert_eq!(visible(BlockKind::Paragraph, "a **b** c"), "a b c");
}

#[test]
fn nested_spans_each_keep_their_own_delimiters() {
    assert_eq!(
        para("*a **b** c*"),
        "em(0..11) strong(3..8) 0..1@0 3..5@1 6..8@1 10..11@0"
    );
    // One run of three closes both spans: comrak splits it, and so does the
    // mapping.
    assert_eq!(
        para("***a***"),
        "em(0..7) strong(1..6) 0..1@0 1..3@1 4..6@1 6..7@0"
    );
}

#[test]
fn highlight_and_superscript_conceal_their_delimiters() {
    assert_eq!(para("a ==b== c"), "highlight(2..7) 2..4@0 5..7@0");
    assert_eq!(para("x^2^"), "superscript(1..4) 1..2@0 3..4@0");
    assert_eq!(visible(BlockKind::Paragraph, "==b== x^2^"), "b x2");
    // Highlight takes exactly two `=`, and superscript no space-free run.
    assert_eq!(para("a == b == c"), "");
    assert_eq!(para("a ===b=== c"), "");
    assert_eq!(para("2^10"), "");
}

#[test]
fn a_formula_conceals_its_fences_and_reads_nothing_inside() {
    assert_eq!(para("$x^2$"), "math(0..5) 0..1@0 4..5@0");
    assert_eq!(para("$$a*b*c$$"), "math(0..9) 0..2@0 7..9@0");
    assert_eq!(para("$`x^2`$"), "math(0..7) 0..2@0 5..7@0");
    let derived = derive(
        BlockKind::Paragraph,
        "$$\nE=mc^2\n$$",
        &DeriveContext::new(),
    );
    assert_eq!(
        derived.styles,
        [StyleSpan {
            range: 0..12,
            style: Style::Math { display: true },
        }]
    );
    assert_eq!(
        derive(BlockKind::Paragraph, "$x$", &DeriveContext::new()).styles[0].style,
        Style::Math { display: false }
    );
}

#[test]
fn dollars_a_formula_cannot_close_are_text() {
    for text in ["costs $5 and $10", "$ a $", "a $b $c", "$5$6"] {
        assert_eq!(para(text), "", "{text:?}");
    }
}

#[test]
fn emphasis_next_to_cjk_punctuation_is_read() {
    assert_eq!(para("**注意：**这里"), "strong(0..7) 0..2@0 5..7@0");
    assert_eq!(para("中文**“引号”**中文"), "strong(2..10) 2..4@0 8..10@0");
    assert_eq!(para("中_文_字"), "");
}

#[test]
fn paired_mark_and_sup_tags_style_as_their_markdown_does() {
    assert_eq!(
        para("<mark>a</mark> <sup>2</sup>"),
        "highlight(0..14) superscript(15..27) 0..6@0 7..14@0 15..20@1 21..27@1"
    );
}

#[test]
fn an_unpaired_delimiter_is_plain_text() {
    assert_eq!(para("**ab"), "");
    assert_eq!(para("a*b"), "");
    assert_eq!(para("** a**"), "");
}

// -- code spans -----------------------------------------------------------

#[test]
fn a_multi_backtick_code_span_conceals_its_whole_fence() {
    assert_eq!(para("``a ` b``"), "code(0..9) 0..2@0 7..9@0");
    assert_eq!(visible(BlockKind::Paragraph, "``a ` b``"), "a ` b");
}

#[test]
fn a_code_span_conceals_the_one_space_a_reader_strips() {
    assert_eq!(para("`` `a` ``"), "code(0..9) 0..3@0 6..9@0");
    assert_eq!(visible(BlockKind::Paragraph, "`` `a` ``"), "`a`");
    // All spaces: nothing is stripped.
    assert_eq!(para("`  `"), "code(0..4) 0..1@0 3..4@0");
    // A line ending counts as a space.
    assert_eq!(para("`\nb `"), "code(0..5) 0..2@0 3..5@0");
}

#[test]
fn a_code_span_closing_on_an_indented_line_ends_at_its_backticks() {
    // comrak reports this span as ending two columns early.
    assert_eq!(
        para("aaa\n   `  bbb\n     ` ccc"),
        "code(7..20) 7..9@0 13..20@0"
    );
    // …and this one past the end of its line.
    assert_eq!(para("``\n f`oo \n&`"), "code(5..12) 5..6@0 11..12@0");
    // A span that closes with one keeps its delimiters where they are.
    assert_eq!(
        para("x\n  *a `b\n c`*"),
        "em(4..14) code(7..13) 4..5@0 7..8@1 12..13@1 13..14@0"
    );
    // The indentation after a padding line ending is spelling with it.
    assert_eq!(para("`\n  a\n `"), "code(0..8) 0..4@0 5..8@0");
}

#[test]
fn escapes_and_entities_inside_a_code_span_are_content() {
    assert_eq!(para(r"`\*`"), "code(0..4) 0..1@0 3..4@0");
    assert_eq!(para("`&amp;`"), "code(0..7) 0..1@0 6..7@0");
}

// -- links ----------------------------------------------------------------

#[test]
fn an_inline_link_conceals_its_brackets_and_destination() {
    assert_eq!(para("[a](/u) x"), r#"link(0..7 "/u" "") 0..1@0 2..7@0"#);
    assert_eq!(visible(BlockKind::Paragraph, "[a](/u) x"), "a x");
}

#[test]
fn entities_and_escapes_in_a_destination_are_resolved_not_concealed() {
    assert_eq!(
        para(r#"[a](/u&amp;v "t&quot;")"#),
        r#"link(0..23 "/u&v" "t\"") 0..1@0 2..23@0"#
    );
    assert_eq!(
        para(r"[a](\(foo\))"),
        r#"link(0..12 "(foo)" "") 0..1@0 2..12@0"#
    );
    assert_eq!(
        para("[a](</my uri>)"),
        r#"link(0..14 "/my uri" "") 0..1@0 2..14@0"#
    );
}

#[test]
fn angle_autolinks_conceal_their_brackets_and_bare_ones_nothing() {
    assert_eq!(
        para("<https://a.b/c>"),
        r#"link(0..15 "https://a.b/c" "") 0..1@0 14..15@0"#
    );
    assert_eq!(
        para("see https://x.y and www.a.com"),
        r#"link(4..15 "https://x.y" "") link(20..29 "http://www.a.com" "")"#
    );
    assert_eq!(
        para("<me@a.example>"),
        r#"link(0..14 "mailto:me@a.example" "") 0..1@0 13..14@0"#
    );
}

#[test]
fn link_text_may_span_lines_and_hold_styles() {
    assert_eq!(
        para("[a *b*\nc](/u \"t\") x"),
        r#"link(0..17 "/u" "t") em(3..6) 0..1@0 3..4@1 5..6@1 8..17@0"#
    );
}

/// comrak does not count a line ending inside a link's `(…)`: it reports this
/// link as ending at `/u` and everything on the following lines one line too
/// early. See the module documentation for how the tail is read instead.
#[test]
fn a_link_whose_title_is_on_the_next_line_conceals_all_of_it() {
    assert_eq!(
        para("[a](/u\n\"t\") x\n*b*"),
        r#"link(0..11 "/u" "t") em(14..17) 0..1@0 2..11@0 14..15@1 16..17@1"#
    );
}

#[test]
fn an_empty_link_label_still_conceals_both_runs() {
    assert_eq!(para("[](/u)"), r#"link(0..6 "/u" "") 0..1@0 1..6@0"#);
}

#[test]
fn reference_links_resolve_against_the_context() {
    let ctx = DeriveContext::new().with_definitions("[r]: /u \"t\"");
    assert_eq!(
        read_with(BlockKind::Paragraph, "[a][r] [r][] [r]", &ctx),
        r#"link(0..6 "/u" "t") link(7..12 "/u" "t") link(13..16 "/u" "t") 0..1@0 2..6@0 7..8@1 9..12@1 13..14@2 15..16@2"#
    );
    // Without the definition it is bracketed text.
    assert_eq!(para("[a][r]"), "");
}

// -- escapes, entities, breaks --------------------------------------------

#[test]
fn an_escape_conceals_its_backslash_only() {
    assert_eq!(para(r"\*a\*"), "0..1@0 3..4@1");
    assert_eq!(visible(BlockKind::Paragraph, r"\*a\*"), "*a*");
    // An escaped backslash shows one backslash.
    assert_eq!(para(r"a\\b"), "1..2@0");
    // A backslash before a letter is a character.
    assert_eq!(para(r"a\b"), "");
}

#[test]
fn an_entity_conceals_its_spelling_and_displays_what_it_decodes_to() {
    assert_eq!(
        para("&amp; &#35; &bogus; &#x1F600;"),
        r##"0..5@0="&" 6..11@1="#" 20..29@2="😀""##
    );
    // `&amp;amp;` is an entity followed by text.
    assert_eq!(para("&amp;amp;"), r#"0..5@0="&""#);
    assert_eq!(visible(BlockKind::Paragraph, "x &copy; y"), "x © y");
}

#[test]
fn hard_breaks_conceal_their_spelling() {
    assert_eq!(para("a  \nb"), "1..3@0 br(3)");
    assert_eq!(para("a\\\nb"), "1..2@0 br(2)");
    // comrak's node covers the last two spaces; all three are spelling.
    assert_eq!(para("a   \nb"), "1..4@0 br(4)");
    assert_eq!(para("a\nb"), "");
    assert_eq!(para("a \nb"), "");
}

#[test]
fn a_multi_line_paragraph_maps_every_line() {
    assert_eq!(para("a\n  b *c\nd* e"), "em(6..11) 6..7@0 10..11@0");
    assert_eq!(
        para("é\n*ü* `x`"),
        "em(2..5) code(6..9) 2..3@0 4..5@0 6..7@1 8..9@1"
    );
}

// -- atoms and underline --------------------------------------------------

#[test]
fn an_atom_placeholder_sits_inside_emphasis() {
    assert_eq!(para("*a\u{fffc}b*"), "em(0..5) 0..1@0 4..5@0");
    // The placeholder is punctuation, so it flanks the way `![x](y)` does.
    assert_eq!(para("*\u{fffc}*"), "em(0..3) 0..1@0 2..3@0");
    // …and so, like `**![x](y)**a`, does not close before a letter.
    assert_eq!(para("**\u{fffc}**a"), "");
    assert_eq!(para("**![x](y)**a"), "");
}

#[test]
fn paired_u_tags_underline_and_conceal_as_one_span() {
    assert_eq!(
        para("<u>a *b*</u>"),
        "underline(0..12) em(5..8) 0..3@0 5..6@1 7..8@1 8..12@0"
    );
    assert_eq!(para("<U >a</u>"), "underline(0..9) 0..4@0 5..9@0");
    // Unpaired, or paired across a span boundary: plain text.
    assert_eq!(para("<u>a"), "");
    assert_eq!(para("<u>*a</u>*"), "em(3..10) 3..4@0 9..10@0");
    assert_eq!(para("<span>a</span>"), "");
}

#[test]
fn paired_style_tags_style_and_conceal_as_one_span() {
    assert_eq!(para("<em>a</em>"), "em(0..10) 0..4@0 5..10@0");
    assert_eq!(
        para("<strong>a <del>b</del></strong>"),
        "strong(0..31) strikethrough(10..22) 0..8@0 10..15@1 16..22@1 22..31@0"
    );
    assert_eq!(
        para("<a href=\"/u?a&amp;b\" title='t'>x</a>"),
        r#"link(0..36 "/u?a&b" "t") 0..31@0 32..36@0"#
    );
    // A closing tag pairs with the latest opening tag of its own name.
    assert_eq!(para("<em>a<em>b</em>"), "em(5..15) 5..9@0 10..15@0");
    // An anchor with no destination, a tag closing itself and tags split
    // across a span boundary pair with nothing.
    assert_eq!(para("<a>x</a>"), "");
    assert_eq!(para("<em/>x</em>"), "");
    assert_eq!(para("<em>*a</em>*"), "em(4..12) 4..5@0 11..12@0");
}

#[test]
fn a_br_tag_ending_a_line_is_that_lines_hard_break() {
    assert_eq!(para("a<br>\nb"), "1..5@0 br(5)");
    assert_eq!(para("a<BR/> \nb"), "1..7@0 br(7)");
    assert_eq!(para("a<br>  \nb"), "5..7@0 br(7)", "two spaces spell it");
    // Mid-line, or before a break that is hard already, it is the atom.
    assert_eq!(para("a<br>b"), "");
    assert_eq!(para("a<br>\\\nb"), "5..6@0 br(6)");
    assert_eq!(atoms("a<br>b"), [r#"raw_inline(1..5 source=Str("<br>"))"#]);
    assert!(atoms("a<br>\nb").is_empty());
}

#[test]
fn an_img_tag_is_the_image_it_shows() {
    assert_eq!(
        atoms("a <img src=\"i.png\" alt='b' title=t> c"),
        [concat!(
            r#"image(2..35 alt=Str("b") source=Str("<img src=\"i.png\" alt='b' title=t>") "#,
            r#"src=Str("i.png") title=Str("t"))"#
        )]
    );
    // With nothing to show it stays the raw tag.
    assert_eq!(
        atoms("a <img alt=x>"),
        [r#"raw_inline(2..13 source=Str("<img alt=x>"))"#]
    );
}

#[test]
fn an_image_by_reference_is_no_atom() {
    let ctx = DeriveContext::new().with_definitions("[r]: /i.png");
    let derived = derive(BlockKind::Paragraph, "![a][r] ![r]", &ctx);
    assert!(derived.atoms.is_empty(), "{:?}", derived.atoms);
}

#[test]
fn a_spelled_image_or_wiki_link_styles_nothing_inside() {
    assert_eq!(para("*![a *b*](i.png)*"), "em(0..17) 0..1@0 16..17@0");
    assert_eq!(para("[[Note|*al*]]"), "");
}

/// The atoms [`derive`] reads out of `text`: `image(0..9 src="i" …)`.
fn atoms(text: &str) -> Vec<String> {
    derive(BlockKind::Paragraph, text, &DeriveContext::new())
        .atoms
        .iter()
        .map(|atom| {
            let mut attrs: Vec<String> = atom
                .attrs
                .iter()
                .map(|(name, value)| format!("{name}={value:?}"))
                .collect();
            attrs.sort();
            format!("{}({:?} {})", atom.node_type, atom.range, attrs.join(" "))
        })
        .collect()
}

#[test]
fn text_spelling_an_atom_is_reported_as_one() {
    assert_eq!(
        atoms("a ![b *c*](i.png \"t\") d"),
        [r#"image(2..21 alt=Str("b c") src=Str("i.png") title=Str("t"))"#]
    );
    assert_eq!(
        atoms("[[Note|al]] and ![[Pic]]"),
        [
            r#"wiki_link(0..11 alias=Str("al") embed=Bool(false) target=Str("Note"))"#,
            r#"wiki_link(16..24 alias=Str("") embed=Bool(true) target=Str("Pic"))"#,
        ]
    );
    assert_eq!(
        atoms("a <span class=\"x\">b</span> <!-- c -->"),
        [
            r#"raw_inline(2..18 source=Str("<span class=\"x\">"))"#,
            r#"raw_inline(19..26 source=Str("</span>"))"#,
            r#"raw_inline(27..37 source=Str("<!-- c -->"))"#,
        ]
    );
}

#[test]
fn u_tags_and_what_the_reader_refuses_are_no_atoms() {
    assert!(atoms("<u>a</u> <u>b").is_empty());
    assert!(atoms("[[a|]] `![x](y)` \\![x](y)").len() <= 1);
    assert!(atoms("[[a|]]").is_empty());
    assert!(atoms("`![x](y)`").is_empty());
    // An atom already in the tree is never folded into another.
    assert!(atoms("![a\u{fffc}](y)").is_empty());
}

#[test]
fn a_multi_line_link_tail_leaves_what_follows_in_place() {
    // A title over three lines.
    assert_eq!(
        para("[a](/u 'x\ny\nz') *b*\n*c*"),
        r#"link(0..15 "/u" "x\ny\nz") em(16..19) em(20..23) 0..1@0 2..15@0 16..17@1 18..19@1 20..21@2 22..23@2"#
    );
    // A destination on the next line.
    assert_eq!(
        para("[a](\n/u) *b*"),
        r#"link(0..8 "/u" "") em(9..12) 0..1@0 2..8@0 9..10@1 11..12@1"#
    );
    // A reference label over two lines.
    let ctx = DeriveContext::new().with_definitions("[r s]: /u");
    assert_eq!(
        read_with(BlockKind::Paragraph, "[a][r\ns] x\n*b*", &ctx),
        r#"link(0..8 "/u" "") em(11..14) 0..1@0 2..8@0 11..12@1 13..14@1"#
    );
    // An image, which is an atom, and a wiki link spanning lines.
    assert_eq!(para("![a](/u\n\"t\") *b*"), "em(13..16) 13..14@0 15..16@0");
    assert_eq!(atoms("![a](/u\n\"t\") *b*").len(), 1);
    assert_eq!(
        para("[[a\nb]] *c*\n*d*"),
        "em(8..11) em(12..15) 8..9@0 10..11@0 12..13@1 14..15@1"
    );
}

// -- guard ----------------------------------------------------------------

fn inserted(kind: BlockKind, text: &str) -> Vec<usize> {
    guard(kind, text).insertions
}

#[test]
fn guard_escapes_every_line_start_that_opens_a_block() {
    use BlockKind::Paragraph as P;
    for (text, expected) in [
        ("# a", vec![0]),
        ("###### a", vec![0]),
        ("a\n# b", vec![2]),
        ("- a", vec![0]),
        ("+ a", vec![0]),
        ("* a", vec![0]),
        ("> a", vec![0]),
        (">a", vec![0]),
        ("a\n   > b", vec![5]),
        ("1. a", vec![1]),
        ("1) a", vec![1]),
        ("a\n1. b", vec![3]),
        ("123456789. a", vec![9]),
        ("a\n===", vec![2]),
        ("a\n---", vec![2]),
        ("a\n-", vec![2]),
        ("***", vec![0]),
        ("___", vec![0]),
        ("a\n- - -", vec![2]),
        ("```", vec![0]),
        ("~~~ rust", vec![0]),
        ("a\n```\nb", vec![2]),
        ("<div>", vec![0]),
        ("a\n<div>", vec![2]),
        ("<!-- c", vec![0]),
        ("[a]: /u", vec![0]),
        ("[a]: /u\n[b]: /v\nc", vec![0]),
        ("a|b\n-|-", vec![4]),
        ("x\na|b\n-|-", vec![6]),
        ("a\n- b\n- c", vec![2, 6]),
    ] {
        assert_eq!(inserted(P, text), expected, "{text:?}");
    }
}

#[test]
fn guard_leaves_what_cannot_open_a_block_there() {
    use BlockKind::Paragraph as P;
    for text in [
        "a",
        "a # b",
        "#a",
        "-a",
        "a\n2. b",
        "a\n    b",
        "10000000000. a",
        "a\n*",
        "a\n<span>",
        "1.a",
        "a | b",
        r"\# a",
        "a\n\\- b",
    ] {
        assert_eq!(inserted(P, text), Vec::<usize>::new(), "{text:?}");
    }
}

#[test]
fn guard_cannot_protect_leading_indentation_or_a_blank_line() {
    use BlockKind::Paragraph as P;
    // No backslash makes whitespace literal; see the guard module.
    assert_eq!(inserted(P, "    a"), Vec::<usize>::new());
    assert_eq!(inserted(P, "a\n\nb"), Vec::<usize>::new());
    // A later line that can be protected still is.
    assert_eq!(inserted(P, "a\n\nb\n# c"), vec![5]);
    // Indentation before a block start is skipped over.
    assert_eq!(inserted(P, "  # a"), vec![2]);
}

#[test]
fn guard_escapes_a_heading_closing_sequence() {
    use BlockKind::Heading as H;
    assert_eq!(inserted(H, "a #"), vec![2]);
    assert_eq!(inserted(H, "a ##  "), vec![2]);
    assert_eq!(inserted(H, "#"), vec![0]);
    assert_eq!(inserted(H, "a#"), Vec::<usize>::new());
    assert_eq!(inserted(H, r"a \#"), Vec::<usize>::new());
    // A heading's content may start like a block; only its end is special.
    assert_eq!(inserted(H, "- a"), Vec::<usize>::new());
    // A setext heading's lines are guarded like a paragraph's.
    assert_eq!(inserted(H, "a\n# b"), vec![2]);
}

#[test]
fn guard_escapes_every_unescaped_pipe_in_a_cell() {
    use BlockKind::TableCell as C;
    assert_eq!(inserted(C, "a|b"), vec![1]);
    assert_eq!(inserted(C, r"a\|b"), Vec::<usize>::new());
    assert_eq!(inserted(C, r"a\\|b"), vec![3]);
    // A code span protects nothing: the row is split first.
    assert_eq!(inserted(C, "`a|b`"), vec![2]);
    assert_eq!(guard(C, "`a|b`").text, r"`a\|b`");
}

#[test]
fn guarding_guarded_text_inserts_nothing() {
    for (kind, text) in [
        (BlockKind::Paragraph, "# a\n- b\n1. c\n===\na|b\n-|-"),
        (BlockKind::Heading, "a ##"),
        (BlockKind::TableCell, r"a|b\\|c"),
    ] {
        let once = guard(kind, text);
        assert!(!once.insertions.is_empty(), "{text:?}");
        assert_eq!(
            guard(kind, &once.text).insertions,
            Vec::<usize>::new(),
            "{text:?}"
        );
    }
}

#[test]
fn guarded_offsets_map_both_ways() {
    let guarded = guard(BlockKind::Paragraph, "# a\n- b");
    assert_eq!(guarded.text, "\\# a\n\\- b");
    assert_eq!(guarded.insertions, vec![0, 4]);
    assert_eq!(guarded.to_guarded(0), 1);
    assert_eq!(guarded.to_guarded(3), 4);
    assert_eq!(guarded.to_guarded(4), 6);
    assert_eq!(guarded.to_guarded(7), 9);
    assert_eq!(guarded.to_original(0), 0);
    assert_eq!(guarded.to_original(1), 0);
    assert_eq!(guarded.to_original(5), 4);
    assert_eq!(guarded.to_original(6), 4);
    assert_eq!(guarded.to_original(9), 7);
}

// -- guarded derive -------------------------------------------------------

#[test]
fn derive_reads_text_the_guard_protects_without_reporting_its_backslashes() {
    assert_eq!(para("# *a*"), "em(2..5) 2..3@0 4..5@0");
    assert_eq!(visible(BlockKind::Paragraph, "# *a*"), "# a");
    assert_eq!(para("a\n- *b*"), "em(4..7) 4..5@0 6..7@0");
    assert_eq!(para("1. `x`"), "code(3..6) 3..4@0 5..6@0");
}

#[test]
fn derive_reads_leading_whitespace_as_absent() {
    assert_eq!(para("    *a*"), "em(4..7) 4..5@0 6..7@0");
}

#[test]
fn a_blank_line_does_not_stop_the_reading() {
    // Not a paragraph any file can hold; derive still says what each part is.
    assert_eq!(
        para("*a*\n\n*b*"),
        "em(0..3) em(5..8) 0..1@0 2..3@0 5..6@1 7..8@1"
    );
}

#[test]
fn a_heading_is_read_after_its_marker() {
    use BlockKind::Heading as H;
    assert_eq!(read(H, "a *b*"), "em(2..5) 2..3@0 4..5@0");
    assert_eq!(read(H, "*a* #"), "em(0..3) 0..1@0 2..3@0");
    assert_eq!(visible(H, "*a* #"), "a #");
    assert_eq!(read(H, "- *a*"), "em(2..5) 2..3@0 4..5@0");
    // Two lines: a setext heading.
    assert_eq!(read(H, "a *b\nc*"), "em(2..7) 2..3@0 6..7@0");
}

#[test]
fn a_cell_maps_positions_past_its_escaped_pipes() {
    use BlockKind::TableCell as C;
    // comrak reports positions in the cell with each `\|` shortened to `|`;
    // without the replay the code span would land one character early. The
    // backslash of each `\|` is a concealed escape, inside a code span too.
    assert_eq!(
        read(C, r"a \| b `c\|d` [x](u)"),
        r#"code(7..13) link(14..20 "u" "") 2..3@0 7..8@1 9..10@2 12..13@1 14..15@3 16..20@3"#
    );
    assert_eq!(visible(C, r"a \| b `c\|d`"), "a | b c|d");
}

#[test]
fn a_cell_guards_its_own_pipes() {
    use BlockKind::TableCell as C;
    assert_eq!(
        read(C, "`a|b` *c*"),
        "code(0..5) em(6..9) 0..1@0 4..5@0 6..7@1 8..9@1"
    );
    assert_eq!(read(C, "# *a*"), "em(2..5) 2..3@0 4..5@0");
    assert_eq!(read(C, "- a"), "");
}
