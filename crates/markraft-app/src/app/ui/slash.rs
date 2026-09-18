use super::*;
use markraft_doc::commands::{Command as EditCommand, command};
use markraft_gpui::{Typeahead, TypeaheadItem, TypeaheadProvider};
use std::{any::Any, rc::Rc};

/// The extension id the `/` menu registers under; it names its edits' origin and its
/// host events.
pub(in crate::app) const SLASH_MENU: &str = "slash-menu";

/// Typing `/` under a Chinese input method produces `、`, so both open the menu.
const TRIGGERS: [char; 2] = ['/', '、'];

/// One command of the app. The ⌘K panel offers the ones that carry an [`Intent`], the
/// editor's `/` menu the ones that carry a [`SlashEffect`], and most carry both.
#[derive(Clone)]
pub(super) struct Command {
    pub id: &'static str,
    pub label: &'static str,
    pub shortcut: &'static str,
    /// What ⌘K runs; `None` keeps the command out of that panel.
    pub intent: Option<Intent>,
    /// Rank in the `/` menu and how it is applied; `None` keeps it out.
    pub slash: Option<(u8, SlashEffect)>,
}

/// How the `/` menu applies a command, once the trigger text has been deleted.
#[derive(Clone)]
pub(super) enum SlashEffect {
    /// Set the caret's block format inside the accepting transaction.
    Block(doc::Block),
    /// Hand the command's intent back to the host, which runs it once the edit lands.
    Host,
}

impl Command {
    pub(super) fn new(
        id: &'static str,
        label: &'static str,
        shortcut: &'static str,
        intent: Intent,
    ) -> Self {
        Self {
            id,
            label,
            shortcut,
            intent: Some(intent),
            slash: None,
        }
    }
    /// A command only the editor's `/` menu offers.
    pub(super) fn editor(
        id: &'static str,
        label: &'static str,
        rank: u8,
        effect: SlashEffect,
    ) -> Self {
        Self {
            id,
            label,
            shortcut: "",
            intent: None,
            slash: Some((rank, effect)),
        }
    }
    pub(super) fn slash(mut self, rank: u8, effect: SlashEffect) -> Self {
        self.slash = Some((rank, effect));
        self
    }
}

struct Entry {
    item: TypeaheadItem,
    icon: Icon,
    effect: SlashEffect,
    intent: Option<Intent>,
}

/// Serves the `/` menu from a snapshot of the app's commands. Nothing here reads the
/// app, so the provider can live in the editor for the session's lifetime.
struct SlashProvider {
    entries: Vec<Entry>,
}

impl SlashProvider {
    fn new(commands: Vec<Command>) -> Self {
        let mut ranked: Vec<_> = commands
            .into_iter()
            .filter_map(|command| {
                let (rank, effect) = command.slash?;
                Some((
                    rank,
                    Entry {
                        item: TypeaheadItem::new(command.id, command.label).hint(command.shortcut),
                        icon: command.intent.as_ref().map_or(Icon::Divider, intent_icon),
                        effect,
                        intent: command.intent,
                    },
                ))
            })
            .collect();
        ranked.sort_by_key(|(rank, _)| *rank);
        Self {
            entries: ranked.into_iter().map(|(_, entry)| entry).collect(),
        }
    }
}

impl TypeaheadProvider for SlashProvider {
    /// The same case-insensitive substring match the ⌘K panel uses, widened to the id so
    /// that "code" also finds the code block by its id. Items whose label starts with
    /// the query come first, so "di" offers Divider before it offers Heading.
    fn items(&self, query: &str) -> Vec<TypeaheadItem> {
        let query = query.trim().to_lowercase();
        let mut matches: Vec<_> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let label = entry.item.label.to_lowercase();
                (label.contains(&query) || entry.item.id.to_lowercase().contains(&query))
                    .then(|| (!label.starts_with(&query), &entry.item))
            })
            .collect();
        // Stable, so the menu's own ranking decides within each group.
        matches.sort_by_key(|(inside, _)| *inside);
        matches.into_iter().map(|(_, item)| item.clone()).collect()
    }

    fn leading(&self, item: &TypeaheadItem, color: Hsla) -> Option<AnyElement> {
        let entry = self.entries.iter().find(|entry| entry.item.id == item.id)?;
        Some(icon(entry.icon, color).into_any_element())
    }

    fn accept(&self, item: &TypeaheadItem) -> EditCommand {
        let effect = self
            .entries
            .iter()
            .find(|entry| entry.item.id == item.id)
            .map(|entry| entry.effect.clone());
        match effect {
            Some(SlashEffect::Block(block)) => block.command(),
            // A host command makes no edit of its own; the payload carries its intent.
            _ => command(|_| None),
        }
    }

    fn payload(&self, item: &TypeaheadItem) -> Option<Rc<dyn Any>> {
        let entry = self.entries.iter().find(|entry| entry.item.id == item.id)?;
        match &entry.effect {
            SlashEffect::Host => entry
                .intent
                .clone()
                .map(|intent| Rc::new(intent) as Rc<dyn Any>),
            SlashEffect::Block(_) => None,
        }
    }
}

impl NotesApp {
    /// The `/` menu for a note editor, built from the app's one command list.
    pub(in crate::app) fn slash_menu(&self) -> Typeahead {
        Typeahead::new(
            SLASH_MENU,
            TRIGGERS.to_vec(),
            SlashProvider::new(self.action_items()),
        )
    }

    /// Run what a `/` menu item asked the host to do. The editor has already made its
    /// edit, so this only opens the popover the command needs.
    pub(in crate::app) fn slash_effect(
        &mut self,
        payload: &markraft_gpui::ExtensionPayload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.panel != Panel::Editor {
            return;
        }
        if let Some(intent) = payload.downcast_ref::<Intent>() {
            self.intent(intent.clone(), window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: that would bring gpui's `test` macro in over the built-in one.
    use super::{Command, Intent, SlashEffect, SlashProvider, TypeaheadProvider};
    use crate::doc;
    use markraft_doc::commands::delete_range;
    use markraft_doc::projection::projection_of;
    use markraft_doc::{EditorState, EditorStateConfig, Extension};

    fn provider() -> SlashProvider {
        SlashProvider::new(vec![
            Command::new("format-link", "Link", "⌘L", Intent::Link).slash(10, SlashEffect::Host),
            Command::new(
                "format-heading",
                "Heading 1",
                "⌥⌘1",
                Intent::Block(doc::Block::Heading(1)),
            )
            .slash(1, SlashEffect::Block(doc::Block::Heading(1))),
            Command::new("new-action", "New Note", "⌘N", Intent::New),
            Command::editor(
                "insert-divider",
                "Divider",
                9,
                SlashEffect::Block(doc::Block::Divider),
            ),
        ])
    }

    fn ids(provider: &SlashProvider, query: &str) -> Vec<String> {
        provider
            .items(query)
            .into_iter()
            .map(|item| item.id.to_string())
            .collect()
    }

    #[test]
    fn a_label_prefix_outranks_a_match_inside_the_label() {
        let provider = provider();
        assert_eq!(ids(&provider, "di"), ["insert-divider", "format-heading"]);
        assert_eq!(
            ids(&provider, ""),
            ["format-heading", "insert-divider", "format-link"]
        );
    }

    fn state_of(source: &str) -> EditorState {
        EditorState::create(
            EditorStateConfig::new(doc::schema().clone())
                .doc(doc::from_markdown(source))
                .extensions(Extension::all([
                    markraft_doc::projection::projection(),
                    markraft_doc::history(Default::default()),
                    doc::extensions(),
                ])),
        )
        .expect("a valid state")
        .update([
            markraft_doc::TransactionSpec::new().selection(markraft_doc::Selection::cursor(
                doc::from_markdown(source).content_size() - 1,
            )),
        ])
        .expect("a caret at the end")
        .state()
        .clone()
    }

    /// Exactly what the typeahead runs: the trigger text goes, then the provider's
    /// command applies against what that leaves.
    fn accept(provider: &SlashProvider, text: &str, trigger: usize, query: &str) -> EditorState {
        let item = provider.items(query).remove(0);
        let state = state_of(text);
        let caret = state.selection().head(state.doc());
        let delete = delete_range(1 + trigger, caret)(&state).expect("a deletion");
        let after = state
            .update([delete.clone()])
            .expect("the deletion applies");
        let apply = provider.accept(&item)(after.state());
        let mut specs = vec![delete];
        if let Some(apply) = apply {
            specs.push(apply.sequential());
        }
        state
            .update(specs)
            .expect("one transaction")
            .state()
            .clone()
    }

    #[test]
    fn the_menu_offers_the_ranked_subset_and_filters_on_label_and_id() {
        let provider = provider();
        assert_eq!(
            ids(&provider, ""),
            ["format-heading", "insert-divider", "format-link"]
        );
        assert_eq!(ids(&provider, "HEAD"), ["format-heading"]);
        assert_eq!(ids(&provider, "insert"), ["insert-divider"]);
        // A command with no slash effect stays out however well it matches.
        assert!(ids(&provider, "new note").is_empty());
    }

    #[test]
    fn a_block_item_is_applied_inside_the_transaction() {
        let state = accept(&provider(), "/head", 0, "head");
        assert_eq!(projection_of(&state).plain_text(), "");
        assert_eq!(doc::to_markdown(state.doc()), "# ");
    }

    #[test]
    fn a_host_item_hands_its_intent_back_instead_of_editing() {
        let provider = provider();
        let item = provider.items("link").remove(0);
        let state = state_of("");
        assert!(
            provider.accept(&item)(&state).is_none(),
            "a host item makes no edit of its own"
        );
        assert!(provider.payload(&item).is_some());
    }

    #[test]
    fn the_divider_takes_a_line_of_its_own() {
        let state = accept(&provider(), "note /div", 5, "div");
        assert_eq!(doc::to_markdown(state.doc()), "note \n\n---");
        // An empty line becomes the rule itself rather than leaving a blank above it.
        let state = accept(&provider(), "/div", 0, "div");
        assert_eq!(doc::to_markdown(state.doc()), "---");
    }
}
