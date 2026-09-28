//! Native LaTeX typesetting without a window system or graphics backend.
//!
//! [`typeset`] preserves the TeX baseline and emits self-contained SVG glyph
//! paths. Consumers choose their raster resolution or use the vector artifact
//! directly. Standard math fonts are bundled; Unicode text uses system fallback.

use std::fmt;

use ratex_layout::LayoutOptions;
use ratex_svg::{SvgColorSyntax, SvgOptions};
use ratex_types::{color::Color, math_style::MathStyle};

const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_DISPLAY_ITEMS: usize = 16 * 1024;
// Bound vector geometry independently of the consumer's output resolution.
const MAX_LAYOUT_DIMENSION: f32 = 16_384.0;
const MAX_LAYOUT_AREA: f32 = 16.0 * 1024.0 * 1024.0;
// Small logical padding prevents glyph edges from being clipped by consumers.
const PADDING: f32 = 1.0;

/// Logical typesetting inputs. Output resolution is a consumer concern.
#[derive(Clone, Copy, Debug)]
pub struct TypesetOptions {
    /// Display style uses larger operators and places their limits vertically.
    pub display: bool,
    /// Logical units per em, in the range 1 through 256.
    pub font_size: f32,
    /// Foreground red, green, blue and alpha, each in the range 0 through 1.
    pub color: [f32; 4],
}

/// A vector formula and its baseline metrics, all in logical units.
#[derive(Clone, Debug)]
pub struct MathArtifact {
    /// SVG with embedded glyph outlines; no KaTeX stylesheet is required.
    pub svg: String,
    /// Advance width, including the small padding around the artifact.
    pub width: f32,
    /// Distance from the top of the padded artifact to its TeX baseline.
    pub ascent: f32,
    /// Distance from its TeX baseline to the bottom of the padded artifact.
    pub descent: f32,
}

/// Invalid TeX, invalid options or a formula exceeding the typesetting limits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypesetError {
    /// The source exceeds the supported byte limit.
    SourceTooLarge,
    /// The formula has no non-whitespace source.
    Empty,
    /// The requested font size is outside the supported range.
    InvalidFontSize,
    /// A color component is not finite or outside zero through one.
    InvalidColor,
    /// The display list exceeds the supported item count.
    TooComplex,
    /// The logical geometry exceeds the supported dimensions.
    LayoutTooLarge,
    /// External parser details for invalid LaTeX source.
    InvalidLatex(String),
}

impl fmt::Display for TypesetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceTooLarge => formatter.write_str("Formula exceeds the 16 KiB source limit"),
            Self::Empty => formatter.write_str("Formula is empty"),
            Self::InvalidFontSize => formatter.write_str("Invalid formula font size"),
            Self::InvalidColor => formatter.write_str("Invalid formula foreground color"),
            Self::TooComplex => formatter.write_str("Formula exceeds the display complexity limit"),
            Self::LayoutTooLarge => {
                formatter.write_str("Formula exceeds the logical layout size limit")
            }
            Self::InvalidLatex(error) => write!(formatter, "Invalid LaTeX: {error}"),
        }
    }
}

impl std::error::Error for TypesetError {}

/// Parse and typeset a formula without its Markdown dollar delimiters.
///
/// Source is limited to 16 KiB and output to 16,384 drawing items. Logical
/// geometry is bounded before SVG generation; raster consumers must impose
/// their own pixel allocation limits. This function may run on a worker thread.
pub fn typeset(source: &str, options: TypesetOptions) -> Result<MathArtifact, TypesetError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(TypesetError::SourceTooLarge);
    }
    if source.trim().is_empty() {
        return Err(TypesetError::Empty);
    }
    let font_size = options.font_size;
    if !font_size.is_finite() || !(1.0..=256.0).contains(&font_size) {
        return Err(TypesetError::InvalidFontSize);
    }
    if options
        .color
        .iter()
        .any(|component| !component.is_finite() || !(0.0..=1.0).contains(component))
    {
        return Err(TypesetError::InvalidColor);
    }
    let [r, g, b, a] = options.color;
    let nodes = ratex_parser::parse(source)
        .map_err(|error| TypesetError::InvalidLatex(error.to_string()))?;
    let layout = ratex_layout::layout(
        &nodes,
        &LayoutOptions {
            style: if options.display {
                MathStyle::Display
            } else {
                MathStyle::Text
            },
            color: Color::new(r, g, b, a),
            ..Default::default()
        },
    );
    let list = ratex_layout::to_display_list(&layout);
    if list.items.len() > MAX_DISPLAY_ITEMS {
        return Err(TypesetError::TooComplex);
    }
    let width = list.width as f32 * font_size + 2.0 * PADDING;
    let ascent = list.height as f32 * font_size + PADDING;
    let descent = list.depth as f32 * font_size + PADDING;
    let height = ascent + descent;
    if !width.is_finite()
        || !height.is_finite()
        || !(0.0..=MAX_LAYOUT_DIMENSION).contains(&width)
        || !(0.0..=MAX_LAYOUT_DIMENSION).contains(&height)
        || width == 0.0
        || height == 0.0
        || width * height > MAX_LAYOUT_AREA
    {
        return Err(TypesetError::LayoutTooLarge);
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
    Ok(MathArtifact {
        svg,
        width,
        ascent,
        descent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(display: bool) -> TypesetOptions {
        TypesetOptions {
            display,
            font_size: 16.,
            color: [0., 0., 0., 1.],
        }
    }

    #[test]
    fn common_formulas_emit_self_contained_glyph_paths() {
        for source in [
            r"E = mc^2",
            r"\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
            r"\int_0^1 x^2\,dx = \frac{1}{3}",
            r"\begin{aligned}x &= a + b \\ y &= c + d\end{aligned}",
            r"\begin{pmatrix}a & b \\ c & d\end{pmatrix}",
            r"f(x)=\begin{cases}x^2 & x > 0 \\ 0 & x \leq 0\end{cases}",
        ] {
            let artifact = typeset(source, options(true)).unwrap();
            assert!(artifact.svg.contains("<path"), "{source}");
            assert!(!artifact.svg.contains("<text"), "{source}");
            assert!(artifact.width > 2. && artifact.ascent + artifact.descent > 2.);
        }
    }

    #[test]
    fn display_operators_have_larger_baseline_metrics_than_inline() {
        let source = r"\sum_{n=1}^{\infty} \frac{1}{n^2}";
        let inline = typeset(source, options(false)).unwrap();
        let display = typeset(source, options(true)).unwrap();
        assert!(display.ascent + display.descent > inline.ascent + inline.descent);
        assert!(inline.descent > 0. && display.descent > 0.);
    }

    #[test]
    fn unicode_text_uses_available_system_font_outlines() {
        let artifact = typeset(r"\text{面积} = \pi r^2", options(true)).unwrap();
        assert!(artifact.width > 16.);
        assert!(!artifact.svg.contains("<text"));
    }

    #[test]
    fn malformed_and_excessive_formulas_are_rejected() {
        for source in [
            "",
            r"\frac{1}{",
            r"\notARealCommand{x}",
            r"\rule{100000em}{100000em}",
        ] {
            assert!(typeset(source, options(false)).is_err(), "{source}");
        }
        assert!(typeset(&"x".repeat(MAX_SOURCE_BYTES + 1), options(false)).is_err());
        assert!(
            typeset(
                "x",
                TypesetOptions {
                    font_size: f32::NAN,
                    ..options(false)
                }
            )
            .is_err()
        );
        assert!(
            typeset(
                "x",
                TypesetOptions {
                    color: [0., 0., 0., f32::NAN],
                    ..options(false)
                }
            )
            .is_err()
        );
    }

    #[test]
    fn logical_font_size_scales_baseline_and_width_without_device_inputs() {
        let small = typeset(r"\frac{1}{2}", options(false)).unwrap();
        let large = typeset(
            r"\frac{1}{2}",
            TypesetOptions {
                font_size: 32.,
                ..options(false)
            },
        )
        .unwrap();
        assert!((large.width - 2. * PADDING - (small.width - 2. * PADDING) * 2.).abs() < 0.001);
        assert!((large.ascent - PADDING - (small.ascent - PADDING) * 2.).abs() < 0.001);
        assert!((large.descent - PADDING - (small.descent - PADDING) * 2.).abs() < 0.001);
    }
}
