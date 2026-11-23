import AppKit
import UniformTypeIdentifiers

@objc(MonodriveShare)
final class MonodriveShare: NSViewController {
    override func loadView() {
        view = NSView(frame: .zero)
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        let providers = (extensionContext?.inputItems as? [NSExtensionItem] ?? [])
            .flatMap { $0.attachments ?? [] }
            .filter { $0.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) }
        var paths = [String?](repeating: nil, count: providers.count)
        var failed: Error?
        let group = DispatchGroup()
        for (i, provider) in providers.enumerated() {
            group.enter()
            _ = provider.loadObject(ofClass: URL.self) { url, error in
                DispatchQueue.main.async {
                    if let url, url.isFileURL { paths[i] = url.path }
                    failed = failed ?? error
                    group.leave()
                }
            }
        }
        group.notify(queue: .main) {
            if let failed {
                self.extensionContext?.cancelRequest(withError: failed)
            } else {
                self.handOff(paths.compactMap { $0 })
            }
        }
    }

    private func handOff(_ paths: [String]) {
        // This extension sits at Contents/PlugIns/Share.appex in the app.
        let app = Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        guard let url = Self.request(paths) else {
            extensionContext?.completeRequest(returningItems: nil)
            return
        }
        NSWorkspace.shared.open(
            [url], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration()
        ) { _, error in
            DispatchQueue.main.async {
                if let error {
                    self.extensionContext?.cancelRequest(withError: error)
                } else {
                    self.extensionContext?.completeRequest(returningItems: nil)
                }
            }
        }
    }

    /// `monodrive://localsend?path=…&path=…`, as `finder.rs` reads it.
    static func request(_ paths: [String]) -> URL? {
        guard !paths.isEmpty else { return nil }
        var unreserved = CharacterSet()
        unreserved.insert(
            charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~/")
        let query = paths
            .compactMap { $0.addingPercentEncoding(withAllowedCharacters: unreserved) }
            .map { "path=\($0)" }
            .joined(separator: "&")
        return URL(string: "monodrive://localsend?\(query)")
    }
}
