//! What an export embeds: pictures read from disk and typeset formulas.

use base64::Engine as _;
use markraft_media::{ImageLocation, ImageType, MAX_IMAGE_BYTES};
use std::path::{Path, PathBuf};

/// Where an exported picture comes from.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Picture {
    /// Embedded as a `data:` URI.
    Embedded(String),
    /// Left as a URL the reader fetches.
    Linked(String),
    /// Not available: missing, too large, unreadable, or remote when remote
    /// images are off. The export shows its alt text.
    Unavailable,
}

pub(super) struct Pictures {
    pub(super) base: Option<PathBuf>,
    pub(super) root: markraft_media::ImageRoot,
    pub(super) remote: bool,
}

impl Pictures {
    pub(super) fn picture(&self, source: &str) -> Picture {
        match markraft_media::locate(source, self.base.as_deref(), self.root.as_root()) {
            Some(ImageLocation::Remote(url)) if self.remote => Picture::Linked(url),
            Some(ImageLocation::File(path)) => {
                embed(&path).map_or(Picture::Unavailable, Picture::Embedded)
            }
            _ => Picture::Unavailable,
        }
    }
}

fn embed(path: &Path) -> Option<String> {
    let mime = ImageType::of_path(path)?.mime();
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(data_uri(mime, &bytes))
}

pub(super) fn data_uri(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// How a formula is drawn into the export.
#[derive(Clone, Copy, Debug)]
pub(super) enum FormulaStyle {
    /// Inline SVG that takes the colour of the text around it.
    Vector,
    /// A PNG at twice the size, in `color`, for readers that do not draw SVG.
    Raster { color: [f32; 4] },
}

/// `tex` typeset at `font_size` pixels, as markup to place in running text, or
/// `None` when it does not typeset.
pub(super) fn formula(
    tex: &str,
    display: bool,
    font_size: f32,
    style: FormulaStyle,
) -> Option<String> {
    let color = match style {
        FormulaStyle::Vector => [0., 0., 0., 1.],
        FormulaStyle::Raster { color } => color,
    };
    let artifact = markraft_math::typeset(
        tex,
        markraft_math::TypesetOptions {
            display,
            font_size,
            color,
        },
    )
    .ok()?;
    let (width, height) = (artifact.width, artifact.ascent + artifact.descent);
    // The artifact's box runs `descent` below the baseline; sitting it that far
    // down lines its baseline up with the text's.
    let align = format!("vertical-align:-{:.2}px", artifact.descent);
    match style {
        FormulaStyle::Vector => {
            let svg = sized_svg(&artifact.svg, width, height, &align);
            Some(current_color(&svg))
        }
        FormulaStyle::Raster { .. } => {
            let png = rasterize(&artifact.svg, width, height)?;
            Some(format!(
                "<img src=\"{}\" alt=\"{}\" width=\"{:.0}\" height=\"{:.0}\" style=\"{align}\">",
                data_uri("image/png", &png),
                markraft_commonmark::html::escape_attr(tex.trim()),
                width.ceil(),
                height.ceil(),
            ))
        }
    }
}

/// The SVG with its size in CSS pixels, which is the unit it was laid out in,
/// rather than the points the typesetter writes.
fn sized_svg(svg: &str, width: f32, height: f32, style: &str) -> String {
    let Some(start) = svg.find("<svg") else {
        return svg.to_owned();
    };
    let Some(end) = svg[start..].find('>').map(|end| start + end) else {
        return svg.to_owned();
    };
    let head: String = svg[start..end]
        .split(' ')
        .filter(|part| !part.starts_with("width=") && !part.starts_with("height="))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{}{head} width=\"{width:.2}\" height=\"{height:.2}\" style=\"{style}\" role=\"img\"{}",
        &svg[..start],
        &svg[end..],
    )
}

/// Every fill as `currentColor`, so the formula follows the text colour of the
/// page it sits in, light or dark.
fn current_color(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(start) = rest.find("fill=\"") {
        let value = start + "fill=\"".len();
        let Some(end) = rest[value..].find('"') else {
            break;
        };
        out.push_str(&rest[..value]);
        if rest[value..value + end] == *"none" {
            out.push_str("none");
        } else {
            out.push_str("currentColor");
        }
        rest = &rest[value + end..];
    }
    out.push_str(rest);
    out
}

fn rasterize(svg: &str, width: f32, height: f32) -> Option<Vec<u8>> {
    const SCALE: f32 = 2.;
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(
        (width * SCALE).ceil() as u32,
        (height * SCALE).ceil() as u32,
    )?;
    let size = tree.size();
    let transform = resvg::tiny_skia::Transform::from_scale(
        width * SCALE / size.width(),
        height * SCALE / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // Native rich-text readers may ignore the HTML image dimensions and use
    // PNG density instead. At 144 DPI, a 2x raster keeps its intended point size.
    let pixels_per_meter = (72. * SCALE / 0.0254_f32).round() as u32;
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, pixmap.width(), pixmap.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: pixels_per_meter,
        yppu: pixels_per_meter,
        unit: png::Unit::Meter,
    }));
    // tiny-skia stores premultiplied alpha; PNG carries straight RGBA.
    let pixels: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&pixels).ok()?;
    writer.finish().ok()?;
    Some(bytes)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn a_vector_formula_is_sized_in_pixels_and_follows_the_text_colour() {
        let formula = formula("x^2", false, 16., FormulaStyle::Vector).unwrap();
        assert!(formula.starts_with("<svg"), "{formula}");
        assert!(!formula.contains("pt\""), "{formula}");
        assert!(formula.contains("vertical-align:-"), "{formula}");
        assert!(formula.contains("fill=\"currentColor\""), "{formula}");
        assert!(!formula.contains("rgb("), "{formula}");
    }

    #[test]
    fn a_raster_formula_is_an_embedded_png_with_its_source_as_alt_text() {
        let formula = formula(
            "a<b",
            true,
            16.,
            FormulaStyle::Raster {
                color: [0., 0., 0., 1.],
            },
        )
        .unwrap();
        assert!(
            formula.starts_with("<img src=\"data:image/png;base64,"),
            "{formula}"
        );
        assert!(formula.contains("alt=\"a&lt;b\""), "{formula}");
    }

    #[test]
    fn invalid_tex_does_not_typeset() {
        assert!(formula("\\frac{", false, 16., FormulaStyle::Vector).is_none());
    }

    #[test]
    fn a_raster_keeps_its_logical_size_and_straight_alpha() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2"><rect width="4" height="2" fill="red" opacity="0.5"/></svg>"#;
        let bytes = rasterize(svg, 4., 2.).unwrap();
        let mut reader = png::Decoder::new(bytes.as_slice()).read_info().unwrap();
        let info = reader.info();
        assert_eq!((info.width, info.height), (8, 4));
        let density = info
            .pixel_dims
            .expect("the native reader needs PNG density");
        assert_eq!(density.unit, png::Unit::Meter);
        let points_per_meter = 72. / 0.0254;
        assert!((info.width as f64 / density.xppu as f64 * points_per_meter - 4.).abs() < 0.001);
        assert!((info.height as f64 / density.yppu as f64 * points_per_meter - 2.).abs() < 0.001);
        let mut pixels = vec![0; reader.output_buffer_size()];
        reader.next_frame(&mut pixels).unwrap();
        assert_eq!(&pixels[..3], &[255, 0, 0]);
        assert!((127..=128).contains(&pixels[3]));
    }

    #[test]
    fn pictures_embed_local_files_and_link_remote_ones_only_when_allowed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"\x89PNG").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"text").unwrap();
        let pictures = |remote| Pictures {
            base: Some(dir.path().to_owned()),
            root: markraft_media::ImageRoot::None,
            remote,
        };
        assert_eq!(
            pictures(false).picture("a.png"),
            Picture::Embedded("data:image/png;base64,iVBORw==".into())
        );
        assert_eq!(pictures(false).picture("b.txt"), Picture::Unavailable);
        assert_eq!(pictures(false).picture("missing.png"), Picture::Unavailable);
        assert_eq!(
            pictures(false).picture("https://example.com/c.png"),
            Picture::Unavailable
        );
        assert_eq!(
            pictures(true).picture("//example.com/c.png"),
            Picture::Linked("https://example.com/c.png".into())
        );
    }
}
