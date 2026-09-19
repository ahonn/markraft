//! Rasterize system symbols into GPUI textures without exporting Apple assets.
use gpui::{Image, ImageFormat, RenderImage, Rgba, SvgRenderer};
use objc2::{AllocAnyThread, rc::autoreleasepool};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSCompositingOperation,
    NSDeviceRGBColorSpace, NSFontWeightRegular, NSGraphicsContext, NSImage,
    NSImageSymbolConfiguration, NSImageSymbolScale, NSRectFillUsingOperation,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::sync::Arc;

/// The caller caches the texture by symbol, logical size, display scale and tint.
pub(crate) fn render(name: &str, extent: f32, scale: f32, color: Rgba) -> Option<Arc<RenderImage>> {
    if !extent.is_finite() || !scale.is_finite() || extent <= 0. || scale <= 0. {
        return None;
    }
    let pixels = (extent * scale).ceil() as usize;
    if pixels > 256 {
        return None;
    }
    autoreleasepool(|_| {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            None,
        )?;
        // Ask AppKit for the optical drawing at the actual logical icon size.
        // Shrinking the point size first can make thin symbols disappear at 1x.
        let configuration = NSImageSymbolConfiguration::configurationWithPointSize_weight_scale(
            f64::from(extent),
            unsafe { NSFontWeightRegular },
            NSImageSymbolScale::Medium,
        )
        .configurationByApplyingConfiguration(
            &NSImageSymbolConfiguration::configurationPreferringMonochrome(),
        );
        let image = image.imageWithSymbolConfiguration(&configuration)?;
        let natural = image.size();
        let source = NSRect::new(NSPoint::new(0., 0.), natural);
        if natural.width <= 0. || natural.height <= 0. {
            return None;
        }
        // A dedicated bitmap gives explicit Retina resolution, independent of the
        // current window's graphics context. AppKit owns the allocated planes.
        let bitmap = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(),
                pixels as isize, pixels as isize, 8, 4, true, false,
                NSDeviceRGBColorSpace, (pixels * 4) as isize, 32,
            )?
        };
        let data = bitmap.bitmapData();
        if data.is_null() {
            return None;
        }
        // The bitmap above owns at least bytesPerRow * pixelsHigh writable bytes.
        unsafe { data.write_bytes(0, bitmap.bytesPerRow() as usize * pixels) };
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
        let canvas = NSRect::new(
            NSPoint::new(0., 0.),
            NSSize::new(pixels as f64, pixels as f64),
        );
        let fit = (f64::from(extent) / natural.width.max(natural.height)).min(1.);
        let width = natural.width * fit * f64::from(scale);
        let height = natural.height * fit * f64::from(scale);
        let bounds = NSRect::new(
            NSPoint::new(
                (canvas.size.width - width) / 2.,
                (canvas.size.height - height) / 2.,
            ),
            NSSize::new(width, height),
        );
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        image.drawInRect_fromRect_operation_fraction(
            bounds,
            source,
            NSCompositingOperation::SourceOver,
            1.,
        );
        // Tint the alpha mask so all symbols, including filled variants, obey the
        // same theme/disabled/destructive colors. Cutouts remain transparent.
        NSColor::colorWithSRGBRed_green_blue_alpha(
            color.r.into(),
            color.g.into(),
            color.b.into(),
            color.a.into(),
        )
        .setFill();
        NSRectFillUsingOperation(canvas, NSCompositingOperation::SourceIn);
        NSGraphicsContext::restoreGraphicsState_class();
        // An empty properties dictionary is valid for the PNG encoder.
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )?
        };
        Image::from_bytes(ImageFormat::Png, png.to_vec())
            .to_image_data(SvgRenderer::new(Arc::new(())))
            .ok()
    })
}
