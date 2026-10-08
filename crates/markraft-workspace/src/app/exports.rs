//! The note on screen, taken somewhere else: a file (Markdown, HTML or PDF),
//! paper, the clipboard as rich text, or an Obsidian vault.
//!
//! Every way out takes one road. [`MarkraftApp::deliver`] captures the
//! committed note ([`ExportInput`]), asks for a file where the way out writes
//! one, produces the result on a background thread, and hands it back on the
//! UI thread. A way out says only what it produces and what it does with it.

use super::*;
use crate::export::{Options, Profile};
use markraft_commonmark::SourceSnapshot;

/// The file a note is exported as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExportFormat {
    Markdown,
    Html,
}

impl ExportFormat {
    fn extension(self) -> &'static str {
        match self {
            ExportFormat::Markdown => "md",
            ExportFormat::Html => "html",
        }
    }
}

/// The note as every way out reads it: committed, with its pictures located.
pub(super) struct ExportInput {
    pub(super) document: Node,
    /// The note's Markdown as it would be saved now.
    pub(super) markdown: String,
    /// The file's bytes as read, where the note has a file.
    pub(super) source: Option<SourceSnapshot>,
    /// The Markdown style new syntax is spelled in.
    pub(super) house: markraft_commonmark::HouseStyleHandle,
    pub(super) options: Options,
}

impl ExportInput {
    /// Export database attachments beside the selected Markdown file.
    fn markdown_with_assets(
        &self,
        path: &std::path::Path,
    ) -> Result<(String, tempfile::TempDir), StoreError> {
        let parent = path
            .parent()
            .ok_or_else(|| StoreError::from("Export directory is unavailable".to_owned()))?;
        let folder = tempfile::Builder::new()
            .prefix("note-assets-")
            .tempdir_in(parent)
            .map_err(|error| StoreError::from(error.to_string()))?;
        let mut references = std::collections::HashMap::new();
        for (source, asset) in &self.options.assets {
            let kind = markraft_media::ImageType::sniff(&asset.bytes)
                .ok_or_else(|| StoreError::Invalid(Message::new("asset.images-only")))?;
            let extension = match kind {
                markraft_media::ImageType::Png => "png",
                markraft_media::ImageType::Jpeg => "jpg",
                markraft_media::ImageType::Gif => "gif",
                markraft_media::ImageType::Webp => "webp",
                markraft_media::ImageType::Svg => "svg",
                markraft_media::ImageType::Bmp => "bmp",
                markraft_media::ImageType::Tiff => "tiff",
                markraft_media::ImageType::Ico => "ico",
            };
            let target = folder
                .path()
                .join(format!("{}.{}", uuid::Uuid::new_v4(), extension));
            crate::fs::atomic_write_shared(&target, &asset.bytes)?;
            references.insert(
                source.clone(),
                markraft_media::relative_url(parent, &target),
            );
        }
        let document = assets::relink_backend_sources(&self.document, &references);
        let markdown = match &self.source {
            Some(source) => source
                .render(doc::schema(), &document)
                .map_err(|error| StoreError::from(error.to_string())),
            None => Ok(format!("{}\n", doc::to_markdown_in(&document, &self.house))),
        }?;
        Ok((markdown, folder))
    }

    /// The note as `profile` reads it.
    fn render(&self, profile: Profile) -> String {
        crate::export::render(&self.document, profile, &self.options)
    }
}

impl MarkraftApp {
    /// The active note as an export reads it, once the persistence worker has
    /// captured it. Without a folder there is nothing on disk to capture, and
    /// the note is taken as it stands.
    fn export_input(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl std::future::Future<Output = Result<ExportInput, StoreError>> + 'static {
        self.sync_documents(cx);
        let note = self.notes.library.active_note().clone();
        let options = Options {
            title: note.title(),
            base: None,
            image_root: markraft_media::ImageRoot::None,
            remote_images: self.preferences.remote_images,
            assets: Default::default(),
            auto_number_equations: self.preferences.auto_number_equations,
            i18n: self.i18n.clone(),
        };
        let note_path = note.path.clone();
        let rendering = self.notes.persistence.as_ref().map(|persistence| {
            persistence.snapshot_async(
                note.clone(),
                self.notes.library.generation(),
                self.preferences.auto_number_equations,
            )
        });
        let house = self.house.clone();
        let asset_reader = self
            .notes
            .persistence
            .as_ref()
            .filter(|persistence| persistence.capabilities().assets)
            .map(|persistence| persistence.asset_reader());
        async move {
            let Some(rendering) = rendering else {
                return Ok(ExportInput {
                    markdown: format!("{}\n", doc::to_markdown_in(&note.document, &house)),
                    document: note.document,
                    source: None,
                    house,
                    options,
                });
            };
            let snapshot = rendering.await?;
            let mut captured_assets = std::collections::HashMap::new();
            if let Some(read) = asset_reader {
                for source in assets::backend_sources(&snapshot.document) {
                    let id =
                        markraft_notes::AssetId::from_source(&source).expect("asset reference");
                    let asset = read(id).await?;
                    if asset.bytes.len() as u64 > markraft_media::MAX_IMAGE_BYTES {
                        return Err(StoreError::Invalid(Message::new("asset.too-large")));
                    }
                    if markraft_media::ImageType::sniff(&asset.bytes).is_none() {
                        return Err(StoreError::Invalid(Message::new("asset.images-only")));
                    }
                    captured_assets.insert(source, asset);
                }
            }
            // A root the note names but that cannot be used refuses absolute
            // paths, as it does in the editor, rather than reading them from `/`.
            let image_root = match note_path
                .as_deref()
                .map(|note| assets::image_root(&snapshot.markdown, note))
            {
                Some(Ok(Some(root))) => markraft_media::ImageRoot::At(root),
                Some(Err(_)) => markraft_media::ImageRoot::Unusable,
                Some(Ok(None)) | None => markraft_media::ImageRoot::None,
            };
            Ok(ExportInput {
                options: Options {
                    base: snapshot.base_path.clone(),
                    image_root,
                    assets: captured_assets,
                    ..options
                },
                document: snapshot.document,
                markdown: snapshot.markdown,
                source: snapshot.source,
                house,
            })
        }
    }

    /// Capture the note, ask for a file when `save_as` names one, run `produce`
    /// on a background thread, and hand its result to `done`. A cancelled save
    /// panel ends it quietly; an error is shown.
    fn deliver<T: Send + 'static>(
        &mut self,
        save_as: Option<String>,
        produce: impl FnOnce(ExportInput, Option<PathBuf>) -> Result<T, StoreError> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.export_input(cx);
        let prompt = save_as.map(|name| {
            let directory = self.path.clone().unwrap_or_default();
            let panel = cx.prompt_for_new_path(&directory, Some(&name));
            self.file_panel(panel, window, cx)
        });
        // The save panel asks before replacing a file, but an export never
        // replaces a note: that would lose it.
        let notes: Vec<PathBuf> = self
            .notes
            .library
            .notes
            .iter()
            .filter_map(|note| note.path.clone())
            .collect();
        let executor = cx.background_executor().clone();
        // Only capturing the note is workspace I/O. The save panel and the work
        // after it take as long as the person, or another app, takes, and must
        // not hold up reloading or renaming meanwhile.
        self.run_io(input, window, cx, move |this, input, window, cx| {
            let input = match input {
                Ok(input) => input,
                Err(error) => {
                    this.feedback.set_error(error);
                    return;
                }
            };
            cx.spawn_in(window, async move |this, cx| {
                let outcome: Result<Option<T>, StoreError> = async {
                    let path = match prompt {
                        None => None,
                        Some(prompt) => {
                            let Some(path) = prompt
                                .await
                                .map_err(|e| StoreError::from(e.to_string()))?
                                .map_err(|e| StoreError::from(e.to_string()))?
                            else {
                                return Ok(None);
                            };
                            if notes
                                .iter()
                                .any(|note| crate::fs::same_regular_file(note, &path))
                            {
                                return Err(StoreError::Invalid(
                                    Message::new("error.export-over-note")
                                        .arg("name", crate::fs::file_label(&path)),
                                ));
                            }
                            Some(path)
                        }
                    };
                    executor
                        .spawn(async move { produce(input, path).map(Some) })
                        .await
                }
                .await;
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        match outcome {
                            Ok(Some(value)) => done(this, value, window, cx),
                            Ok(None) => {}
                            Err(error) => this.feedback.set_error(error),
                        }
                        cx.notify();
                    })
                });
            })
            .detach();
        });
    }

    /// A notice that `path` was written, with Show in Finder.
    fn exported(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = crate::fs::file_label(&path);
        self.feedback
            .inform_with_reveal(Message::new("notice.exported").arg("name", name), path);
        cx.notify();
    }

    pub(super) fn export(
        &mut self,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .notes
            .library
            .active_note()
            .file_name(format.extension());
        self.deliver(
            Some(name),
            move |input, path| {
                let path = path.expect("a file is asked for");
                let mut exported_assets = None;
                let bytes = match format {
                    ExportFormat::Markdown => {
                        if input.options.assets.is_empty() {
                            input.markdown.into_bytes()
                        } else {
                            let (markdown, directory) = input.markdown_with_assets(&path)?;
                            exported_assets = Some(directory);
                            markdown.into_bytes()
                        }
                    }
                    ExportFormat::Html => input.render(Profile::Page).into_bytes(),
                };
                crate::fs::atomic_write_shared(&path, &bytes)?;
                if let Some(directory) = exported_assets {
                    let _ = directory.keep();
                }
                Ok(path)
            },
            |this, path, _, cx| this.exported(path, cx),
            window,
            cx,
        );
    }

    /// The note on paper through the print panel, or as a PDF when `to_pdf`.
    pub(super) fn print_note(&mut self, to_pdf: bool, window: &mut Window, cx: &mut Context<Self>) {
        let name = to_pdf.then(|| self.notes.library.active_note().file_name("pdf"));
        self.deliver(
            name,
            |input, path| Ok((input.render(Profile::Print), path)),
            |this, (html, path), window, cx| this.run_print(&html, path, window, cx),
            window,
            cx,
        );
    }

    fn run_print(
        &mut self,
        html: &str,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::platform::print::{Destination, print_html};
        let destination = match &path {
            Some(path) => Destination::Pdf(path.clone()),
            None => Destination::Panel,
        };
        let receiver = match print_html(window, html, destination) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.feedback.set_error(error);
                cx.notify();
                return;
            }
        };
        cx.spawn_in(window, async move |this, cx| {
            let outcome = receiver.await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(true)) => {
                        if let Some(path) = path {
                            this.exported(path, cx);
                        }
                    }
                    // With no panel there is nothing to cancel: a PDF that did
                    // not print was not written.
                    Ok(Ok(false)) if path.is_some() => {
                        let name = path
                            .as_deref()
                            .map(crate::fs::file_label)
                            .unwrap_or_default();
                        this.feedback
                            .set_error(Message::new("error.pdf-failed").arg("name", name));
                    }
                    // Cancelled in the panel, or the job was dropped.
                    Ok(Ok(false)) | Err(_) => {}
                    Ok(Err(detail)) => this
                        .feedback
                        .set_error(Message::new("error.print-failed").arg("detail", detail)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The whole note on the clipboard as rich text, with its plain text beside it.
    pub(super) fn copy_rich_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.deliver(
            None,
            |input, _| {
                let html = input.render(Profile::RichText);
                Ok((doc::plain_text(&input.document), html, input.document))
            },
            |this, (text, html, document), _, cx| {
                let codecs = doc::codecs(&this.house);
                markraft_gpui::write_rich_text(
                    doc::schema(),
                    codecs.as_ref(),
                    &document,
                    text,
                    &html,
                    cx,
                );
                this.inform(Message::new("notice.copied-rich-text"), cx);
            },
            window,
            cx,
        );
    }

    /// User-selected vaults carry the sandbox grant needed to write the copy.
    pub(super) fn choose_obsidian_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.i18n.text("dialog.use-folder").into()),
        });
        let prompt = self.file_panel(prompt, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = prompt.await
                && let Some(selected) = paths.pop()
            {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        let granted = this.file_access.remember(&selected).and_then(|()| {
                            crate::send::obsidian::validate_selected_vault(&selected)
                        });
                        if let Err(error) = granted {
                            this.feedback.set_error(error);
                            cx.notify();
                            return;
                        }
                        this.send_to_obsidian(selected, window, cx);
                    })
                });
            }
        })
        .detach();
    }

    /// A copy of the note, with its pictures, as a new note in `vault`, opened
    /// there.
    pub(super) fn send_to_obsidian(
        &mut self,
        vault: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.notes.library.active_note().file_stem();
        self.deliver(
            None,
            move |input, _| {
                if !input.options.assets.is_empty() {
                    let staging =
                        tempfile::tempdir().map_err(|error| StoreError::from(error.to_string()))?;
                    let (markdown, _attachments) =
                        input.markdown_with_assets(&staging.path().join("note.md"))?;
                    let origin =
                        markraft_commonmark::SourceDocument::parse(doc::schema(), &markdown)
                            .map_err(|error| StoreError::from(error.to_string()))?;
                    let source = markraft_commonmark::SourceTrack::new(origin.clone()).snapshot();
                    return crate::send::obsidian::send(
                        &vault,
                        &crate::send::obsidian::Note {
                            name: &name,
                            document: origin.document(),
                            markdown: &markdown,
                            source: Some(&source),
                            house: &input.house,
                            base: Some(staging.path()),
                            root: markraft_media::Root::None,
                        },
                    );
                }
                let note = crate::send::obsidian::Note {
                    name: &name,
                    document: &input.document,
                    markdown: &input.markdown,
                    source: input.source.as_ref(),
                    house: &input.house,
                    base: input.options.base.as_deref(),
                    root: input.options.image_root.as_root(),
                };
                crate::send::obsidian::send(&vault, &note)
            },
            |this, path, _, cx| {
                cx.open_url(&crate::send::obsidian::open_url(&path));
                let name = crate::fs::file_label(&path);
                this.inform(
                    Message::new("notice.sent-to-obsidian").arg("name", name),
                    cx,
                );
            },
            window,
            cx,
        );
    }
}

#[cfg(all(test, feature = "mac-app-store"))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod store_tests {
    use crate::e2e::harness::{Harness, open_with};
    use gpui::TestAppContext;

    fn choose_vault(h: &mut Harness) {
        h.keys("cmd-k");
        h.type_text("Obsidian");
        assert_eq!(
            h.app.update(h.cx, |app, cx| app.test_action_labels(cx)),
            Some(vec!["Send to Obsidian".into()])
        );
        h.keys("enter");
        assert!(h.cx.did_prompt_for_paths());
    }

    #[gpui::test]
    fn store_obsidian_send_requires_an_explicit_valid_vault_selection(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("alpha.md", "A note for Obsidian\n")], |_| {});
        let vault = h.root().join("Vault");
        std::fs::create_dir(&vault).unwrap();

        choose_vault(&mut h);
        h.cx.simulate_path_prompt_response(|options| {
            assert!(!options.files && options.directories && !options.multiple);
            None
        });
        h.cx.run_until_parked();
        assert!(!vault.join("alpha.md").exists());

        choose_vault(&mut h);
        h.cx.simulate_path_prompt_response(|_| Some(vec![vault.clone()]));
        h.cx.run_until_parked();
        h.wait_for_io();
        assert!(!vault.join("alpha.md").exists());

        std::fs::create_dir(vault.join(".obsidian")).unwrap();
        choose_vault(&mut h);
        h.cx.simulate_path_prompt_response(|_| Some(vec![vault.clone()]));
        h.cx.run_until_parked();
        h.wait_until(|_| vault.join("alpha.md").is_file());
        assert_eq!(
            std::fs::read_to_string(vault.join("alpha.md")).unwrap(),
            "A note for Obsidian\n"
        );
    }
}

#[cfg(test)]
mod backend_asset_tests {
    use super::{ExportInput, Options, doc};

    #[test]
    fn markdown_export_materializes_assets_and_preserves_untouched_source() {
        let source =
            "---\ntitle: keep\n---\n\nSetext title\n============\n\n![image](markraft-asset:one)\n";
        let origin = markraft_commonmark::SourceDocument::parse(doc::schema(), source).unwrap();
        let input = ExportInput {
            document: origin.document().clone(),
            markdown: source.into(),
            source: Some(markraft_commonmark::SourceTrack::new(origin).snapshot()),
            house: Default::default(),
            options: Options {
                title: "Keep".into(),
                base: None,
                image_root: markraft_media::ImageRoot::None,
                remote_images: false,
                auto_number_equations: false,
                i18n: crate::locale::I18n::english(),
                assets: [(
                    "markraft-asset:one".into(),
                    markraft_notes::Asset {
                        id: markraft_notes::AssetId("one".into()),
                        media_type: "image/svg+xml".into(),
                        bytes: b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
                    },
                )]
                .into(),
            },
        };
        let destination = tempfile::tempdir().unwrap();
        let (markdown, assets) = input
            .markdown_with_assets(&destination.path().join("note.md"))
            .unwrap();
        assert!(markdown.starts_with("---\ntitle: keep\n---\n\nSetext title\n============\n"));
        assert!(!markdown.contains("markraft-asset:"));
        let attachment = std::fs::read_dir(assets.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(markdown.contains(attachment.file_name().unwrap().to_str().unwrap()));
        assert_eq!(
            std::fs::read(&attachment).unwrap(),
            input.options.assets["markraft-asset:one"].bytes
        );
        drop(assets);
        assert!(
            !attachment.exists(),
            "Uncommitted exports clean their staged attachments"
        );
    }
}
