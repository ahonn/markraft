use super::*;

fn options(base: Option<PathBuf>) -> Options {
    Options {
        title: "Trip <notes>".into(),
        base,
        image_root: markraft_media::ImageRoot::None,
        remote_images: false,
        assets: Default::default(),
        auto_number_equations: true,
        i18n: I18n::english(),
    }
}

fn export(markdown: &str, profile: Profile) -> String {
    render(
        &crate::doc::from_markdown(markdown),
        profile,
        &options(None),
    )
}

#[test]
fn a_page_is_a_complete_document_following_the_reader_appearance() {
    let page = export("# Hello\n\nWorld", Profile::Page);
    assert!(
        page.starts_with("<!doctype html>\n<html lang=\"en\">"),
        "{page}"
    );
    assert!(page.contains("<title>Trip &lt;notes&gt;</title>"), "{page}");
    assert!(page.contains("<meta name=\"color-scheme\" content=\"light dark\">"));
    assert!(page.contains("@media (prefers-color-scheme: dark)"));
    assert!(page.contains("<h1>Hello</h1>\n<p>World</p>"), "{page}");
    assert!(!page.contains("break-inside"));
}

#[test]
fn a_print_page_is_light_and_keeps_blocks_whole() {
    let page = export("Hello", Profile::Print);
    assert!(page.contains("<meta name=\"color-scheme\" content=\"light\">"));
    assert!(!page.contains("prefers-color-scheme"));
    assert!(page.contains("break-inside: avoid"));
    assert!(page.contains("print-color-adjust: exact"));
}

#[test]
fn fragments_carry_no_page_around_them() {
    assert_eq!(
        export("Hello **there**", Profile::RichText),
        "<p>Hello <strong>there</strong></p>"
    );
}

#[test]
fn code_is_coloured_by_class_on_a_page_and_inline_in_rich_text() {
    let source = "```rust\nfn main() {}\n```";
    let page = export(source, Profile::Page);
    assert!(
        page.contains("<pre><code class=\"language-rust\"><span class=\"code-keyword\">fn</span>"),
        "{page}"
    );
    let rich = export(source, Profile::RichText);
    assert!(
        rich.contains("<span style=\"color:#7a5af8\">fn</span>"),
        "{rich}"
    );
}

#[test]
fn formulas_are_typeset_with_their_numbers_and_resolved_references() {
    let source = "Energy:\n\n$$\nE=mc^2 \\label{e}\n$$\n\nBy $\\eqref{e}$ and $x^2$.";
    let page = export(source, Profile::Page);
    assert_eq!(page.matches("<svg").count(), 4, "{page}");
    assert!(page.contains("<span class=\"math display\"><svg"), "{page}");
    assert!(page.contains("<span class=\"math-tag\"><svg"), "{page}");
    assert!(!page.contains("\\label"), "{page}");
    let rich = export(source, Profile::RichText);
    assert_eq!(
        rich.matches("<img src=\"data:image/png;base64,").count(),
        4,
        "{rich}"
    );
    assert!(!rich.contains("<svg"));
}

#[test]
fn a_formula_that_does_not_typeset_stays_as_its_source() {
    let page = export("Bad $\\frac{$ math", Profile::Page);
    assert!(page.contains("<code>\\frac{</code>"), "{page}");
}

#[test]
fn callouts_read_their_title_or_their_type_in_the_interface_language() {
    let page = export("> [!warning]\n> Careful", Profile::Page);
    assert!(
        page.contains("<aside class=\"callout callout-caution\">\n<p class=\"callout-title\">Warning</p>\n<p>Careful</p>\n</aside>"),
        "{page}"
    );
    let named = export("> [!tip] Remember\n> this", Profile::RichText);
    assert!(named.starts_with("<blockquote style="), "{named}");
    assert!(
        named.contains(">Remember</p>\n<p>this</p>\n</blockquote>"),
        "{named}"
    );
    let custom = export("> [!recipe]\n> eggs", Profile::Page);
    assert!(custom.contains(">Recipe</p>"), "{custom}");
    let quote = export("> plain", Profile::Page);
    assert!(
        quote.contains("<blockquote>\n<p>plain</p>\n</blockquote>"),
        "{quote}"
    );
}

#[test]
fn wiki_links_read_as_their_label_without_pointing_anywhere() {
    let page = export("See [[Trip|the trip]] and [[Home]]", Profile::Page);
    assert!(
        page.contains("See <span class=\"wiki-link\">the trip</span> and <span class=\"wiki-link\">Home</span>"),
        "{page}"
    );
    let rich = export("See [[Trip|the trip]]", Profile::RichText);
    assert!(rich.starts_with("<p>See <span style=\"color:#"), "{rich}");
}

#[test]
fn local_pictures_are_embedded_and_missing_ones_leave_their_alt_text() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dot.png"), b"\x89PNG").unwrap();
    let doc =
        crate::doc::from_markdown("![dot](dot.png) ![gone](gone.png) ![web](https://e.com/x.png)");
    let html = render(
        &doc,
        Profile::RichText,
        &options(Some(dir.path().to_owned())),
    );
    assert_eq!(
        html,
        "<p><img src=\"data:image/png;base64,iVBORw==\" alt=\"dot\"> gone web</p>"
    );
}

#[test]
fn tasks_keep_their_state_in_every_profile() {
    let source = "- [x] done\n- [ ] open";
    for profile in [Profile::Page, Profile::Print] {
        let page = export(source, profile);
        assert!(
            page.contains(
                "<li class=\"task done\"><input type=\"checkbox\" disabled checked>done</li>"
            ) && page.contains("<li class=\"task\"><input type=\"checkbox\" disabled>open</li>"),
            "{page}"
        );
    }
    let rich = export(source, Profile::RichText);
    assert!(
        rich.contains(
            "<li style=\"list-style:none;color:#686b71;text-decoration:line-through\">☑ done</li>"
        ),
        "{rich}"
    );
    assert!(rich.contains(">☐ open</li>"), "{rich}");
}

#[test]
fn rich_text_keeps_lists_in_separate_elements() {
    assert_eq!(
        export("- a\n\n1. b", Profile::RichText),
        "<ul>\n<li><p>a</p></li>\n</ul>\n<ol>\n<li><p>b</p></li>\n</ol>"
    );
}

#[test]
fn an_html_block_renders_as_the_editor_draws_it_or_stays_source() {
    let shown = export("<p align=\"center\"><b>Hi</b></p>", Profile::Page);
    assert!(
        shown.contains("<div style=\"text-align:center\"><p><strong>Hi</strong></p></div>"),
        "{shown}"
    );
    let kept = export("<script>alert(1)</script>", Profile::Page);
    assert!(
        kept.contains("<pre class=\"raw\">&lt;script&gt;alert(1)&lt;/script&gt;</pre>"),
        "{kept}"
    );
}

#[test]
fn rich_text_tables_carry_their_grid_inline() {
    let rich = export("| A | B |\n|---|--:|\n| 1 | 2 |", Profile::RichText);
    assert!(
        rich.contains("<table style=\"border-collapse:collapse\">"),
        "{rich}"
    );
    assert_eq!(rich.matches("border:1px solid").count(), 4, "{rich}");
    assert!(
        rich.contains("padding:4px 10px\" align=\"right\">2</td>"),
        "{rich}"
    );
}

#[test]
fn touching_formulas_are_typeset_apart() {
    let page = export("$a$$b$", Profile::Page);
    assert_eq!(
        page.matches("<span class=\"math\"><svg").count(),
        2,
        "{page}"
    );
}

#[test]
fn a_formula_in_an_html_block_never_borrows_a_number() {
    // The HTML block renders to a tree of its own; a formula there must not
    // take the number of the equation at the same position in the note.
    let source = "<p><code data-math-style=\"display\">y=1</code></p>\n\n$$\nE=mc^2\n$$";
    let page = export(source, Profile::Page);
    assert_eq!(page.matches("<svg").count(), 3, "{page}");
    assert_eq!(
        page.matches("<span class=\"math-tag\">").count(),
        1,
        "{page}"
    );
}

#[test]
fn a_callout_heading_follows_the_editor_rule() {
    // English names no type differently from the author, so the spelling stays.
    let page = export("> [!NOTE]\n> body", Profile::Page);
    assert!(
        page.contains("<p class=\"callout-title\">NOTE</p>"),
        "{page}"
    );
    let chinese = Options {
        i18n: I18n::for_preference(&crate::locale::LanguagePreference::Locale("zh-Hans".into())),
        ..options(None)
    };
    let doc = crate::doc::from_markdown("> [!NOTE]\n> body");
    let page = render(&doc, Profile::Page, &chinese);
    assert!(!page.contains(">NOTE</p>"), "{page}");
}

#[test]
fn formulas_in_tables_and_tasks_keep_their_numbers_and_references() {
    let source = "$$\nE=mc^2 \\label{e}\n$$\n\n\
        | ref | x |\n|---|---|\n| $\\eqref{e}$ | $x$ |\n\n\
        - [ ] By $\\eqref{e}$\n\n\
          $$\n  F=ma\n  $$";
    let page = export(source, Profile::Page);
    assert!(
        !page.contains("\\eqref"),
        "a reference stayed source: {page}"
    );
    // Both standalone formulas are numbered, the nested one included.
    assert_eq!(
        page.matches("<span class=\"math-tag\">").count(),
        2,
        "{page}"
    );
}

#[test]
fn an_embedded_picture_is_a_picture_and_an_embedded_note_its_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dot.png"), b"\x89PNG").unwrap();
    let doc = crate::doc::from_markdown("![[dot.png]] and ![[Other note]]");
    let html = render(
        &doc,
        Profile::RichText,
        &options(Some(dir.path().to_owned())),
    );
    assert!(
        html.starts_with("<p><img src=\"data:image/png;base64,iVBORw==\" alt=\"dot.png\"> and <span style=\"color:#"),
        "{html}"
    );
    assert!(html.ends_with("\">Other note</span></p>"), "{html}");
}

#[test]
fn database_assets_export_without_allowing_network_images() {
    let source = "markraft-asset:one";
    let mut options = options(None);
    let bytes = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec();
    options.assets.insert(
        source.into(),
        markraft_notes::Asset {
            id: markraft_notes::AssetId("one".into()),
            media_type: "image/svg+xml".into(),
            bytes,
        },
    );
    let document = crate::doc::from_markdown(
        "![saved](markraft-asset:one)\n\n![remote](https://example.com/image.png)",
    );
    for profile in [Profile::Page, Profile::Print, Profile::RichText] {
        let rendered = render(&document, profile, &options);
        assert!(rendered.contains("data:image/svg+xml;base64,"));
        assert!(!rendered.contains("src=\"https://example.com"));
    }
}
