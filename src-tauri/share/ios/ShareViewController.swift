// Snorkle's share extension for iPadOS (owner request 2026-11-25): "Snorkle"
// in the share sheet. The web link shared to it (common/Links.swift) is
// queued for the app in the App Group's user defaults, which the app reads
// whenever it becomes active (crates/snorkle-content/src/ios/share.rs), and
// opens in a new tab. The sheet says so and offers to open Snorkle at once.
//
// Opening Snorkle from here: Apple documents no way for a share extension to
// open its own app (NSExtensionContext.open is for Today and widget
// extensions, and answers false here). The extension tries it, then the
// workaround other browsers use: it finds the application object through the
// responder chain and calls openURL:options:completionHandler: on it, with
// the app's link (snorkle://open-url?url=…). The link leaves the queue first
// (the app gets it by its address instead) and goes back if Snorkle cannot be
// opened, so it still opens the next time Snorkle comes to the front.
// Whether App Review accepts the workaround is not known (OPEN_QUESTIONS).

import UIKit

/// The App Group's list of links waiting for Snorkle; nil when the extension
/// was signed without the App Group (the app would never see the list).
final class LinkQueue {
    /// The most links kept waiting; the oldest go first.
    private static let limit = 20
    private let defaults: UserDefaults

    init?() {
        guard FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: snorkleAppGroup) != nil,
              let defaults = UserDefaults(suiteName: snorkleAppGroup)
        else { return nil }
        self.defaults = defaults
    }

    func add(_ link: URL) {
        var list = (defaults.stringArray(forKey: snorkleQueueKey) ?? []).filter { $0 != link.absoluteString }
        list.append(link.absoluteString)
        defaults.set(Array(list.suffix(Self.limit)), forKey: snorkleQueueKey)
    }

    func remove(_ link: URL) {
        let list = (defaults.stringArray(forKey: snorkleQueueKey) ?? []).filter { $0 != link.absoluteString }
        defaults.set(list, forKey: snorkleQueueKey)
    }
}

final class ShareViewController: UIViewController {
    private let heading = UILabel()
    private let detail = UILabel()
    private let openButton = UIButton(configuration: .filled())
    private let doneButton = UIButton(configuration: .gray())
    private var link: URL?
    private var queue: LinkQueue?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        preferredContentSize = CGSize(width: 400, height: 200)
        heading.font = .preferredFont(forTextStyle: .headline)
        heading.text = "Sending to Snorkle…"
        detail.font = .preferredFont(forTextStyle: .subheadline)
        detail.textColor = .secondaryLabel
        detail.numberOfLines = 0
        openButton.configuration?.title = "Open Snorkle"
        doneButton.configuration?.title = "Done"
        openButton.isEnabled = false
        openButton.addAction(UIAction { [weak self] _ in self?.openSnorkle() }, for: .primaryActionTriggered)
        doneButton.addAction(UIAction { [weak self] _ in self?.finish() }, for: .primaryActionTriggered)
        let buttons = UIStackView(arrangedSubviews: [doneButton, openButton])
        buttons.spacing = 12
        buttons.distribution = .fillEqually
        let stack = UIStackView(arrangedSubviews: [heading, detail, buttons])
        stack.axis = .vertical
        stack.spacing = 14
        stack.setCustomSpacing(22, after: detail)
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.layoutMarginsGuide.leadingAnchor, constant: 8),
            stack.trailingAnchor.constraint(equalTo: view.layoutMarginsGuide.trailingAnchor, constant: -8),
            stack.centerYAnchor.constraint(equalTo: view.centerYAnchor),
        ])
        Task { await receive() }
    }

    /// Takes the link and queues it.
    private func receive() async {
        guard let link = await sharedLink(from: extensionContext) else {
            show("No web link to send", "Snorkle takes web addresses (http and https).", canOpen: false)
            return
        }
        self.link = link
        queue = LinkQueue()
        if let queue {
            queue.add(link)
            show("Sent to Snorkle", "\(siteName(link)) opens in a new tab the next time you switch to Snorkle.", canOpen: true)
        } else {
            // Signed without the App Group: only opening Snorkle can take it there.
            show("Send to Snorkle", "Open Snorkle to see \(siteName(link)) in a new tab.", canOpen: true)
        }
    }

    private func show(_ title: String, _ text: String, canOpen: Bool) {
        heading.text = title
        detail.text = text
        openButton.isEnabled = canOpen
    }

    private func openSnorkle() {
        guard let link, let target = appLink(for: link) else { return }
        openButton.isEnabled = false
        queue?.remove(link)
        openApp(target) { [weak self] opened in
            guard let self else { return }
            if opened {
                self.finish()
                return
            }
            if let queue = self.queue {
                queue.add(link)
                self.show("Sent to Snorkle", "Snorkle couldn't be opened from here. \(siteName(link)) opens in a new tab the next time you switch to Snorkle.", canOpen: false)
            } else {
                self.show("Couldn't open Snorkle", "Snorkle couldn't be opened from here.", canOpen: false)
            }
        }
    }

    /// Opens `url` in Snorkle: the documented call first, then the
    /// responder-chain workaround (see the top of this file).
    private func openApp(_ url: URL, done: @escaping (Bool) -> Void) {
        let viaApplication = { [weak self] in
            let selector = NSSelectorFromString("openURL:options:completionHandler:")
            var next: UIResponder? = self
            while let responder = next {
                if responder.isKind(of: UIApplication.self), responder.responds(to: selector),
                   let method = responder.method(for: selector) {
                    typealias Open = @convention(c) (AnyObject, Selector, NSURL, NSDictionary, (@convention(block) (Bool) -> Void)?) -> Void
                    let open = unsafeBitCast(method, to: Open.self)
                    let completion: @convention(block) (Bool) -> Void = { ok in DispatchQueue.main.async { done(ok) } }
                    open(responder, selector, url as NSURL, NSDictionary(), completion)
                    return
                }
                next = responder.next
            }
            done(false)
        }
        guard let context = extensionContext else {
            viaApplication()
            return
        }
        context.open(url) { opened in
            DispatchQueue.main.async { opened ? done(true) : viaApplication() }
        }
    }

    private func finish() {
        extensionContext?.completeRequest(returningItems: nil, completionHandler: nil)
    }
}
