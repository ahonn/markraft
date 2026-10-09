//! Atomic saves need a directory grant to create and replace a sibling file.
use super::*;
use std::path::Path;

/// Keep authorization separate from opening so canceling any panel leaves the
/// whole selection unopened. An ancestor directory can cover several files.
impl WorkspaceView {
    pub(super) fn authorize_open_paths(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.notes.persistence.is_none() {
            self.inform(Message::new("notice.folder-before-files"), cx);
            return;
        }
        if let Some(file) =
            first_uncovered_file(&paths, |parent| self.file_access.has_write_access(parent))
                .map(Path::to_owned)
        {
            self.request_open_parent(paths, file, window, cx);
        } else {
            self.open_authorized_paths(paths, window, cx);
        }
    }

    fn request_open_parent(
        &mut self,
        paths: Vec<PathBuf>,
        file: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = parent_directory(&file);
        let name = crate::fs::file_label(&file);
        let answer = window.prompt(
            PromptLevel::Info,
            &self
                .i18n
                .text_with("dialog.allow-file-saving", &[("name", &name)]),
            Some(&self.i18n.text_with(
                "dialog.file-folder-access",
                &[("folder", &parent.display().to_string())],
            )),
            &[
                self.i18n.text("dialog.cancel").as_str(),
                self.i18n.text("dialog.choose-folder-access").as_str(),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(1) {
                return;
            }
            let Ok(Ok(prompt)) = cx.update(|window, cx| {
                this.update(cx, |this, cx| {
                    let prompt = cx.prompt_for_paths(PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: Some(this.i18n.text("dialog.use-folder").into()),
                    });
                    this.file_panel(prompt, window, cx)
                })
            }) else {
                return;
            };
            if let Ok(Ok(Some(mut selected))) = prompt.await
                && let Some(directory) = selected.pop()
            {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        if !directory_contains_parent(&directory, &parent) {
                            this.inform(
                                Message::new("error.file-parent-folder")
                                    .arg("folder", parent.display().to_string()),
                                cx,
                            );
                            return;
                        }
                        if let Err(error) = this.file_access.remember(&directory) {
                            this.inform(error, cx);
                            return;
                        }
                        this.authorize_open_paths(paths, window, cx);
                    })
                });
            }
        })
        .detach();
    }
}

fn parent_directory(file: &Path) -> PathBuf {
    // Persistence opens the canonical file. A symlink's directory grant cannot
    // authorize atomic replacement beside a target in another directory.
    let resolved = file.canonicalize().unwrap_or_else(|_| file.to_owned());
    let file = resolved.as_path();
    let absolute = if file.is_absolute() {
        file.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(file)
    };
    absolute.parent().unwrap_or(&absolute).to_owned()
}

fn first_uncovered_file(
    paths: &[PathBuf],
    mut covered: impl FnMut(&Path) -> bool,
) -> Option<&Path> {
    paths
        .iter()
        .find(|path| !covered(&parent_directory(path)))
        .map(PathBuf::as_path)
}

fn directory_contains_parent(directory: &Path, parent: &Path) -> bool {
    directory.is_dir()
        && match (directory.canonicalize(), parent.canonicalize()) {
            (Ok(directory), Ok(parent)) => parent.starts_with(directory),
            _ => false,
        }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{directory_contains_parent, first_uncovered_file, parent_directory};
    use crate::e2e::harness::{Harness, open_with};
    use gpui::TestAppContext;
    use std::path::{Path, PathBuf};

    #[test]
    fn access_planning_uses_parent_grants_and_skips_covered_files() {
        let paths = vec![
            PathBuf::from("/notes/a.md"),
            PathBuf::from("/external/b.md"),
        ];
        assert_eq!(
            first_uncovered_file(&paths, |parent| parent.starts_with("/notes")),
            Some(Path::new("/external/b.md"))
        );
        assert!(first_uncovered_file(&paths, |_| true).is_none());
    }

    #[test]
    fn directory_selection_accepts_ancestors_and_rejects_unrelated_folders() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let unrelated = root.path().join("other");
        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir(&unrelated).unwrap();
        assert!(directory_contains_parent(&parent, &parent));
        assert!(directory_contains_parent(root.path(), &parent));
        assert!(!directory_contains_parent(&unrelated, &parent));
        let link = root.path().join("alias");
        std::os::unix::fs::symlink(&parent, &link).unwrap();
        assert!(directory_contains_parent(&link, &parent));
        let file = parent.join("note.md");
        std::fs::write(&file, "note").unwrap();
        let file_link = unrelated.join("alias.md");
        std::os::unix::fs::symlink(&file, &file_link).unwrap();
        assert_eq!(parent_directory(&file_link), parent.canonicalize().unwrap());
    }

    fn request(h: &mut Harness, file: PathBuf) {
        let app = h.app.clone();
        h.cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.request_open_parent(vec![file.clone()], file, window, cx);
            })
        });
        h.cx.run_until_parked();
        assert!(h.cx.has_pending_prompt());
    }

    #[gpui::test]
    fn canceling_parent_access_or_selecting_an_unrelated_folder_opens_nothing(
        cx: &mut TestAppContext,
    ) {
        let mut h = open_with(cx, &[("alpha.md", "existing\n")], |_| {});
        let external = h.root().join("external");
        std::fs::create_dir(&external).unwrap();
        let file = external.join("outside.md");
        std::fs::write(&file, "outside\n").unwrap();
        let initial = h.active_note().id;

        request(&mut h, file.clone());
        let (question, detail) = h.cx.pending_prompt().unwrap();
        assert!(question.contains("outside.md"));
        assert!(detail.contains(external.to_str().unwrap()));
        h.cx.simulate_prompt_answer("Cancel");
        h.cx.run_until_parked();
        assert_eq!(h.active_note().id, initial);
        assert!(!h.cx.did_prompt_for_paths());

        request(&mut h, file.clone());
        h.cx.simulate_prompt_answer("Choose Folder…");
        h.cx.run_until_parked();
        h.cx.simulate_path_prompt_response(|options| {
            assert!(options.directories && !options.files && !options.multiple);
            None
        });
        h.cx.run_until_parked();
        assert_eq!(h.active_note().id, initial);

        request(&mut h, file);
        h.cx.simulate_prompt_answer("Choose Folder…");
        h.cx.run_until_parked();
        h.cx.simulate_path_prompt_response(|_| Some(vec![h.notes.clone()]));
        h.cx.run_until_parked();
        h.wait_for_io();
        assert_eq!(h.active_note().id, initial);
        assert!(!h.cx.has_pending_prompt());
        assert!(!h.cx.did_prompt_for_paths());
    }

    #[gpui::test]
    fn granted_parent_opens_the_file_without_changing_the_notes_root(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("alpha.md", "existing\n")], |_| {});
        let external = h.root().join("external");
        std::fs::create_dir(&external).unwrap();
        let file = external.join("outside.md");
        std::fs::write(&file, "outside\n").unwrap();
        let notes_root = h.notes.clone();
        request(&mut h, file.clone());
        h.cx.simulate_prompt_answer("Choose Folder…");
        h.cx.run_until_parked();
        h.cx.simulate_path_prompt_response(|_| Some(vec![external]));
        h.cx.run_until_parked();
        h.wait_for_io();
        assert_eq!(h.active_note().path, Some(file.canonicalize().unwrap()));
        assert_eq!(
            h.app.update(h.cx, |app, _| app.path.clone()),
            Some(notes_root)
        );
        // The disabled access backend models an already granted directory. A
        // repeated Open Markdown request does not ask for access again.
        let app = h.app.clone();
        h.cx.update(|window, cx| app.update(cx, |app, cx| app.open_paths(vec![file], window, cx)));
        h.cx.run_until_parked();
        h.wait_for_io();
        assert!(!h.cx.has_pending_prompt());
        assert!(!h.cx.did_prompt_for_paths());
    }
}
