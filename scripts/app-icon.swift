// Resize the Icon Composer export to the representations required by iconutil.
import AppKit
import Foundation

guard CommandLine.arguments.count == 3 else {
    fputs("Usage: swift scripts/app-icon.swift <source.png> <output.iconset>\n", stderr)
    exit(2)
}
let sourceURL = URL(fileURLWithPath: CommandLine.arguments[1])
guard let source = NSImage(contentsOf: sourceURL) else {
    fputs("Cannot load icon source: \(sourceURL.path)\n", stderr)
    exit(1)
}
let directory = URL(fileURLWithPath: CommandLine.arguments[2])
try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
        NSGraphicsContext.current?.imageInterpolation = .high
        source.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels),
                    from: .zero, operation: .copy, fraction: 1)
        NSGraphicsContext.restoreGraphicsState()
        let suffix = scale == 2 ? "@2x" : ""
        let url = directory.appendingPathComponent("icon_\(size)x\(size)\(suffix).png")
        try bitmap.representation(using: .png, properties: [:])!.write(to: url)
    }
}
