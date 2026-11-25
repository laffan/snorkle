// Snorkle's share extension for macOS (owner request 2026-11-25): "Snorkle"
// in the Share menu. The web link shared to it (common/Links.swift) is sent to
// Snorkle at once, in the background, with the app's own link
// (snorkle://open-url?url=…, snorkle_core::input::from_app_link), so it opens
// in a new tab while the app the link came from stays in front. The popover
// says so and offers to bring Snorkle to the front. No App Group is needed:
// on the Mac an extension can open its app (NSWorkspace).
// Built by scripts/macos-share-extension.sh.

import AppKit

final class ShareViewController: NSViewController {
    private let heading = NSTextField(labelWithString: "Sending to Snorkle…")
    private let detail = NSTextField(wrappingLabelWithString: "")
    private lazy var openButton = NSButton(title: "Open Snorkle", target: self, action: #selector(openSnorkle))
    private lazy var doneButton = NSButton(title: "Done", target: self, action: #selector(finish))

    override var nibName: NSNib.Name? { nil }

    override func loadView() {
        view = NSView(frame: NSRect(x: 0, y: 0, width: 360, height: 130))
        heading.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        detail.textColor = .secondaryLabelColor
        detail.preferredMaxLayoutWidth = 320
        openButton.keyEquivalent = "\r"
        openButton.isEnabled = false
        doneButton.keyEquivalent = "\u{1b}"
        let buttons = NSStackView(views: [doneButton, openButton])
        buttons.spacing = 8
        let stack = NSStackView(views: [heading, detail, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: view.topAnchor, constant: 18),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: view.bottomAnchor, constant: -18),
        ])
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        Task { await receive() }
    }

    /// Snorkle.app, the bundle this extension is in
    /// (Snorkle.app/Contents/PlugIns/SnorkleShare.appex), or nil.
    private var appURL: URL? {
        let app = Bundle.main.bundleURL.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        return app.pathExtension == "app" ? app : nil
    }

    /// Takes the link and sends it to Snorkle without bringing it forward.
    private func receive() async {
        guard let link = await sharedLink(from: extensionContext), let target = appLink(for: link) else {
            show("No web link to send", "Snorkle takes web addresses (http and https).", canOpen: false)
            return
        }
        let quietly = NSWorkspace.OpenConfiguration()
        quietly.activates = false
        let sent: (Error?) -> Void = { [weak self] error in
            DispatchQueue.main.async {
                if error == nil {
                    self?.show("Sent to Snorkle", "\(siteName(link)) is open in a new tab in Snorkle.", canOpen: true)
                } else {
                    self?.show("Couldn't send to Snorkle", "Snorkle could not be reached.", canOpen: true)
                }
            }
        }
        // This copy of Snorkle (the one the extension is in), else whichever
        // app the system has for the link.
        guard let app = appURL else {
            NSWorkspace.shared.open(target, configuration: quietly) { _, error in sent(error) }
            return
        }
        NSWorkspace.shared.open([target], withApplicationAt: app, configuration: quietly) { _, error in
            if error == nil {
                sent(nil)
            } else {
                NSWorkspace.shared.open(target, configuration: quietly) { _, error in sent(error) }
            }
        }
    }

    private func show(_ title: String, _ text: String, canOpen: Bool) {
        heading.stringValue = title
        detail.stringValue = text
        openButton.isEnabled = canOpen
    }

    @objc private func openSnorkle() {
        let forward = NSWorkspace.OpenConfiguration()
        forward.activates = true
        if let app = appURL {
            NSWorkspace.shared.openApplication(at: app, configuration: forward) { _, _ in }
        }
        finish()
    }

    @objc private func finish() {
        extensionContext?.completeRequest(returningItems: nil, completionHandler: nil)
    }
}
