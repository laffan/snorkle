// Snorkle's share extensions (owner request 2026-11-25), iPadOS and macOS:
// what both take from the share sheet and how they name it to the app.
// Built into each extension (src-tauri/share/ios: the Xcode target from
// src-tauri/ios/project.yml; src-tauri/share/macos: scripts/macos-share-extension.sh).
//
// Only a web link is taken: a URL shared as such, else the first web link in
// shared text. The app checks it again (snorkle_core::input) and opens it
// through its navigation policy, as it does links from other apps.
// SECURITY-REVIEW: docs/SECURITY_REVIEW.md, "Share extensions".

import Foundation
import UniformTypeIdentifiers

/// The App Group the iPadOS extension leaves links in and the key they wait
/// under: the same as `snorkle_core::input::{SHARE_APP_GROUP, SHARED_LINKS_KEY}`.
let snorkleAppGroup = "group.com.laffan.snorkle"
let snorkleQueueKey = "sharedLinks"
/// The longest link taken (`snorkle_core::input::MAX_INPUT_LEN`).
let maxLinkBytes = 8 * 1024

/// A web page's address: http(s), with a host, without a user name or
/// password, at most `maxLinkBytes` long; else nil.
func webLink(_ url: URL) -> URL? {
    guard let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
          let host = url.host, !host.isEmpty, url.user == nil, url.password == nil,
          url.absoluteString.utf8.count <= maxLinkBytes
    else { return nil }
    return url
}

/// The first web link in shared text (a message, a note).
func firstLink(in text: String) -> URL? {
    guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return nil }
    let range = NSRange(text.startIndex..., in: text)
    for match in detector.matches(in: text, options: [], range: range) {
        if let url = match.url, let link = webLink(url) { return link }
    }
    return nil
}

/// The web link in what was shared: a URL attachment, else the first link in
/// shared text (attachments, then the item's own text); nil if there is none.
func sharedLink(from context: NSExtensionContext?) async -> URL? {
    let items = (context?.inputItems as? [NSExtensionItem]) ?? []
    let providers = items.flatMap { $0.attachments ?? [] }
    for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
        if let url = try? await provider.loadItem(forTypeIdentifier: UTType.url.identifier) as? URL,
           let link = webLink(url) {
            return link
        }
    }
    for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
        if let text = try? await provider.loadItem(forTypeIdentifier: UTType.plainText.identifier) as? String,
           let link = firstLink(in: text) {
            return link
        }
    }
    for item in items {
        if let text = item.attributedContentText?.string, let link = firstLink(in: text) { return link }
    }
    return nil
}

/// The app's own link for a page: `snorkle://open-url?url=<encoded>`
/// (`snorkle_core::input::from_app_link`).
func appLink(for link: URL) -> URL? {
    let unreserved = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~")
    guard let encoded = link.absoluteString.addingPercentEncoding(withAllowedCharacters: unreserved) else { return nil }
    return URL(string: "snorkle://open-url?url=" + encoded)
}

/// What the sheet says about a link: its site.
func siteName(_ link: URL) -> String {
    let host = link.host ?? link.absoluteString
    return host.hasPrefix("www.") ? String(host.dropFirst(4)) : host
}
