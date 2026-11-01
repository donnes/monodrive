// Draws a local HTML page to a PNG at twice its size, through WebKit, with no
// browser to install and no capture of the screen. The window never appears.
//
//   swift scripts/html-to-png.swift <page.html> <out.png> <width> <height>
import AppKit
import WebKit

let arguments = CommandLine.arguments
guard arguments.count == 5, let width = Double(arguments[3]), let height = Double(arguments[4]) else {
    print("usage: html-to-png.swift <page.html> <out.png> <width> <height>")
    exit(1)
}

final class Renderer: NSObject, WKNavigationDelegate {
    let out: URL
    init(out: URL) { self.out = out }

    func webView(_ view: WKWebView, didFinish navigation: WKNavigation!) {
        view.takeSnapshot(with: nil) { image, error in
            guard let image, let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil),
                let png = NSBitmapImageRep(cgImage: cg).representation(using: .png, properties: [:])
            else { Self.fail("snapshot failed: \(String(describing: error))") }
            do { try png.write(to: self.out) } catch { Self.fail("\(error)") }
            print("\(self.out.path) \(cg.width)x\(cg.height)")
            exit(0)
        }
    }

    func webView(_ view: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        Self.fail("\(error)")
    }

    func webView(_ view: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        Self.fail("\(error)")
    }

    static func fail(_ message: String) -> Never {
        FileHandle.standardError.write(Data((message + "\n").utf8))
        exit(1)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)

// The same pixels on any display: a 1x screen gets a bigger frame at double zoom.
let scale = NSScreen.main?.backingScaleFactor ?? 1
let frame = NSRect(x: 0, y: 0, width: width * 2 / scale, height: height * 2 / scale)
let window = NSWindow(contentRect: frame, styleMask: .borderless, backing: .buffered, defer: false)
let view = WKWebView(frame: frame)
view.pageZoom = 2 / scale
window.contentView = view

let page = URL(fileURLWithPath: arguments[1])
let renderer = Renderer(out: URL(fileURLWithPath: arguments[2]))
view.navigationDelegate = renderer
view.loadFileURL(page, allowingReadAccessTo: page.deletingLastPathComponent())
app.run()
