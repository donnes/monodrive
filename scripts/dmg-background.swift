#!/usr/bin/env swift
// Draws assets/prod/dmg-background.tiff, the installer window, at 1x and 2x.
// Run it from the repo after changing it; scripts/release uses the tracked copy.
//
// Finder anchors the image at the window's top-left and draws icon labels in
// black even in Dark Mode, so it stays light. Newer macOS can cover about 68 pt
// of the window with its title pill, so the bottom 40 pt is only texture. Icon
// centers are in scripts/release (app 170,200; Applications 490,200).
import AppKit

let width = 660, height = 440, cell = 4

func gray(_ value: Int) -> NSColor {
    let v = CGFloat(value) / 255
    return NSColor(srgbRed: v, green: v, blue: v, alpha: 1)
}

func draw() {
    gray(236).setFill()
    NSRect(x: 0, y: 0, width: width, height: height).fill()
    // Square cells like the logo's, a little brighter behind the title and icons.
    for y in stride(from: 0, to: height, by: cell) {
        for x in stride(from: 0, to: width, by: cell) {
            let dx = (Double(x) - Double(width) / 2) / (Double(width) * 0.7)
            let dy = (Double(y) - 180) / (Double(height) * 0.8)
            let d = min(1, hypot(dx, dy))
            gray(Int((226 + 8 * (1 - d) - 6 * d).rounded())).setFill()
            NSRect(x: x, y: height - y - cell + 1, width: cell - 1, height: cell - 1).fill()
        }
    }

    let title = NSMutableAttributedString(
        string: "Welcome to ",
        attributes: [.font: NSFont.systemFont(ofSize: 28), .foregroundColor: gray(133)])
    title.append(
        NSAttributedString(
            string: "Monodrive",
            attributes: [
                .font: NSFont.systemFont(ofSize: 28, weight: .medium),
                .foregroundColor: gray(20),
            ]))
    title.draw(at: NSPoint(x: (Double(width) - title.size().width) / 2, y: Double(height) - 100))

    let arrow = NSBezierPath()
    arrow.lineWidth = 1.5
    arrow.lineCapStyle = .round
    arrow.lineJoinStyle = .round
    let middle = Double(height) - 200
    arrow.move(to: NSPoint(x: 266, y: middle))
    arrow.line(to: NSPoint(x: 394, y: middle))
    arrow.move(to: NSPoint(x: 387, y: middle + 6))
    arrow.line(to: NSPoint(x: 394, y: middle))
    arrow.line(to: NSPoint(x: 387, y: middle - 6))
    gray(160).setStroke()
    arrow.stroke()

    let hint = NSAttributedString(
        string: "Drag Monodrive into Applications to install",
        attributes: [.font: NSFont.systemFont(ofSize: 12), .foregroundColor: gray(120)])
    hint.draw(at: NSPoint(x: (Double(width) - hint.size().width) / 2, y: Double(height) - 378))
}

func rendered(scale: Int) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: width * scale, pixelsHigh: height * scale,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .calibratedRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = NSSize(width: width, height: height)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    draw()
    NSGraphicsContext.restoreGraphicsState()
    return rep
}

let tiff = NSBitmapImageRep.tiffRepresentationOfImageReps(
    in: [rendered(scale: 1), rendered(scale: 2)], using: .lzw, factor: 0)!
try tiff.write(to: URL(fileURLWithPath: "assets/prod/dmg-background.tiff"))
