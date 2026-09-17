// Draw the app's original icon at the sizes required by iconutil.
import AppKit
import Foundation

let directory = URL(fileURLWithPath: CommandLine.arguments[1])
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
        let transform = NSAffineTransform()
        transform.scale(by: CGFloat(pixels) / 1024)
        transform.concat()
        let background = NSBezierPath(roundedRect: NSRect(x: 55, y: 55, width: 914, height: 914), xRadius: 206, yRadius: 206)
        NSGradient(starting: NSColor(calibratedRed: 0.34, green: 0.45, blue: 0.66, alpha: 1), ending: NSColor(calibratedRed: 0.18, green: 0.25, blue: 0.40, alpha: 1))!.draw(in: background, angle: -90)
        NSColor(calibratedWhite: 0, alpha: 0.12).setFill()
        NSBezierPath(roundedRect: NSRect(x: 250, y: 183, width: 544, height: 658), xRadius: 52, yRadius: 52).fill()
        NSColor(calibratedWhite: 0.97, alpha: 1).setFill()
        NSBezierPath(roundedRect: NSRect(x: 240, y: 200, width: 544, height: 650), xRadius: 52, yRadius: 52).fill()
        NSColor(calibratedRed: 0.36, green: 0.46, blue: 0.62, alpha: 1).setFill()
        for (y, width) in [(655, 310), (525, 310), (395, 205)] {
            NSBezierPath(roundedRect: NSRect(x: 335, y: y, width: width, height: 28), xRadius: 14, yRadius: 14).fill()
        }
        NSGraphicsContext.restoreGraphicsState()
        let suffix = scale == 2 ? "@2x" : ""
        let url = directory.appendingPathComponent("icon_\(size)x\(size)\(suffix).png")
        try bitmap.representation(using: .png, properties: [:])!.write(to: url)
    }
}
