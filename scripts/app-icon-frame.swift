#!/usr/bin/env swift
// Frames an icon Icon Composer exported edge to edge the way macOS frames app
// icons: 824 of every 1024 points, centered, over a soft shadow.
//
//   scripts/app-icon-frame.swift <body.png> <out.png> <size>
import AppKit

let arguments = CommandLine.arguments
guard arguments.count == 4, let body = NSImage(contentsOfFile: arguments[1]),
    let size = Int(arguments[3])
else {
    FileHandle.standardError.write("usage: app-icon-frame.swift <body.png> <out.png> <size>\n".data(using: .utf8)!)
    exit(2)
}
let scale = CGFloat(size) / 1024
let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
    bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let shadow = NSShadow()
shadow.shadowColor = NSColor(white: 0, alpha: 0.3)
shadow.shadowOffset = NSSize(width: 0, height: -10 * scale)
shadow.shadowBlurRadius = 10 * scale
shadow.set()
body.draw(in: NSRect(x: 100 * scale, y: 100 * scale, width: 824 * scale, height: 824 * scale))
NSGraphicsContext.restoreGraphicsState()
try rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: arguments[2]))
