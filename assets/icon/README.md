# Markraft icon

Editable production artwork for the blue-and-white, parallel-cut M direction.

## Files

- `Markraft.icon`: Icon Composer document with a solid blue background and one independent SVG glyph layer.
- `markraft-m.svg`: off-white glyph on a transparent 1024 × 1024 canvas.
- `markraft-m-black.svg`: the same geometry in charcoal for use on light backgrounds.
- `Markraft.png`: 1024-pixel native Default render used by the application bundle.
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

`cargo xtask bundle` uses macOS `sips` and `iconutil` to resize `Markraft.png` into the ten standard iconset representations and packages them as `Contents/Resources/Markraft.icns`. Regular builds do not require Icon Composer or Swift scripts. After changing the `.icon` document, regenerate `Markraft.png` before bundling. The `.icns` uses the Default appearance on all supported macOS versions; dynamic Icon Composer appearances are not bundled. The separate menu bar template image remains unchanged.
