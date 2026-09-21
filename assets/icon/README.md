# Markraft icon

Editable production artwork for the blue-and-white, parallel-cut M direction.

## Files

- `Markraft.icon`: Icon Composer document with a solid blue background and one independent SVG glyph layer.
- `markraft-m.svg`: off-white glyph on a transparent 1024 × 1024 canvas.
- `markraft-m-black.svg`: the same geometry in charcoal for use on light backgrounds.
- `Markraft.png`: 1024-pixel native Default render used by the application bundle.
- `markraft-menubar.svg`, `markraft-menubar@2x.svg`: the glyph redrawn for the menu bar, once per display scale.
- `markraft-menubar.png`, `markraft-menubar@2x.png`: their renders, embedded by `crates/markraft-app/src/platform.rs` as the two representations of the status item's template image.
- `previews/`: native macOS appearance and small-size previews.

The background is `#2F7CF6`, matching `EditorStyle::notes()` marker and link colors in `crates/markraft-gpui/src/style.rs`. The glyph is `#FAFAFA`. The glyph occupies 660 × 491.04 points and is centered. In master coordinates, the opposing slit edges lie on `y = x - 250` and `y = x - 340`. Both have a 45-degree slope; their perpendicular separation on the final canvas is approximately 42 points.

Glass, group specular effects, translucency, and group shadows are disabled. Icon Composer still supplies the platform mask and background edge lighting. Do not bake those effects into the SVG. Tinted and Clear previews show system appearance transformations, not additional brand colors.

## Editing and rendering

Open `Markraft.icon` in Icon Composer. Its embedded `Assets/markraft-m.svg` matches the standalone white SVG; keep these copies synchronized when editing the geometry.

From the repository root, render with the Xcode-bundled tool:

```sh
"/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool" \
  assets/icon/Markraft.icon --export-image \
  --output-file assets/icon/Markraft.png \
  --platform macOS --rendition Default --width 1024 --height 1024 --scale 1
```

The document was created using [compose-app-icon](https://github.com/giginet/apple-icon-composer-skill) at commit `eb6051e461521e00fdea7da6d59407f54b35a797`, then opened and saved in Icon Composer. Its schema validator passed. Native `ictool` 1.6 successfully rendered Default, Dark, TintedLight, TintedDark, ClearLight, and ClearDark; Default was also inspected at 32 pixels. The original charcoal version was checked in the GUI in Default, Dark, and Mono. The blue revision was validated using native renders.

The manifest intentionally omits `color-space-for-untagged-svg-colors`: the skill schema accepts `srgb` for this key, but the installed native renderer rejects it. Omitting it renders successfully; the SVG uses explicit neutral hex fills.

`cargo xtask bundle` uses macOS `sips` and `iconutil` to resize `Markraft.png` into the ten standard iconset representations and packages them as `Contents/Resources/Markraft.icns`. Regular builds do not require Icon Composer or Swift scripts. After changing the `.icon` document, regenerate `Markraft.png` before bundling. The `.icns` uses the Default appearance on all supported macOS versions; dynamic Icon Composer appearances are not bundled.

## Menu bar template

The menu bar glyph is drawn for its size, not reduced from `markraft-m.svg`, and drawn twice because a non-Retina display shows the 18-pixel bitmap and a Retina display the 36-pixel one. Both share an 18-point canvas with a centered 16 × 12 point glyph, stems a quarter of its width, and every straight edge on that file's pixel grid. The slit keeps its 45-degree slope but is wider than the app icon's: 3 pixels across at 1x, which leaves two whole pixels clear in each row, and 4 at 2x. A plain reduction would leave 1.4 and 2.9, and the 1x slit fills in. Keep the two files' outlines in step when editing, but do not derive one from the other.

macOS uses only the alpha channel of a template image, so the fill color is irrelevant and the blue background is never part of it.

After editing, render each SVG at its own size with `sips`, which rasterizes SVG natively and keeps transparency:

```sh
sips -s format png assets/icon/markraft-menubar.svg --out assets/icon/markraft-menubar.png
sips -s format png assets/icon/markraft-menubar@2x.svg --out assets/icon/markraft-menubar@2x.png
```

A test in `platform.rs` checks that the embedded files are 18 and 36 pixels.
