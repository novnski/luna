import Foundation
import Network

/// RM-03 network lifecycle adapter: reports the device's path to the runtime.
/// The core decides what a change means (close and wait, or reconnect now);
/// this only observes `NWPathMonitor` and hops to the main actor.
final class NetworkPathObserver: @unchecked Sendable {
    private let monitor = NWPathMonitor()
    private let queue = DispatchQueue(label: "luna.remote-network-path")

    /// `changed` runs on the main actor with whether a usable path exists,
    /// once at start and then on every change.
    init(changed: @escaping @Sendable @MainActor (Bool) -> Void) {
        monitor.pathUpdateHandler = { path in
            let available = path.status == .satisfied
            Task { @MainActor in changed(available) }
        }
        monitor.start(queue: queue)
    }

    deinit { monitor.cancel() }
}

/// RM-03 saved projections, one file per paired Mac in
/// `Application Support/LunaRemote/`. The bytes come from and go back to the
/// core, which keys them by host and profile and refuses anything else, so
/// another Mac's data is never shown. The file holds task titles and
/// transcript previews but no credential: it is file-protected, excluded from
/// backups, and deleted when this phone forgets the Mac or must pair again.
/// It is a new store: the edge `LunaDocs` cache and RM-02's preferences and
/// Keychain items are never read or rewritten.
enum RemoteTaskCache {
    /// The core's own bound (`luna_sdk::MAX_CACHE_BYTES`).
    static let maxBytes = 4 * 1024 * 1024

    /// `account` is the saved Mac's id (a SHA-256 of its device, profile and
    /// scope), so it is a safe file name.
    static func url(account: String) -> URL? {
        guard account.count == 64, account.allSatisfy({ $0.isHexDigit }),
              let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
        else { return nil }
        return base.appendingPathComponent("LunaRemote", isDirectory: true)
            .appendingPathComponent("tasks-\(account).json")
    }

    static func load(account: String) -> Data? {
        guard let url = url(account: account),
              let size = (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize, size <= maxBytes
        else { return nil }
        return try? Data(contentsOf: url)
    }

    static func save(_ data: Data, account: String) {
        guard !data.isEmpty, data.count <= maxBytes, let url = url(account: account) else { return }
        var directory = url.deletingLastPathComponent()
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try directory.setResourceValues(values)
            try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch {
            // A cache is only a convenience: the next run starts empty.
        }
    }

    static func remove(account: String) {
        guard let url = url(account: account) else { return }
        try? FileManager.default.removeItem(at: url)
    }
}
