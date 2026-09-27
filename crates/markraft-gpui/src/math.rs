//! GPUI rasterization and bounded screen-image caching for math artifacts.
//! Callers schedule typesetting and rasterization on the background executor.

use std::{collections::HashMap, fmt, sync::Arc};

use gpui::{DevicePixels, Hsla, RenderImage, Rgba, Size, SvgRenderer, SvgSize};
use markraft_math::{MathArtifact, TypesetOptions, typeset};

const MAX_PIXEL_DIMENSION: f32 = 8192.0;
const MAX_PIXELS: f32 = 4.0 * 1024.0 * 1024.0;
const CACHE_ENTRIES: usize = 128;
const CACHE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct MathRequest {
    pub source: Arc<str>,
    display: bool,
    font_size: u32,
    scale_factor: u32,
    color: [u32; 4],
}

impl MathRequest {
    pub(crate) fn new(
        source: impl Into<Arc<str>>,
        display: bool,
        font_size: f32,
        scale_factor: f32,
        color: Hsla,
    ) -> Self {
        let color: Rgba = color.into();
        Self {
            source: source.into(),
            display,
            font_size: font_size.to_bits(),
            scale_factor: scale_factor.to_bits(),
            color: [color.r, color.g, color.b, color.a].map(f32::to_bits),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RenderedMath {
    pub image: Arc<RenderImage>,
    /// Logical dimensions, including the antialiasing padding.
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
}

impl RenderedMath {
    fn byte_len(&self) -> usize {
        let size = self.image.size(0);
        size.width.0 as usize * size.height.0 as usize * 4
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MathError(String);

impl fmt::Display for MathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for MathError {}

/// Typeset logical geometry, then rasterize for this window's device scale.
pub(crate) fn render_math(request: &MathRequest) -> Result<RenderedMath, MathError> {
    let scale_factor = f32::from_bits(request.scale_factor);
    if !scale_factor.is_finite() || !(0.5..=8.0).contains(&scale_factor) {
        return Err(MathError("Invalid formula display scale".into()));
    }
    let artifact = typeset(
        &request.source,
        TypesetOptions {
            display: request.display,
            font_size: f32::from_bits(request.font_size),
            color: request.color.map(f32::from_bits),
        },
    )
    .map_err(|error| MathError(error.to_string()))?;
    rasterize(&artifact, scale_factor)
}

/// Pixel allocation limits belong to the graphics adapter, not typesetting.
fn rasterize(artifact: &MathArtifact, scale_factor: f32) -> Result<RenderedMath, MathError> {
    let pixel_width = (artifact.width * scale_factor).ceil();
    let pixel_height = ((artifact.ascent + artifact.descent) * scale_factor).ceil();
    if !pixel_width.is_finite()
        || !pixel_height.is_finite()
        || !(1.0..=MAX_PIXEL_DIMENSION).contains(&pixel_width)
        || !(1.0..=MAX_PIXEL_DIMENSION).contains(&pixel_height)
        || pixel_width * pixel_height > MAX_PIXELS
    {
        return Err(MathError(
            "Formula exceeds the rendered image size limit".into(),
        ));
    }
    let renderer = SvgRenderer::new(Arc::new(()));
    let svg = renderer
        .parse_svg(artifact.svg.as_bytes())
        .map_err(|error| MathError(format!("Could not parse formula SVG: {error}")))?;
    let image = renderer
        .render_parsed(
            &svg,
            SvgSize::ExactSize(Size::new(
                DevicePixels(pixel_width as i32),
                DevicePixels(pixel_height as i32),
            )),
        )
        .map_err(|error| MathError(format!("Could not render formula SVG: {error}")))?;
    Ok(RenderedMath {
        image,
        width: artifact.width,
        ascent: artifact.ascent,
        descent: artifact.descent,
    })
}

struct CacheEntry {
    result: Result<RenderedMath, MathError>,
    used: u64,
    bytes: usize,
}

/// Bounded completed results, including errors so invalid input is not retried each frame.
/// The owner tracks in-flight tasks separately and inserts only completed requests.
#[derive(Default)]
pub(crate) struct MathCache {
    entries: HashMap<MathRequest, CacheEntry>,
    clock: u64,
    bytes: usize,
}

impl MathCache {
    pub(crate) fn get(
        &mut self,
        request: &MathRequest,
    ) -> Option<&Result<RenderedMath, MathError>> {
        let entry = self.entries.get_mut(request)?;
        self.clock += 1;
        entry.used = self.clock;
        Some(&entry.result)
    }

    pub(crate) fn insert(&mut self, request: MathRequest, result: Result<RenderedMath, MathError>) {
        if let Some(previous) = self.entries.remove(&request) {
            self.bytes -= previous.bytes;
        }
        let bytes = request.source.len()
            + result
                .as_ref()
                .map_or_else(|error| error.0.len(), RenderedMath::byte_len);
        if bytes > CACHE_BYTES {
            return;
        }
        while self.entries.len() >= CACHE_ENTRIES || self.bytes + bytes > CACHE_BYTES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.bytes;
            }
        }
        self.clock += 1;
        self.bytes += bytes;
        self.entries.insert(
            request,
            CacheEntry {
                result,
                used: self.clock,
                bytes,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(source: &str, display: bool) -> MathRequest {
        MathRequest::new(source, display, 16.0, 2.0, gpui::black())
    }

    #[test]
    fn vector_artifact_rasterizes_without_changing_logical_metrics() {
        let artifact = typeset(
            r"\frac{1}{2}",
            TypesetOptions {
                display: true,
                font_size: 16.,
                color: [0., 0., 0., 1.],
            },
        )
        .unwrap();
        for scale in [1., 2.] {
            let rendered = rasterize(&artifact, scale).unwrap();
            assert_eq!(rendered.width, artifact.width);
            assert_eq!(rendered.ascent, artifact.ascent);
            assert_eq!(rendered.descent, artifact.descent);
            assert_eq!(
                rendered.image.size(0).width.0,
                (artifact.width * scale).ceil() as i32
            );
            assert!(
                rendered
                    .image
                    .as_bytes(0)
                    .unwrap()
                    .chunks_exact(4)
                    .any(|pixel| pixel[3] > 0)
            );
        }
    }

    #[test]
    fn device_scale_and_pixel_limits_are_enforced_after_typesetting() {
        for scale in [f32::NAN, 0., 9.] {
            assert!(render_math(&MathRequest::new("x", false, 16., scale, gpui::black())).is_err());
        }
        let artifact = typeset(
            r"\rule{200em}{200em}",
            TypesetOptions {
                display: true,
                font_size: 16.,
                color: [0., 0., 0., 1.],
            },
        )
        .unwrap();
        assert!(rasterize(&artifact, 2.).is_err());
        assert!(render_math(&request(r"\unknownCommand", false)).is_err());
    }

    #[test]
    fn scale_and_color_are_part_of_the_cache_key() {
        let first = request("x", false);
        assert_ne!(
            first,
            MathRequest::new("x", false, 16.0, 1.0, gpui::black())
        );
        assert_ne!(
            first,
            MathRequest::new("x", false, 16.0, 2.0, gpui::white())
        );
        let normal = render_math(&MathRequest::new("x", false, 16.0, 1.0, gpui::black())).unwrap();
        let retina = render_math(&first).unwrap();
        assert_eq!(normal.width, retina.width);
        assert!(retina.image.size(0).width > normal.image.size(0).width);
    }

    #[test]
    fn cache_keeps_recent_errors_and_evicts_oldest_entries() {
        let mut cache = MathCache::default();
        for index in 0..CACHE_ENTRIES {
            cache.insert(
                request(&index.to_string(), false),
                Err(MathError("invalid".into())),
            );
        }
        assert!(cache.get(&request("0", false)).is_some());
        cache.insert(request("new", false), Err(MathError("invalid".into())));
        assert!(cache.get(&request("0", false)).is_some());
        assert!(cache.get(&request("1", false)).is_none());
        assert_eq!(cache.entries.len(), CACHE_ENTRIES);
        assert!(cache.bytes < CACHE_BYTES);
    }
}
