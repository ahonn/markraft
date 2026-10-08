use super::*;
use markraft_core::commands::{Command as EditCommand, command};
use markraft_gpui::{Typeahead, TypeaheadItem, TypeaheadProvider};
use std::{any::Any, cell::RefCell, rc::Rc};

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
    /// Message ID, resolved at the surface that displays it.
    pub label: &'static str,
    pub shortcut: &'static str,
    /// What ⌘K runs; `None` keeps the command out of that panel.
    pub intent: Option<Intent>,
    /// Rank in the `/` menu and how it is applied; `None` keeps it out.
    pub slash: Option<(u8, SlashEffect)>,
    /// Whether the ⌘K panel marks the command as the one already in force, for the few
    /// that describe a state rather than an action. `None` for the rest.
    pub checked: Option<bool>,
    /// The vim `:` commands that run it, each as its full name and the length of its
    /// shortest abbreviation: `("write", 1)` is `:w`, `:wr` and on to `:write`.
    pub ex: &'static [(&'static str, usize)],
    /// Offered only to a `:` query, for a command that stands for vim's rather than
    /// being one of the panel's own.
    pub ex_only: bool,
    /// A value the label names, such as the vault a note is sent to.
    pub arg: Option<(&'static str, String)>,
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
    /// A command the ⌘K panel offers, with the shortcut the one table gives its
    /// intent.
    pub(super) fn new(id: &'static str, label: &'static str, intent: Intent) -> Self {
        Self {
            id,
            label,
            shortcut: super::shortcut_label(&intent),
            intent: Some(intent),
            slash: None,
            checked: None,
            ex: &[],
            ex_only: false,
            arg: None,
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
            checked: None,
            ex: &[],
            ex_only: false,
            arg: None,
        }
    }
    /// The label names `value` where its message says `%{name}`.
    #[cfg(not(feature = "mac-app-store"))]
    pub(super) fn arg(mut self, name: &'static str, value: String) -> Self {
        self.arg = Some((name, value));
        self
    }
    /// The label in `i18n`'s language.
    pub(super) fn text(&self, i18n: &crate::locale::I18n) -> String {
        match &self.arg {
            Some((name, value)) => i18n.text_with(self.label, &[(name, value)]),
            None => i18n.text(self.label),
        }
    }
    pub(super) fn slash(mut self, rank: u8, effect: SlashEffect) -> Self {
        self.slash = Some((rank, effect));
        self
    }
    pub(super) fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }
    pub(super) fn ex(mut self, names: &'static [(&'static str, usize)]) -> Self {
        self.ex = names;
        self
    }
    /// A command that answers a `:` query and nothing else.
    pub(super) fn ex_only(mut self, names: &'static [(&'static str, usize)]) -> Self {
        self.ex = names;
        self.ex_only = true;
        self
    }
    /// The shortest `:` command that runs it, as vim users write it.
    pub(super) fn ex_label(&self) -> Option<String> {
        let (name, shortest) = self.ex.first()?;
        Some(format!(":{}", &name[..*shortest]))
    }
    /// How `typed`, the text after a `:`, reads as one of its names: `Some(true)` when
    /// it runs the command, as a whole name or an abbreviation vim accepts, and
    /// `Some(false)` when it is only on the way to one. `None` when it is neither.
    pub(super) fn ex_match(&self, typed: &str) -> Option<bool> {
        self.ex
            .iter()
            .filter(|(name, _)| name.starts_with(typed))
            .map(|(_, shortest)| typed.len() >= *shortest)
            .max()
    }
}

struct Entry {
    item: TypeaheadItem,
    english_label: String,
    icon: Icon,
    effect: SlashEffect,
    intent: Option<Intent>,
}

/// A translated command cache shared by every open editor. Updating it keeps the
/// providers registered in their original order and preserves each editor's state.
#[derive(Clone, Default)]
pub(in crate::app) struct SlashCommands(Rc<RefCell<SlashCache>>);

#[derive(Default)]
struct SlashCache {
    revision: u64,
    provider: SlashProvider,
}

impl SlashCommands {
    fn replace(&self, provider: SlashProvider) {
        let mut cache = self.0.borrow_mut();
        cache.provider = provider;
        cache.revision = cache.revision.wrapping_add(1);
    }
}

impl TypeaheadProvider for SlashCommands {
    fn revision(&self) -> u64 {
        self.0.borrow().revision
    }

    fn items(&self, query: &str) -> Vec<TypeaheadItem> {
        self.0.borrow().provider.items(query)
    }

    fn leading(&self, item: &TypeaheadItem, color: Hsla) -> Option<AnyElement> {
        self.0.borrow().provider.leading(item, color)
    }

    fn accept(&self, item: &TypeaheadItem) -> EditCommand {
        self.0.borrow().provider.accept(item)
    }

    fn payload(&self, item: &TypeaheadItem) -> Option<Rc<dyn Any>> {
        self.0.borrow().provider.payload(item)
    }
}

#[derive(Default)]
struct SlashProvider {
    entries: Vec<Entry>,
    markers: crate::doc::Markers,
}

impl SlashProvider {
    fn new(
        commands: Vec<Command>,
        i18n: &crate::locale::I18n,
        markers: crate::doc::Markers,
    ) -> Self {
        let english = crate::locale::I18n::english();
        let mut ranked: Vec<_> = commands
            .into_iter()
            .filter_map(|command| {
                let label = command.text(i18n);
                let english_label = command.text(&english);
                let (rank, effect) = command.slash?;
                Some((
                    rank,
                    Entry {
                        item: TypeaheadItem::new(command.id, label).hint(command.shortcut),
                        english_label,
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
            markers,
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
                (label.contains(&query)
                    || entry.english_label.to_lowercase().contains(&query)
                    || entry.item.id.to_lowercase().contains(&query))
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
            Some(SlashEffect::Block(block)) => block.command_with_markers(self.markers),
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

impl MarkraftApp {
    pub(in crate::app) fn refresh_slash_commands(&self) {
        self.slash_commands.replace(SlashProvider::new(
            self.action_items(Caret::default()),
            &self.i18n,
            self.preferences.markers(),
        ));
    }

    /// The `/` menu for a note editor, built from the app's one command list.
    pub(in crate::app) fn slash_menu(&self) -> Typeahead {
        Typeahead::new(SLASH_MENU, TRIGGERS.to_vec(), self.slash_commands.clone())
            // Commands are named in phrases — "Code Block", "Bullet List" — searched as typed.
            .spaces_in_query()
    }

    /// Run what a `/` menu item asked the host to do. The editor has already taken the
    /// trigger text away, so this only runs the command's intent: the link popover, or
    /// a new table.
    pub(in crate::app) fn slash_effect(
        &mut self,
        payload: &markraft_gpui::ExtensionPayload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.interaction.panel() != Panel::Editor {
            return;
        }
        if let Some(intent) = payload.downcast_ref::<Intent>() {
            self.intent(intent.clone(), window, cx);
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    // Not `use super::*`: that would bring gpui's `test` macro in over the built-in one.
    use super::{Command, Intent, SlashEffect, SlashProvider, TypeaheadProvider};
    use crate::doc;
    use markraft_core::commands::delete_range;
    use markraft_core::projection::projection_of;
    use markraft_core::{EditorState, EditorStateConfig, Extension};

    fn provider() -> SlashProvider {
        SlashProvider::new(
            vec![
                Command::new("format-link", "command.link", Intent::Link)
                    .slash(10, SlashEffect::Host),
                Command::new(
                    "format-heading",
                    "command.heading-1",
                    Intent::Block(doc::Block::Heading(1)),
                )
                .slash(1, SlashEffect::Block(doc::Block::Heading(1))),
                Command::new("new-action", "command.new-note", Intent::New),
                Command::editor(
                    "insert-divider",
                    "command.divider",
                    9,
                    SlashEffect::Block(doc::Block::Divider),
                ),
            ],
            &crate::locale::I18n::english(),
            Default::default(),
        )
    }

    #[test]
    fn locale_changes_invalidate_existing_providers_and_keep_english_aliases() {
        let shared = super::SlashCommands::default();
        shared.replace(provider());
        let existing = shared.clone();
        let before = existing.revision();
        shared.replace(SlashProvider::new(
            vec![
                Command::new(
                    "format-heading",
                    "command.heading-1",
                    Intent::Block(doc::Block::Heading(1)),
                )
                .slash(1, SlashEffect::Block(doc::Block::Heading(1))),
            ],
            &crate::locale::I18n::fixture("zh-Hans"),
            Default::default(),
        ));
        assert_ne!(existing.revision(), before);
        for query in ["测试标题", "heading", "format-heading"] {
            let items = existing.items(query);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].label.as_ref(), "测试标题一");
            assert_eq!(items[0].id.as_ref(), "format-heading");
        }
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
                    markraft_core::projection::projection(),
                    markraft_core::history::history(Default::default()),
                    doc::extensions(
                        std::sync::Arc::new(true.into()),
                        std::sync::Arc::new(false.into()),
                    ),
                ])),
        )
        .expect("a valid state")
        .update([markraft_core::TransactionSpec::new().selection(
            markraft_core::Selection::cursor(doc::from_markdown(source).content_size() - 1),
        )])
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

    /// A `:` name runs its command typed whole or cut to any length vim accepts, and
    /// a shorter start of it only leads there.
    #[test]
    fn ex_names_follow_vims_abbreviations() {
        let write = Command::new("save", "Save", Intent::Save).ex(&[("write", 1)]);
        assert_eq!(write.ex_label().as_deref(), Some(":w"));
        assert_eq!(write.ex_match("w"), Some(true));
        assert_eq!(write.ex_match("wri"), Some(true));
        assert_eq!(write.ex_match("write"), Some(true));
        assert_eq!(write.ex_match("writes"), None);
        assert_eq!(write.ex_match("x"), None);
        let all = Command::new("quit", "Quit", Intent::Quit).ex(&[("qall", 2), ("quitall", 5)]);
        assert_eq!(all.ex_label().as_deref(), Some(":qa"));
        assert_eq!(all.ex_match("q"), Some(false), "only on the way to `:qa`");
        assert_eq!(all.ex_match("qa"), Some(true));
        assert_eq!(all.ex_match("quit"), Some(false));
        assert_eq!(all.ex_match("quita"), Some(true));
        assert_eq!(all.ex_match(""), Some(false));
        assert_eq!(Command::new("new", "New", Intent::New).ex_match(""), None);
    }
}
