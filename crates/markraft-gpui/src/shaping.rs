//! What laying the document out reads, besides the document itself, and the
//! lines laid out from it.
//!
//! Shaping a line asks the platform to lay out its text, so the view keeps
//! what it shaped — and how tall every line is — until something it read
//! changes; see [`crate::surface::Lines`]. That only holds while nothing can
//! change an input without saying so, which is why they live in their own
//! module: the fields are private to it, so every change goes through a method
//! that marks the lines stale. A field on the view would be writable from every
//! one of the view's sibling modules, and one assignment that forgot would
//! leave the editor drawing a document it no longer holds.
//!
//! Most changes here reach every line — a new style sets every line in a new
//! size — and bump [`Shaping::revision`], which starts every line over. A
//! picture arriving only reaches the lines that draw one.

use crate::WikiResolver;
use crate::images::Images;
use crate::style::EditorStyle;
use crate::surface::Lines;
use std::cell::{RefCell, RefMut};

#[derive(Default)]
pub(crate) struct Shaping {
    messages: crate::EditorMessages,
    style: EditorStyle,
    images: Images,
    maths: crate::maths::Maths,
    scale_factor: Option<f32>,
    /// What the host says a wiki link target can open, so that a link leading
    /// nowhere is not drawn as one that leads somewhere. Absent until the host
    /// says, and then every link is drawn as followable.
    wiki: Option<WikiResolver>,
    /// How the host fetches remote images; absent, none is fetched.
    remote_images: Option<crate::RemoteImageFetcher>,
    asset_loader: Option<crate::AssetLoader>,
    /// Bumped by every change above that reaches every line. Lines laid out
    /// under an older revision are laid out again.
    revision: u64,
    lines: RefCell<Lines>,
}

impl Shaping {
    pub(crate) fn messages(&self) -> &crate::EditorMessages {
        &self.messages
    }

    pub(crate) fn set_messages(&mut self, messages: crate::EditorMessages) {
        self.messages = messages;
        self.changed();
    }
    pub(crate) fn style(&self) -> &EditorStyle {
        &self.style
    }

    /// A style equal to the one held changes nothing shaping reads, so the
    /// lines stay; the host restyles every editor whenever the appearance may
    /// have moved, and most of those calls hand back the same style.
    pub(crate) fn set_style(&mut self, style: EditorStyle) {
        if self.style == style {
            return;
        }
        self.style = style;
        self.changed();
    }

    /// Semantic changes invalidate formula metrics, including whole table grids.
    pub(crate) fn equations_changed(&mut self, types: &markraft_core::kind::DocTypes) {
        self.lines.get_mut().forget_all_math(types);
    }

    pub(crate) fn maths(&self) -> &crate::maths::Maths {
        &self.maths
    }

    pub(crate) fn scale_factor(&self) -> f32 {
        self.scale_factor.unwrap_or(1.0)
    }

    pub(crate) fn set_scale_factor(&mut self, scale: f32) {
        if self.scale_factor() != scale {
            self.scale_factor = Some(scale);
            self.changed();
        }
    }

    pub(crate) fn finish_math(
        &mut self,
        types: &markraft_core::kind::DocTypes,
        equations: &markraft_core::kind::equations::EquationIndex,
        results: Vec<(
            crate::math::MathRequest,
            Result<crate::math::RenderedMath, crate::math::MathError>,
        )>,
    ) {
        let requests: Vec<_> = results.iter().map(|(request, _)| request.clone()).collect();
        self.maths.finish(results);
        let turned_away = self.maths.take_turned_away();
        self.lines
            .get_mut()
            .forget_math(types, Some(equations), &requests, turned_away);
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
    /// that finds nothing must leave the lines alone.
    pub(crate) fn refresh_images(&mut self) -> bool {
        let changed = self.images.refresh();
        if changed {
            self.lines.get_mut().forget_pictures();
        }
        changed
    }

    pub(crate) fn remote_images(&self) -> Option<&crate::RemoteImageFetcher> {
        self.remote_images.as_ref()
    }

    pub(crate) fn set_remote_images(&mut self, fetcher: Option<crate::RemoteImageFetcher>) {
        let changed = match (&self.remote_images, &fetcher) {
            (Some(previous), Some(next)) => !std::sync::Arc::ptr_eq(previous, next),
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.images.set_remote_enabled(false);
            self.images.set_remote_enabled(fetcher.is_some());
        }
        self.remote_images = fetcher;
        self.changed();
    }

    pub(crate) fn asset_loader(&self) -> Option<&crate::AssetLoader> {
        self.asset_loader.as_ref()
    }

    pub(crate) fn set_asset_loader(&mut self, loader: Option<crate::AssetLoader>) {
        let changed = match (&self.asset_loader, &loader) {
            (Some(previous), Some(next)) => !std::sync::Arc::ptr_eq(previous, next),
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.images.set_assets_enabled(false);
            self.images.set_assets_enabled(loader.is_some());
        }
        self.asset_loader = loader;
        self.changed();
    }

    /// Keep a remote image the background fetch delivered, and say whether
    /// the lines drawing pictures have to be laid out again.
    pub(crate) fn finish_remote_image(
        &mut self,
        source: &str,
        result: crate::images::ImageResult,
    ) -> bool {
        let changed = self.images.finish_remote(source, result);
        if changed {
            self.lines.get_mut().forget_pictures();
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

    /// Which revision of the inputs here the lines have to be laid out under.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// The document's lines, as far as they are laid out.
    pub(crate) fn lines(&self) -> RefMut<'_, Lines> {
        self.lines.borrow_mut()
    }

    /// Give back every line's rows, for an editor that is not being drawn.
    /// The heights stay; the next frame shapes what it shows.
    pub(crate) fn release(&self) {
        self.lines.borrow_mut().release();
    }

    fn changed(&mut self) {
        self.revision += 1;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use gpui::px;

    /// Every input that reaches every line starts the lines over.
    #[test]
    fn changing_what_shaping_reads_bumps_the_revision() {
        let bumped = |change: &dyn Fn(&mut Shaping)| {
            let mut shaping = Shaping::default();
            let before = shaping.revision();
            change(&mut shaping);
            shaping.revision() != before
        };
        assert!(bumped(&|shaping| shaping.set_style(EditorStyle {
            body_size: EditorStyle::default().body_size + px(1.),
            ..EditorStyle::default()
        })));
        assert!(bumped(&|shaping| shaping.set_wiki(Box::new(|_| true))));
        assert!(bumped(
            &|shaping| shaping.set_image_base(Some("/tmp".into()))
        ));
        assert!(bumped(
            &|shaping| shaping.set_image_root(Ok(Some("/tmp".into())))
        ));
    }

    /// The host restyles every editor when the appearance may have changed,
    /// and a style equal to the one held must not cost a relayout.
    #[test]
    fn restyling_to_the_same_style_keeps_the_revision() {
        let mut shaping = Shaping::default();
        let before = shaping.revision();
        shaping.set_style(EditorStyle::default());
        assert_eq!(shaping.revision(), before);
    }

    /// The host polls for changed image files several times a second; a poll
    /// that finds nothing must leave the lines alone, or the polling itself
    /// would cost a relayout.
    #[test]
    fn polling_for_unchanged_images_keeps_the_revision() {
        let mut shaping = Shaping::default();
        let before = shaping.revision();
        assert!(!shaping.refresh_images(), "nothing is cached to change");
        assert_eq!(shaping.revision(), before);
    }
    #[test]
    fn replacing_image_fetcher_invalidates_previously_loaded_results() {
        use crate::images::ImageError;
        let mut shaping = Shaping::default();
        let first: crate::RemoteImageFetcher = std::sync::Arc::new(|_| Err("first".into()));
        let second: crate::RemoteImageFetcher = std::sync::Arc::new(|_| Err("disabled".into()));
        let source = "https://example.com/image.png";
        shaping.set_remote_images(Some(first.clone()));
        assert!(matches!(
            shaping.images.load(source),
            Err(ImageError::Loading)
        ));
        shaping.images.take_requests();
        shaping.finish_remote_image(source, Err(ImageError::RemoteFailed));
        shaping.set_remote_images(Some(first));
        assert!(matches!(
            shaping.images.load(source),
            Err(ImageError::RemoteFailed)
        ));
        shaping.set_remote_images(Some(second));
        assert!(matches!(
            shaping.images.load(source),
            Err(ImageError::Loading)
        ));
        assert_eq!(shaping.images.take_requests(), vec![source.to_owned()]);
    }
}
