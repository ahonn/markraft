//! What laying the document out reads, besides the document itself.
//!
//! Shaping walks every line and asks the platform to lay out its text, and a
//! frame asks for that twice: once to measure the note's height, once to draw
//! it. Most redraws change nothing it reads — the caret blinked, the selection
//! moved, a popup opened above — so the rows it produced are kept and handed
//! back until one of the inputs here moves.
//!
//! That only holds while nothing can change an input without saying so, which
//! is why they live in their own module: the fields are private to it, so every
//! change goes through a method that marks the rows stale. A field on the view
//! would be writable from every one of the view's sibling modules, and one
//! assignment that forgot would leave the editor drawing a document it no
//! longer holds.

use crate::images::Images;
use crate::style::EditorStyle;
use crate::surface::LayoutLine;
use crate::{ProtectedSpans, WikiResolver};
use gpui::Pixels;
use markraft_core::projection::Projection;
use std::cell::RefCell;
use std::sync::Arc;

/// The rows one shaping produced, beside what it read to produce them.
///
/// Identity, not equality: the projection is the one the rows were shaped from,
/// and a document that has not changed hands back the very same `Arc`.
struct Shaped {
    projection: Arc<Projection>,
    width: Pixels,
    revision: u64,
    lines: Vec<LayoutLine>,
}

#[derive(Default)]
pub(crate) struct Shaping {
    style: EditorStyle,
    images: Images,
    /// What the host says a wiki link target can open, so that a link leading
    /// nowhere is not drawn as one that leads somewhere. Absent until the host
    /// says, and then every link is drawn as followable.
    wiki: Option<WikiResolver>,
    /// What the host keeps exactly as written. Absent until the host says, and
    /// then nothing is shaded, which is right for an editor whose text is all
    /// alike.
    protected: Option<ProtectedSpans>,
    /// Bumped by every change above. The rows of a shaping that read an older
    /// revision are not the rows this one would produce.
    revision: u64,
    /// The last [`KEPT`] shapings, most recent first. More than one because a
    /// frame measures before it draws, and a measuring pass is free to ask for
    /// a width the drawing pass does not use; keeping only the last would let
    /// the two evict each other every frame and cache nothing at all.
    shaped: RefCell<Vec<Shaped>>,
}

/// How many shapings are kept: the measuring pass's and the drawing pass's.
const KEPT: usize = 2;

impl Shaping {
    pub(crate) fn style(&self) -> &EditorStyle {
        &self.style
    }

    pub(crate) fn set_style(&mut self, style: EditorStyle) {
        self.style = style;
        self.changed();
    }

    pub(crate) fn images(&self) -> &Images {
        &self.images
    }

    pub(crate) fn set_image_base(&mut self, directory: Option<std::path::PathBuf>) {
        self.images.set_base(directory);
        self.changed();
    }

    pub(crate) fn set_image_root(&mut self, root: Result<Option<std::path::PathBuf>, String>) {
        self.images.set_root(root);
        self.changed();
    }

    /// Drop the decoded copy of every image file that changed on disk, and say
    /// whether any had. The host polls this several times a second, so a poll
    /// that finds nothing must leave the laid-out rows alone.
    pub(crate) fn refresh_images(&mut self) -> bool {
        let changed = self.images.refresh();
        if changed {
            self.changed();
        }
        changed
    }

    pub(crate) fn wiki(&self) -> Option<&WikiResolver> {
        self.wiki.as_ref()
    }

    pub(crate) fn set_wiki(&mut self, resolves: WikiResolver) {
        self.wiki = Some(resolves);
        self.changed();
    }

    pub(crate) fn protected(&self) -> Option<&ProtectedSpans> {
        self.protected.as_ref()
    }

    pub(crate) fn set_protected(&mut self, spans: ProtectedSpans) {
        self.protected = Some(spans);
        self.changed();
    }

    /// The rows shaped from `projection` at `width`, when the last shaping read
    /// the same document, the same width and the same inputs. `None` is the
    /// caller's cue to shape and to hand the result to [`Shaping::keep`].
    pub(crate) fn rows(
        &self,
        projection: &Arc<Projection>,
        width: Pixels,
    ) -> Option<Vec<LayoutLine>> {
        self.shaped
            .borrow()
            .iter()
            .find(|shaped| self.matches(shaped, projection, width))
            .map(|shaped| shaped.lines.clone())
    }

    /// Keep `lines` as the rows of `projection` at `width`.
    pub(crate) fn keep(&self, projection: &Arc<Projection>, width: Pixels, lines: &[LayoutLine]) {
        let mut shaped = self.shaped.borrow_mut();
        shaped.retain(|kept| !self.matches(kept, projection, width));
        shaped.insert(
            0,
            Shaped {
                projection: projection.clone(),
                width,
                revision: self.revision,
                lines: lines.to_vec(),
            },
        );
        shaped.truncate(KEPT);
    }

    fn matches(&self, shaped: &Shaped, projection: &Arc<Projection>, width: Pixels) -> bool {
        shaped.width == width
            && shaped.revision == self.revision
            && Arc::ptr_eq(&shaped.projection, projection)
    }

    fn changed(&mut self) {
        self.revision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::single_line;
    use gpui::px;
    use markraft_core::Node;

    fn projection_of(text: &str) -> Arc<Projection> {
        let schema = single_line::schema();
        let paragraph = schema
            .node("paragraph", [schema.text(text)])
            .expect("a paragraph of text");
        let doc: Node = schema
            .doc([paragraph])
            .expect("one paragraph is a document");
        Arc::new(Projection::of(&doc, schema))
    }

    /// The rows come back for the redraws that change nothing: same document,
    /// same width, same inputs.
    #[test]
    fn rows_are_kept_for_a_redraw_that_changes_nothing() {
        let shaping = Shaping::default();
        let projection = projection_of("hello");
        assert!(
            shaping.rows(&projection, px(600.)).is_none(),
            "nothing kept yet"
        );
        shaping.keep(&projection, px(600.), &[]);
        assert!(shaping.rows(&projection, px(600.)).is_some());
        // A second handle on the same projection is the same document.
        assert!(shaping.rows(&projection.clone(), px(600.)).is_some());
    }

    /// Every input shaping reads drops them: a different document, a different
    /// width, or anything reached through a setter here.
    #[test]
    fn changing_what_shaping_reads_drops_the_rows() {
        let projection = projection_of("hello");
        let kept = || {
            let shaping = Shaping::default();
            shaping.keep(&projection, px(600.), &[]);
            shaping
        };

        // A document that changed is a new projection, whatever it holds.
        let shaping = kept();
        assert!(shaping.rows(&projection_of("hello"), px(600.)).is_none());
        assert!(shaping.rows(&projection_of("other"), px(600.)).is_none());

        // A window that resized.
        assert!(shaping.rows(&projection, px(599.)).is_none());

        // And each setter in turn.
        let mut shaping = kept();
        shaping.set_style(EditorStyle::default());
        assert!(shaping.rows(&projection, px(600.)).is_none(), "style");

        let mut shaping = kept();
        shaping.set_wiki(Box::new(|_| true));
        assert!(shaping.rows(&projection, px(600.)).is_none(), "wiki");

        let mut shaping = kept();
        shaping.set_protected(Box::new(|_| Vec::new()));
        assert!(shaping.rows(&projection, px(600.)).is_none(), "protected");

        let mut shaping = kept();
        shaping.set_image_base(Some("/tmp".into()));
        assert!(shaping.rows(&projection, px(600.)).is_none(), "image base");

        let mut shaping = kept();
        shaping.set_image_root(Ok(Some("/tmp".into())));
        assert!(shaping.rows(&projection, px(600.)).is_none(), "image root");
    }

    /// A frame measures at one width and draws at another, over and over. Both
    /// have to keep hitting, or the caching buys nothing.
    #[test]
    fn a_measuring_and_a_drawing_width_do_not_evict_each_other() {
        let shaping = Shaping::default();
        let projection = projection_of("hello");
        for _ in 0..3 {
            shaping.keep(&projection, px(600.), &[]);
            shaping.keep(&projection, px(584.), &[]);
            assert!(shaping.rows(&projection, px(600.)).is_some(), "measured");
            assert!(shaping.rows(&projection, px(584.)).is_some(), "drawn");
        }
        // A third width is one too many, and the oldest goes.
        shaping.keep(&projection, px(320.), &[]);
        assert!(shaping.rows(&projection, px(600.)).is_none());
        assert!(shaping.rows(&projection, px(584.)).is_some());
        assert!(shaping.rows(&projection, px(320.)).is_some());
    }

    /// The host polls for changed image files several times a second; a poll
    /// that finds nothing must leave the rows alone, or the polling itself
    /// would cost a relayout.
    #[test]
    fn polling_for_unchanged_images_keeps_the_rows() {
        let mut shaping = Shaping::default();
        let projection = projection_of("hello");
        shaping.keep(&projection, px(600.), &[]);
        assert!(!shaping.refresh_images(), "nothing is cached to change");
        assert!(shaping.rows(&projection, px(600.)).is_some());
    }
}
