//! Native math rendering. Callers schedule this work on the background executor.

use std::{collections::HashMap, fmt, sync::Arc};

use gpui::{DevicePixels, Hsla, RenderImage, Rgba, Size, SvgRenderer, SvgSize};
use ratex_layout::LayoutOptions;
use ratex_svg::{SvgColorSyntax, SvgOptions};
use ratex_types::{color::Color, math_style::MathStyle};

const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_DISPLAY_ITEMS: usize = 16 * 1024;
const MAX_PIXEL_DIMENSION: f32 = 8192.0;
const MAX_PIXELS: f32 = 4.0 * 1024.0 * 1024.0;
const CACHE_ENTRIES: usize = 128;
const CACHE_BYTES: usize = 32 * 1024 * 1024;
// Small padding prevents antialiased glyph edges from being clipped.
const PADDING: f32 = 1.0;

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

struct MathSvg {
    svg: String,
    width: f32,
    ascent: f32,
    descent: f32,
    pixel_width: i32,
    pixel_height: i32,
}

fn math_svg(request: &MathRequest) -> Result<MathSvg, MathError> {
    let font_size = f32::from_bits(request.font_size);
    let scale_factor = f32::from_bits(request.scale_factor);
    if request.source.len() > MAX_SOURCE_BYTES {
        return Err(MathError("Formula exceeds the 16 KiB source limit".into()));
    }
    if request.source.trim().is_empty() {
        return Err(MathError("Formula is empty".into()));
    }
    if !font_size.is_finite()
        || !(1.0..=256.0).contains(&font_size)
        || !scale_factor.is_finite()
        || !(0.5..=8.0).contains(&scale_factor)
    {
        return Err(MathError(
            "Invalid formula font size or display scale".into(),
        ));
    }
    let [r, g, b, a] = request.color.map(f32::from_bits);
    if [r, g, b, a]
        .iter()
        .any(|component| !component.is_finite() || !(0.0..=1.0).contains(component))
    {
        return Err(MathError("Invalid formula foreground color".into()));
    }
    let nodes = ratex_parser::parse(&request.source)
        .map_err(|error| MathError(format!("Invalid LaTeX: {error}")))?;
    let options = LayoutOptions {
        style: if request.display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        color: Color::new(r, g, b, a),
        ..Default::default()
    };
    let layout = ratex_layout::layout(&nodes, &options);
    let list = ratex_layout::to_display_list(&layout);
    if list.items.len() > MAX_DISPLAY_ITEMS {
        return Err(MathError(
            "Formula exceeds the display complexity limit".into(),
        ));
    }
    let width = list.width as f32 * font_size + 2.0 * PADDING;
    let ascent = list.height as f32 * font_size + PADDING;
    let descent = list.depth as f32 * font_size + PADDING;
    let pixel_width = (width * scale_factor).ceil();
    let pixel_height = ((ascent + descent) * scale_factor).ceil();
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
    let svg = ratex_svg::render_to_svg_with_color_syntax(
        &list,
        &SvgOptions {
            font_size: font_size as f64,
            padding: PADDING as f64,
            embed_glyphs: true,
            ..Default::default()
        },
        SvgColorSyntax::Rgb,
    );
    Ok(MathSvg {
        svg,
        width,
        ascent,
        descent,
        pixel_width: pixel_width as i32,
        pixel_height: pixel_height as i32,
    })
}

/// Parse, typeset and rasterize without a GPUI window or application context.
pub(crate) fn render_math(request: &MathRequest) -> Result<RenderedMath, MathError> {
    let formula = math_svg(request)?;
    let renderer = SvgRenderer::new(Arc::new(()));
    let svg = renderer
        .parse_svg(formula.svg.as_bytes())
        .map_err(|error| MathError(format!("Could not parse formula SVG: {error}")))?;
    let image = renderer
        .render_parsed(
            &svg,
            SvgSize::ExactSize(Size::new(
                DevicePixels(formula.pixel_width),
                DevicePixels(formula.pixel_height),
            )),
        )
        .map_err(|error| MathError(format!("Could not render formula SVG: {error}")))?;
    Ok(RenderedMath {
        image,
        width: formula.width,
        ascent: formula.ascent,
        descent: formula.descent,
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
    fn common_formulas_embed_math_glyphs_and_rasterize() {
        for source in [
            r"E = mc^2",
            r"\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
            r"\int_0^1 x^2\,dx = \frac{1}{3}",
            r"\begin{aligned}x &= a + b \\ y &= c + d\end{aligned}",
            r"\begin{pmatrix}a & b \\ c & d\end{pmatrix}",
            r"f(x)=\begin{cases}x^2 & x > 0 \\ 0 & x \leq 0\end{cases}",
        ] {
            let request = request(source, true);
            let svg = math_svg(&request).unwrap();
            assert!(svg.svg.contains("<path"), "{source}");
            assert!(
                !svg.svg.contains("<text"),
                "math glyphs must be self contained: {source}"
            );
            let rendered = render_math(&request).unwrap();
            assert!(
                rendered
                    .image
                    .as_bytes(0)
                    .unwrap()
                    .chunks_exact(4)
                    .any(|pixel| pixel[3] > 0),
                "formula raster must contain visible glyphs: {source}"
            );
            assert!(
                rendered.width > 2.0 && rendered.ascent + rendered.descent > 2.0,
                "{source}"
            );
            assert_eq!(
                rendered.image.size(0).width.0,
                (rendered.width * 2.0).ceil() as i32
            );
        }
    }

    #[test]
    fn display_operators_preserve_larger_metrics_than_inline() {
        let source = r"\sum_{n=1}^{\infty} \frac{1}{n^2}";
        let inline = math_svg(&request(source, false)).unwrap();
        let display = math_svg(&request(source, true)).unwrap();
        assert!(display.ascent + display.descent > inline.ascent + inline.descent);
        assert!(inline.descent > 0.0 && display.descent > 0.0);
    }

    #[test]
    fn chinese_text_can_be_rasterized() {
        let request = request(r"\text{面积} = \pi r^2", true);
        let rendered = render_math(&request).unwrap();
        assert!(rendered.width > 16.0);
        // CJK uses the platform's font fallback; math glyphs remain bundled.
        assert!(!math_svg(&request).unwrap().svg.contains("<text"));
    }

    #[test]
    fn malformed_and_unbounded_formulas_return_errors() {
        for source in ["", r"\frac{1}{", r"\notARealCommand{x}"] {
            assert!(render_math(&request(source, false)).is_err(), "{source}");
        }
        assert!(render_math(&request(&"x".repeat(MAX_SOURCE_BYTES + 1), false)).is_err());
        assert!(render_math(&request(r"\rule{100000em}{100000em}", false)).is_err());
        assert!(render_math(&MathRequest::new("x", false, f32::NAN, 2.0, gpui::black())).is_err());
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
