//! The `[[` menu: completing a wiki link from the notes the folder already holds.
//!
//! A wiki link is spelled by hand and nothing in the note says whether it landed on a
//! page, so a typo reads exactly like a working link until it is followed. The menu is
//! what keeps the two apart: every link it writes names a note that exists.
//!
//! What it offers changes as notes are written, renamed and deleted, so the list is not
//! the snapshot the `/` menu takes — the provider holds a cell the app refills whenever
//! the library moves on.

use super::*;
use markraft_core::commands::{Command as EditCommand, command, replace_selection};
use markraft_gpui::{Typeahead, TypeaheadItem, TypeaheadProvider};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

/// The extension id the `[[` menu registers under.
pub(in crate::app) const WIKI_MENU: &str = "wiki-menu";

/// Typing `[` under a Chinese input method produces `【`, so both open the menu.
const TRIGGERS: [char; 2] = ['[', '【'];

/// How many notes the menu offers at once. The list scrolls, but a folder of thousands
/// has nothing useful to say past the first screenful of matches.
const LIMIT: usize = 50;

/// A note the menu can link to: what it is called, what a link to it has to spell, and
/// where it sits, for telling two notes of the same name apart.
#[derive(Clone)]
pub(in crate::app) struct LinkTarget {
    title: String,
    target: String,
    location: String,
}

/// The notes the `[[` menu offers, shared with the provider that reads them.
pub(in crate::app) type LinkTargets = Rc<RefCell<Vec<LinkTarget>>>;

/// Every spelling of a link that reaches a note, lower-cased: each note's file stem and
/// its path inside the folder, which are the two forms `resolve_wiki_link` reads. The
/// editor asks it once per link per layout, so it answers from here rather than the disk.
pub(in crate::app) type LinkIndex = Rc<RefCell<HashMap<String, ()>>>;

/// What the `[[` menu offers and what the editor asks about each link it draws,
/// together with the one rule they share: when they are out of date.
///
/// Both lists are handed to editors that outlive any one refill, so they are
/// shared handles rather than values — refilling means writing through them, not
/// replacing them. Refilling walks every note in the library, so only what can
/// actually change what a link reaches may ask for one: a note's name, its path,
/// whether it is in the library at all, and which note is open. Everything else
/// leaves them alone, which is why marking them stale goes through
/// [`Links::invalidate`] rather than a `bool` any caller can set.
pub(in crate::app) struct Links {
    targets: LinkTargets,
    index: LinkIndex,
    stale: bool,
}

impl Default for Links {
    /// Stale: nothing has been read yet, so the first poll fills them.
    fn default() -> Links {
        Links {
            targets: LinkTargets::default(),
            index: LinkIndex::default(),
            stale: true,
        }
    }
}

impl Links {
    /// The list the `[[` menu reads, as the handle it keeps.
    pub(in crate::app) fn targets(&self) -> LinkTargets {
        self.targets.clone()
    }

    /// What the editor asks about each wiki link it draws. Nothing is known before the
    /// folder has been read, and treating every link as broken then would be a page of
    /// dead links that are not dead, so an empty index answers yes.
    pub(in crate::app) fn resolver(&self) -> markraft_gpui::WikiResolver {
        let index = self.index.clone();
        Box::new(move |target: &str| {
            let index = index.borrow();
            index.is_empty() || reaches(&index, target)
        })
    }

    /// Something that decides what a link reaches has changed.
    pub(in crate::app) fn invalidate(&mut self) {
        self.stale = true;
    }

    /// The same, for the callers that are asking a question — a title that may or
    /// may not have changed — rather than stating a fact.
    pub(in crate::app) fn invalidate_if(&mut self, changed: bool) {
        self.stale |= changed;
    }

    /// Whether a refill is due, and no longer is. The poll asks once a tick, so
    /// several changes between two ticks cost one walk over the notes.
    pub(in crate::app) fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale)
    }

    /// Write both lists through the handles the editors hold.
    fn fill(&self, targets: Vec<LinkTarget>, index: HashMap<String, ()>) {
        *self.targets.borrow_mut() = targets;
        *self.index.borrow_mut() = index;
    }
}

/// Whether `target` reaches a note in `index`. The target is cut down the same way
/// resolving cuts it: the place inside a note goes, then `./`, then `.md`.
pub(in crate::app) fn reaches(index: &HashMap<String, ()>, target: &str) -> bool {
    let page = crate::app::wiki_link_page(target);
    // `[[#Heading]]` names a place in this very note, so it always reaches something.
    if page.is_empty() {
        return true;
    }
    let wanted = crate::app::without_markdown(page.trim_start_matches("./")).to_lowercase();
    index.contains_key(&wanted)
}

struct WikiProvider {
    targets: LinkTargets,
}

impl TypeaheadProvider for WikiProvider {
    /// The trigger scan stops at the first bracket of `[[`, so the second one arrives
    /// as the head of the query. That is what tells a link apart from an ordinary
    /// bracket: without it there are no items, and no items keeps the menu closed and
    /// its keys ordinary.
    fn items(&self, query: &str) -> Vec<TypeaheadItem> {
        let Some(query) = query.strip_prefix(TRIGGERS) else {
            return Vec::new();
        };
        let wanted = query.trim().to_lowercase();
        let targets = self.targets.borrow();
        let mut matches: Vec<_> = targets
            .iter()
            .filter_map(|note| {
                let title = note.title.to_lowercase();
                (title.contains(&wanted) || note.target.to_lowercase().contains(&wanted)).then(
                    || {
                        (
                            !title.starts_with(&wanted),
                            TypeaheadItem::new(note.target.clone(), note.title.clone())
                                .detail(note.location.clone()),
                        )
                    },
                )
            })
            .collect();
        // Stable, so notes matching from their first letter come first and the
        // library's own order decides within each group.
        matches.sort_by_key(|(inside, _)| *inside);
        matches
            .into_iter()
            .map(|(_, item)| item)
            .take(LIMIT)
            .collect()
    }

    fn leading(&self, _item: &TypeaheadItem, color: Hsla) -> Option<AnyElement> {
        Some(icon(Icon::Document, color).into_any_element())
    }

    /// The trigger run — both brackets and everything typed after them — has already
    /// gone, so the whole link is written in its place. It goes in as Markdown rather
    /// than as text because text spelling a wiki link is not one: the atom is what the
    /// typed `]]` would have produced, and what a save can write back.
    fn accept(&self, item: &TypeaheadItem) -> EditCommand {
        let markdown = format!("[[{}]]", item.id);
        command(move |state| {
            let slice =
                markraft_commonmark::from_markdown_fragment(doc::schema(), &markdown).ok()?;
            replace_selection(slice)(state)
        })
    }
}

impl MarkraftApp {
    /// The `[[` menu for a note editor. It reads the shared list, so a note written
    /// after this editor opened can still be linked to.
    pub(in crate::app) fn wiki_menu(&self) -> Typeahead {
        Typeahead::new(
            WIKI_MENU,
            TRIGGERS.to_vec(),
            WikiProvider {
                targets: self.links.targets(),
            },
        )
    }

    /// Refill the list the `[[` menu reads. Every note that has a file can be linked
    /// to; one still being written has no name to link to yet, and the note the caret
    /// is in is left out because a link to itself is not what `[[` is for.
    pub(in crate::app) fn refresh_link_targets(&mut self) {
        let root = self.path.as_deref();
        let live = || {
            self.library
                .notes
                .iter()
                .filter(|note| note.deleted_at.is_none())
        };
        // A name two notes share cannot say which of them is meant, so a link to
        // either spells its path instead — the two ways `resolve_wiki_link` reads one.
        let mut shared: HashMap<String, usize> = HashMap::new();
        for note in live() {
            if let Some(stem) = note.path.as_deref().and_then(std::path::Path::file_stem) {
                *shared
                    .entry(stem.to_string_lossy().to_lowercase())
                    .or_default() += 1;
            }
        }
        let targets: Vec<LinkTarget> = live()
            .filter(|note| note.id != self.library.active_id)
            .filter_map(|note| {
                let path = note.path.as_deref()?;
                let stem = path.file_stem()?.to_string_lossy().into_owned();
                let relative = root
                    .and_then(|root| path.strip_prefix(root).ok())
                    .unwrap_or(path);
                let target = if shared.get(&stem.to_lowercase()).is_some_and(|n| *n > 1) {
                    crate::app::without_markdown(&relative.to_string_lossy()).to_owned()
                } else {
                    stem
                };
                Some(LinkTarget {
                    title: note.title(),
                    target,
                    location: relative
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .map(|parent| parent.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                })
            })
            .collect();
        // The index covers every note, the open one included: a link to the note it
        // sits in still reaches somewhere, even though the menu does not offer it.
        //
        // Only a folder can answer for what a link reaches. A single file opened on its
        // own has siblings on disk this never sees, so it indexes nothing and the
        // resolver falls back to treating every link as followable — which is what
        // `follow_wiki_link` does there anyway, by looking beside the file.
        let mut index = HashMap::new();
        if let Some(root) = root {
            for note in live() {
                let Some(path) = note.path.as_deref() else {
                    continue;
                };
                if let Some(stem) = path.file_stem() {
                    index.insert(stem.to_string_lossy().to_lowercase(), ());
                }
                let relative = path.strip_prefix(root).unwrap_or(path);
                index.insert(
                    crate::app::without_markdown(&relative.to_string_lossy()).to_lowercase(),
                    (),
                );
            }
        }
        self.links.fill(targets, index);
    }

    /// What the editor asks about each wiki link it draws.
    pub(in crate::app) fn wiki_resolver(&self) -> markraft_gpui::WikiResolver {
        self.links.resolver()
    }
}

#[cfg(test)]
mod tests {
    use super::{LinkTarget, TRIGGERS, WikiProvider};
    use markraft_gpui::TypeaheadProvider;
    use std::{cell::RefCell, collections::HashMap, rc::Rc};

    fn provider(targets: &[(&str, &str, &str)]) -> WikiProvider {
        WikiProvider {
            targets: Rc::new(RefCell::new(
                targets
                    .iter()
                    .map(|(title, target, location)| LinkTarget {
                        title: (*title).to_owned(),
                        target: (*target).to_owned(),
                        location: (*location).to_owned(),
                    })
                    .collect(),
            )),
        }
    }

    #[test]
    fn one_bracket_offers_nothing_and_the_second_opens_the_notes() {
        let menu = provider(&[
            ("Deep Dive", "Deep Dive", ""),
            ("Index", "Index", ""),
            ("Diving Board", "Diving Board", "sport"),
        ]);
        // A single `[` leaves the query without a bracket, and no items keeps the menu
        // shut so the key stays an ordinary bracket.
        assert!(menu.items("").is_empty());
        assert!(menu.items("Deep").is_empty());
        // The second bracket is the head of the query.
        let all: Vec<_> = menu.items("[").iter().map(|i| i.label.clone()).collect();
        assert_eq!(all, vec!["Deep Dive", "Index", "Diving Board"]);
        // Matching from the first letter comes before matching inside.
        let dive: Vec<_> = menu.items("[div").iter().map(|i| i.label.clone()).collect();
        assert_eq!(dive, vec!["Diving Board", "Deep Dive"]);
        // The id is what the link spells; the detail says which folder it is in.
        let board = menu.items("[diving")[0].clone();
        assert_eq!(board.id, "Diving Board");
        assert_eq!(board.detail, "sport");
        // A folder is prose, not a shortcut: it must not come out as key caps.
        assert!(board.hint.is_empty());
    }

    #[test]
    fn the_chinese_bracket_opens_the_same_menu() {
        let menu = provider(&[("Index", "Index", "")]);
        assert_eq!(menu.items("【ind").len(), 1);
        assert!(TRIGGERS.contains(&'【'));
    }

    #[test]
    fn a_link_reaches_a_note_by_stem_by_path_and_never_by_a_name_no_note_has() {
        let index: HashMap<String, ()> = ["deep dive", "notes/deep dive", "index"]
            .into_iter()
            .map(|key| (key.to_owned(), ()))
            .collect();
        // Both spellings a link may use, and case never decides.
        assert!(super::reaches(&index, "Deep Dive"));
        assert!(super::reaches(&index, "deep dive"));
        assert!(super::reaches(&index, "notes/Deep Dive"));
        // The place inside the note, the `./` and the extension all come off first.
        assert!(super::reaches(&index, "Deep Dive#Structure"));
        assert!(super::reaches(&index, "./Deep Dive"));
        assert!(super::reaches(&index, "Deep Dive.md"));
        // A heading in this very note names no other note, so it always reaches.
        assert!(super::reaches(&index, "#Structure"));
        assert!(!super::reaches(&index, "Nowhere"));
        assert!(!super::reaches(&index, "deep"));
    }
}
