//! Giving a note's file another name, and keeping the links that named it working.
//!
//! A wiki link reaches a note by its file stem, so the file's name is what the rest of
//! the folder knows the note by. Renaming it here rather than in Finder is what lets
//! the note keep its identity and lets the links that pointed at it follow.

use super::*;
use markraft_core::Fragment;
use std::path::Path;

/// The rename pill under the title: whose file it names, and what applying it would
/// do beyond the file itself.
pub(super) struct Rename {
    pub(super) id: String,
    /// How many links elsewhere in the folder reach this note by its present name.
    pub(super) links: usize,
    pub(super) update_links: bool,
    /// Identifies this submission independently of later popovers for the same note.
    operation: Option<std::rc::Rc<()>>,
}

/// The notes a link can resolve to, as `resolve_wiki_link` reads them.
type Places = Vec<(String, PathBuf)>;

/// `target` naming a file called `stem` instead, with everything else it says left as
/// written: the folders before the name, a `.md` after it, the `#heading` or `^block`
/// it goes on to, and the spaces around it.
fn renamed_target(target: &str, stem: &str) -> String {
    let page = wiki_link_page(target);
    let start = target.len() - target.trim_start().len();
    let name = start + page.rfind('/').map_or(0, |slash| slash + 1);
    let end = start + without_markdown(page).len();
    format!("{}{stem}{}", &target[..name], &target[end..])
}

/// `target` spelling out where `path` is inside `root`, for a name that more than one
/// note answers to. Whatever place inside the note it went on to name is kept.
fn qualified_target(target: &str, path: &Path, root: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?.with_extension("");
    let page = wiki_link_page(target);
    let start = target.len() - target.trim_start().len();
    Some(format!(
        "{}{}{}",
        &target[..start],
        relative.to_string_lossy(),
        &target[start + page.len()..]
    ))
}

/// `node` with every wiki link's target passed through `rewrite`, or `None` where
/// that changed nothing. The second count is links `rewrite` answered with a new target.
fn map_wiki_targets(
    node: &Node,
    rewrite: &mut dyn FnMut(&str) -> Option<String>,
    changed: &mut usize,
) -> Option<Node> {
    if doc::schema().node_type(node.type_id()).name() == markraft_commonmark::schema::WIKI_LINK {
        let target = node.attrs().get("target")?.as_str()?;
        let next = rewrite(target)?;
        *changed += 1;
        return Some(node.with_attrs(node.attrs().with("target", next)));
    }
    if node.child_count() == 0 {
        return None;
    }
    let mut any = false;
    let children: Vec<_> = node
        .children()
        .map(|child| match map_wiki_targets(child, rewrite, changed) {
            Some(next) => {
                any = true;
                next
            }
            None => child.clone(),
        })
        .collect();
    any.then(|| node.copy(Fragment::from_nodes(children)))
}

/// What a link written in the note at `from` should say once the note `id` is called
/// `stem`, or `None` for a link that never reached that note.
///
/// `before` and `after` are the folder either side of the rename. The plain respelling
/// is kept wherever it still lands on the note; where another note answers to the new
/// name first, the link spells out the path instead, and a link that cannot be made to
/// land is left alone rather than pointed at a stranger.
fn retarget(
    target: &str,
    id: &str,
    stem: &str,
    from: (&Path, &Path),
    root: Option<&Path>,
    before: &Places,
    after: &Places,
) -> Option<String> {
    let reaches = |target: &str, from: &Path, notes: &Places| {
        let notes = notes.iter().map(|(id, path)| (id.as_str(), path.as_path()));
        resolve_wiki_link(target, Some(from), root, notes).as_deref() == Some(id)
    };
    if !reaches(target, from.0, before) {
        return None;
    }
    let plain = renamed_target(target, stem);
    if reaches(&plain, from.1, after) {
        return Some(plain);
    }
    let path = &after.iter().find(|(note, _)| note == id)?.1;
    qualified_target(target, path, root?).filter(|spelled| reaches(spelled, from.1, after))
}

impl MarkraftApp {
    fn places(&self) -> Places {
        self.library
            .notes
            .iter()
            .filter_map(|note| Some((note.id.clone(), note.path.clone()?)))
            .collect()
    }

    /// Each note's document with its links to `id` respelled for `stem`, and how many
    /// links that is. Notes that take no writing are left out: their links stay as they
    /// are, and the count only promises what can be delivered.
    fn retargeted(
        &self,
        id: &str,
        stem: &str,
        before: &Places,
        after: &Places,
    ) -> Vec<(String, Node, usize)> {
        let root = self.path.as_deref();
        let moved = |path: &Path| {
            after
                .iter()
                .find(|(note, _)| before.iter().any(|(b, p)| b == note && p == path))
                .map_or_else(|| path.to_owned(), |(_, path)| path.clone())
        };
        self.library
            .notes
            .iter()
            .filter(|note| note.read_only.is_none())
            .filter_map(|note| {
                let from = before.iter().find(|(id, _)| *id == note.id)?.1.as_path();
                let to = moved(from);
                let mut changed = 0;
                let document = map_wiki_targets(
                    &note.document,
                    &mut |target| retarget(target, id, stem, (from, &to), root, before, after),
                    &mut changed,
                )?;
                Some((note.id.clone(), document, changed))
            })
            .collect()
    }

    /// The folder as it would stand with `id`'s file called `stem`.
    fn places_after(&self, id: &str, stem: &str) -> Places {
        let mut places = self.places();
        if let Some((_, path)) = places.iter_mut().find(|(note, _)| note == id) {
            let extension = path.extension().map(ToOwned::to_owned);
            path.set_file_name(stem);
            if let Some(extension) = extension {
                // `set_extension` would cut a stem like `v1.2` at its own dot.
                path.as_mut_os_string().push(".");
                path.as_mut_os_string().push(extension);
            }
        }
        places
    }

    /// Click on the title, or Rename… in the command panel.
    pub(super) fn open_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.persistence.is_none() {
            return;
        }
        self.set_panel(Panel::Editor, cx);
        self.ring.release();
        let note = self.library.active_note();
        // A note with no file yet has nothing to rename; the next autosave files it.
        let Some(path) = note.path.clone() else {
            if !note.document_is_empty() {
                let id = note.id.clone();
                self.inform("Saving the note first…", cx);
                self.flush_then(window, cx, move |this, window, cx| {
                    if this.library.active_id == id
                        && this
                            .library
                            .note(&id)
                            .is_some_and(|note| note.path.is_some())
                    {
                        this.open_rename(window, cx);
                    }
                });
            }
            return;
        };
        if note.read_only.is_some() {
            self.inform("Read-only notes can't be renamed.", cx);
            return;
        }
        let id = note.id.clone();
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.sync_documents(cx);
        // Any other name moves every link that reaches this one, so the count does not
        // wait for the name to be typed.
        let links = self
            .retargeted(
                &id,
                "\u{0}",
                &self.places(),
                &self.places_after(&id, "\u{0}"),
            )
            .iter()
            .map(|(_, _, links)| links)
            .sum();
        self.show_popover(
            Popover::Rename(Rename {
                id,
                links,
                update_links: true,
                operation: None,
            }),
            cx,
        );
        self.set_query(stem, "Name", "File name", cx);
        self.query().update(cx, |query, cx| query.select_all(cx));
        window.focus(&self.query().focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn close_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.interaction.rename().is_some() && self.close_popover(cx) {
            self.focus_editor(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_rename_links(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.interaction.rename_mut() {
            rename.update_links = !rename.update_links;
            cx.notify();
        }
    }

    pub(super) fn apply_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.interaction.rename() else {
            return;
        };
        // A failed save barrier does not invoke its continuation. Once I/O has
        // settled, a new submission may replace that abandoned operation.
        if rename.operation.is_some() && self.io.pending > 0 {
            return;
        }
        let (id, update_links) = (rename.id.clone(), rename.update_links);
        if self.library.active_id != id {
            self.close_rename(window, cx);
            return;
        }
        let name = self.query().read(cx).text().trim().to_owned();
        let operation = std::rc::Rc::new(());
        self.interaction.rename_mut().unwrap().operation = Some(operation.clone());
        self.flush_then(window, cx, move |this, window, cx| {
            let Some(before) = this.library.note(&id).and_then(|note| note.path.clone()) else {
                this.finish_rename(&operation, false, window, cx);
                return;
            };
            let Some(persistence) = &this.persistence else {
                this.finish_rename(&operation, false, window, cx);
                return;
            };
            let future = persistence.rename_async(id.clone(), name);
            this.run_io(
                future,
                window,
                cx,
                move |this, result, window, cx| match result {
                    Ok(path) => {
                        this.complete_rename(&id, &before, path, update_links, cx);
                        this.finish_rename(&operation, true, window, cx);
                    }
                    Err(error) => {
                        this.finish_rename(&operation, false, window, cx);
                        this.inform(error.to_string(), cx);
                    }
                },
            );
        });
    }

    fn finish_rename(
        &mut self,
        operation: &std::rc::Rc<()>,
        success: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rename) = self.interaction.rename_mut() else {
            return;
        };
        if !rename
            .operation
            .as_ref()
            .is_some_and(|pending| std::rc::Rc::ptr_eq(pending, operation))
        {
            return;
        }
        rename.operation = None;
        if success {
            self.close_rename(window, cx);
        }
    }

    /// Rename the file, then — asked to — respell the links that reached it.
    ///
    /// The file on disk has to say what the editor says before it moves, so the rename
    /// stands behind the same barrier a quit does. Links are respelled in the documents
    /// and left to the ordinary save, which writes only the lines that changed; a note
    /// whose source cannot take the change keeps its link as it was.
    fn complete_rename(
        &mut self,
        id: &str,
        before: &Path,
        path: PathBuf,
        update_links: bool,
        cx: &mut Context<Self>,
    ) {
        // A save receipt can publish this operation's path first. Accept either
        // address, but never resurrect a deleted note or undo a later rename.
        if path == before
            || !self.library.note(id).is_some_and(|note| {
                note.path.as_deref() == Some(before) || note.path.as_ref() == Some(&path)
            })
        {
            return;
        }
        // Retarget the documents as they are now, including any text entered
        // while the worker was renaming the file.
        self.sync_documents(cx);
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        // A save receipt may publish the renamed path before this response.
        // Resolve old links against the pre-rename address even in that order.
        let mut before_places = self.places();
        if let Some((_, place)) = before_places.iter_mut().find(|(note, _)| note == id) {
            *place = before.to_owned();
        }
        let after = self.places_after(id, &stem);
        let edits = if update_links {
            self.retargeted(id, &stem, &before_places, &after)
        } else {
            Vec::new()
        };
        self.update_paths(vec![(id.to_owned(), path)], cx);
        let (mut updated, mut kept) = (0, 0);
        for (note_id, document, links) in edits {
            let Some(mut note) = self.library.note(&note_id).cloned() else {
                continue;
            };
            note.document = document.clone();
            // Asked of the store first: a link inside source it keeps verbatim cannot be
            // written, and finding that out at save time would leave a note the user
            // never touched reported as unsaved.
            let writable = self
                .persistence
                .as_ref()
                .is_some_and(|p| p.markdown(note).is_ok());
            if !writable || !self.library.set_document(&note_id, document.clone()) {
                kept += links;
                continue;
            }
            updated += links;
            if let Some(session) = self.sessions.get(&note_id) {
                // The editor owns its document; replacing it costs that note its undo
                // history, which is the price of the link being right in both places.
                session
                    .editor()
                    .update(cx, |editor, cx| editor.replace_doc(document, cx));
            }
        }
        self.notes_changed(cx);
        self.refresh_link_targets();
        let mut message = format!("Renamed to “{stem}”");
        if updated > 0 {
            message.push_str(&format!(
                " · {updated} {} updated",
                if updated == 1 { "link" } else { "links" }
            ));
        }
        if kept > 0 {
            message.push_str(&format!(" · {kept} not changed"));
        }
        self.inform(message, cx);
    }
}

#[cfg(test)]
mod tests {
    // Named rather than globbed: `super::*` carries gpui's own `test` attribute.
    use super::{map_wiki_targets, renamed_target, retarget};
    use crate::doc;
    use std::path::{Path, PathBuf};

    #[gpui::test]
    fn an_early_path_receipt_does_not_skip_backlinks_or_break_their_editor(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut h = crate::e2e::harness::open_with(
            cx,
            &[
                ("Welcome.md", "Welcome\n"),
                ("Backlinks.md", "Backlinks\n\n[[Welcome]]\n"),
            ],
            |_| {},
        );
        h.keys("cmd-p");
        h.type_text("Backlinks");
        h.keys("enter");
        h.save();
        let active = h.active_note().id;
        let app = h.app.clone();
        h.cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                let target = app
                    .library
                    .notes
                    .iter()
                    .find(|note| {
                        note.path
                            .as_ref()
                            .is_some_and(|path| path.ends_with("Welcome.md"))
                    })
                    .unwrap();
                let id = target.id.clone();
                let before = target.path.clone().unwrap();
                let future = app
                    .persistence
                    .as_ref()
                    .unwrap()
                    .rename_async(id.clone(), "Renamed".into());
                app.run_io(future, window, cx, move |app, result, _, cx| {
                    let path = result.unwrap();
                    // Replay the order in which a save receipt reaches the UI
                    // before the rename reply, without relying on thread timing.
                    app.update_paths(vec![(id.clone(), path.clone())], cx);
                    app.complete_rename(&id, &before, path, true, cx);
                });
            });
        });
        h.wait_for_io();
        assert_eq!(h.active_note().id, active);
        assert_eq!(h.markdown(), "Backlinks\n\n[[Renamed]]");
        h.keys("cmd-down cmd-right");
        h.type_text(" retained");
        h.save();
        assert_eq!(h.markdown(), "Backlinks\n\n[[Renamed]] retained");
        assert_eq!(
            std::fs::read_to_string(h.notes.join("Backlinks.md")).unwrap(),
            "Backlinks\n\n[[Renamed]] retained\n"
        );
        assert_eq!(h.files(), ["Backlinks.md", "Renamed.md"]);
    }

    #[test]
    fn a_renamed_target_keeps_everything_but_the_name() {
        assert_eq!(renamed_target("Old", "New"), "New");
        assert_eq!(renamed_target(" Old ", "New"), " New ");
        assert_eq!(renamed_target("Old#Heading", "New"), "New#Heading");
        assert_eq!(renamed_target("Old#^block", "New"), "New#^block");
        assert_eq!(renamed_target("Old.md", "New"), "New.md");
        assert_eq!(renamed_target("work/Old.MD#H", "New"), "work/New.MD#H");
        assert_eq!(renamed_target("./work/Old", "v1.2"), "./work/v1.2");
    }

    #[test]
    fn only_links_that_reached_the_note_are_respelled_and_never_onto_a_stranger() {
        let root = Path::new("/notes");
        let place = |id: &str, path: &str| (id.to_owned(), PathBuf::from(path));
        let before = vec![
            place("a", "/notes/docs/Old.md"),
            place("b", "/notes/Index.md"),
            place("c", "/notes/work/Plan.md"),
            place("d", "/notes/work/Other.md"),
        ];
        let mut after = before.clone();
        after[0].1 = PathBuf::from("/notes/docs/Plan.md");
        let index = Path::new("/notes/Index.md");
        let other = Path::new("/notes/work/Other.md");
        let ask = |target: &str, from: &Path| {
            retarget(
                target,
                "a",
                "Plan",
                (from, from),
                Some(root),
                &before,
                &after,
            )
        };
        assert_eq!(ask("Old#Heading", index).as_deref(), Some("Plan#Heading"));
        assert_eq!(ask("docs/Old.md", index).as_deref(), Some("docs/Plan.md"));
        assert_eq!(ask("Index", index), None);
        assert_eq!(ask("Missing", index), None);
        // Beside `work/Plan.md` a bare `Plan` is that neighbour, so the link says where.
        assert_eq!(ask(" Old#^id ", other).as_deref(), Some(" docs/Plan#^id "));
    }

    #[test]
    fn links_are_respelled_wherever_they_sit_in_the_document() {
        let document = doc::from_markdown("See [[Old]].\n\n- item ![[Old#H|alias]]\n\n[[Other]]");
        let mut changed = 0;
        let next = map_wiki_targets(
            &document,
            &mut |target| {
                target
                    .starts_with("Old")
                    .then(|| target.replacen("Old", "New", 1))
            },
            &mut changed,
        )
        .unwrap();
        assert_eq!(changed, 2);
        assert_eq!(
            doc::to_markdown(&next),
            "See [[New]].\n\n- item ![[New#H|alias]]\n\n[[Other]]"
        );
        assert!(map_wiki_targets(&document, &mut |_| None, &mut 0).is_none());
    }

    #[test]
    fn a_respelled_link_is_the_only_thing_the_save_changes_in_its_file() {
        let root = tempfile::tempdir().unwrap();
        let notes = root.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let source = "---\r\ntags: [a]\r\n---\r\n\r\nIndex\r\n\r\nSee   [[Old#H|the old one]] and *this*.\r\n\r\n* item ![[Old]]\r\n";
        std::fs::write(notes.join("Index.md"), source).unwrap();
        let (mut store, mut library) =
            crate::vault::open_reading_settings(notes.clone(), root.path().join("settings.json"))
                .unwrap();
        let id = library.active_id.clone();
        let document = map_wiki_targets(
            &library.active_note().document,
            &mut |target| Some(renamed_target(target, "New")),
            &mut 0,
        )
        .unwrap();
        assert!(library.set_document(&id, document));
        store
            .save(&library, &crate::storage::Preferences::default())
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(notes.join("Index.md")).unwrap(),
            source.replace("Old", "New")
        );
    }
}
