// IOS-02 drafts: text typed for a Mac's task, kept on this phone across
// rotation, split changes, navigation, a deleted task and relaunch.
// Contract: docs/pending-tasks/ios-parity/ios-02/README.md.
//
// A draft is the user's words, never a Mac record: it is keyed by the saved
// Mac and the task's host id, stays on this phone, and is never sent by this
// store. Sending belongs to RM-04 (host admission) and IOS-03 (composer).

import Foundation
import Observation

@MainActor
@Observable
final class DraftBook {
    static let shared = DraftBook()

    /// At most this many drafts per Mac; the least recently edited one goes.
    static let maxDrafts = 100
    /// The largest draft kept, in UTF-8 bytes (the host's message bound).
    static let maxBytes = 64 * 1024

    struct Entry: Codable, Equatable {
        var text: String
        var updatedAtMs: Int64
    }

    private struct Stored: Codable {
        var version: Int
        var drafts: [String: Entry]
    }

    /// Loaded drafts per saved Mac id, read lazily; `revision` is what
    /// views observe, so reading a draft never mutates observed state.
    @ObservationIgnored private var loaded: [String: [String: Entry]] = [:]
    private var revision: UInt64 = 0

    private init() {}

    func text(account: String, taskId: String) -> String {
        _ = revision
        return entries(account)[taskId]?.text ?? ""
    }

    /// Saves at once (no debounce: a terminated app must not lose typing).
    /// Returns false, keeping the saved text, when `text` exceeds the bound.
    @discardableResult
    func setText(_ text: String, account: String, taskId: String) -> Bool {
        guard text.utf8.count <= Self.maxBytes else { return false }
        var drafts = entries(account)
        if text.isEmpty {
            guard drafts.removeValue(forKey: taskId) != nil else { return true }
        } else {
            guard drafts[taskId]?.text != text else { return true }
            drafts[taskId] = Entry(text: text, updatedAtMs: Int64(Date().timeIntervalSince1970 * 1000))
            if drafts.count > Self.maxDrafts,
               let oldest = drafts.filter({ $0.key != taskId }).min(by: { $0.value.updatedAtMs < $1.value.updatedAtMs }) {
                drafts.removeValue(forKey: oldest.key)
            }
        }
        loaded[account] = drafts
        revision &+= 1
        Self.write(drafts, account: account)
        return true
    }

    func discard(account: String, taskId: String) {
        setText("", account: account, taskId: taskId)
    }

    /// Forget on this phone removes the Mac's drafts with its access.
    func forget(account: String) {
        loaded[account] = nil
        revision &+= 1
        guard let url = Self.url(account: account) else { return }
        try? FileManager.default.removeItem(at: url)
    }

    private func entries(_ account: String) -> [String: Entry] {
        if let cached = loaded[account] { return cached }
        let read = Self.read(account: account) ?? [:]
        loaded[account] = read
        return read
    }

    /// `Application Support/LunaRemote/drafts-<saved Mac id>.json`, beside
    /// RM-03's projection cache and protected the same way.
    static func url(account: String) -> URL? {
        guard account.count == 64, account.allSatisfy({ $0.isHexDigit }),
              let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
        else { return nil }
        return base.appendingPathComponent("LunaRemote", isDirectory: true)
            .appendingPathComponent("drafts-\(account).json")
    }

    private static func read(account: String) -> [String: Entry]? {
        guard let url = url(account: account),
              let size = (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize,
              size <= (maxDrafts + 1) * maxBytes * 2,
              let data = try? Data(contentsOf: url),
              let stored = try? JSONDecoder().decode(Stored.self, from: data), stored.version == 1
        else { return nil }
        return stored.drafts.filter { !$0.key.isEmpty && $0.value.text.utf8.count <= maxBytes }
    }

    private static func write(_ drafts: [String: Entry], account: String) {
        guard let url = url(account: account) else { return }
        if drafts.isEmpty {
            try? FileManager.default.removeItem(at: url)
            return
        }
        var directory = url.deletingLastPathComponent()
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try directory.setResourceValues(values)
            let data = try JSONEncoder().encode(Stored(version: 1, drafts: drafts))
            try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch {
            // The in-memory draft stays; the next edit retries the write.
        }
    }
}
