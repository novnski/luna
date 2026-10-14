// IOS-02 scene restoration: which Mac, project, filter, search and task each
// window had open, so rotation, split changes and a relaunch reopen exactly
// that task. Contract: docs/pending-tasks/ios-parity/ios-02/README.md.
//
// This is local presentation state. It names host records by the ids the
// Mac gave them and never copies their content: what a restored route shows
// is always the Mac's projection (live, stale or cached), or a truthful
// missing/offline state.

import Foundation

/// How the task list is narrowed, orthogonal to the project scope.
enum TaskFilter: String, CaseIterable, Identifiable {
    /// Every task that is not archived.
    case all
    /// Waiting for the user: needs approval or input, or failed.
    case attention
    case working
    /// The Mac reports activity nobody has seen yet.
    case unread
    /// Archived tasks only.
    case archived

    var id: String { rawValue }

    var label: String {
        switch self {
        case .all: "All"
        case .attention: "Needs you"
        case .working: "Working"
        case .unread: "Unread"
        case .archived: "Archived"
        }
    }
}

/// One scene's restorable route. Ids only; bounded on load.
struct ShellSceneRecord: Codable, Equatable {
    /// The saved Mac (`PairedMacMetadata.id`, a SHA-256 hex string).
    var macId: String?
    /// A project id on that Mac; nil is every project.
    var projectId: String?
    var filter: String
    var query: String
    /// The open task's host id on that Mac.
    var taskId: String?
    var updatedAtMs: Int64
}

/// `luna.shell.v1` in preferences: one record per scene, the most recently
/// changed one first in line for a scene the system did not restore (a
/// relaunch after the app was ended in the foreground or swiped away).
enum ShellRestoration {
    static let key = "luna.shell.v1"
    static let maxScenes = 8
    static let maxIdBytes = 256
    static let maxQueryCharacters = 200

    private struct Stored: Codable {
        var version: Int
        var scenes: [String: ShellSceneRecord]
        var last: String?
    }

    /// This scene's record, else the last changed one; validated.
    static func load(scene: String) -> ShellSceneRecord? {
        guard let stored = read() else { return nil }
        let record = stored.scenes[scene] ?? stored.last.flatMap { stored.scenes[$0] }
        return record.map(validated)
    }

    static func save(_ record: ShellSceneRecord, scene: String) {
        var stored = read() ?? Stored(version: 1, scenes: [:], last: nil)
        stored.scenes[scene] = validated(record)
        stored.last = scene
        if stored.scenes.count > maxScenes {
            let keep = stored.scenes.sorted { $0.value.updatedAtMs > $1.value.updatedAtMs }
                .prefix(maxScenes).map(\.key)
            stored.scenes = stored.scenes.filter { keep.contains($0.key) }
        }
        if let data = try? JSONEncoder().encode(stored) {
            UserDefaults.standard.set(data, forKey: key)
        }
    }

    /// A record read from storage, with anything malformed dropped: a
    /// task id without a well-formed Mac id is never restored, because it
    /// could name another Mac's task.
    static func validated(_ record: ShellSceneRecord) -> ShellSceneRecord {
        var record = record
        if let mac = record.macId, !(mac.count == 64 && mac.allSatisfy(\.isHexDigit)) {
            record.macId = nil
        }
        if record.macId == nil {
            record.taskId = nil
            record.projectId = nil
        }
        if let task = record.taskId, task.isEmpty || task.utf8.count > maxIdBytes {
            record.taskId = nil
        }
        if let project = record.projectId, project.isEmpty || project.utf8.count > maxIdBytes {
            record.projectId = nil
        }
        if TaskFilter(rawValue: record.filter) == nil {
            record.filter = TaskFilter.all.rawValue
        }
        if record.query.count > maxQueryCharacters {
            record.query = String(record.query.prefix(maxQueryCharacters))
        }
        return record
    }

    private static func read() -> Stored? {
        guard let data = UserDefaults.standard.data(forKey: key), data.count < 262_144,
              let stored = try? JSONDecoder().decode(Stored.self, from: data), stored.version == 1
        else { return nil }
        return stored
    }
}
