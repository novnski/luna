import XCTest
@testable import Luna

/// IOS-02 local-state contracts at their own boundary, as tables: the
/// one-time appearance migration rule (F11), which `LunaApp.init` applies
/// through `AppearanceMigration.migrate()` on every launch, and the
/// validation of a route read back from preferences (F4/F8). Both are pure
/// functions the app's own startup and restore paths call. What the stored choice renders, and the
/// restored route against a real Mac, belong to
/// `LunaUITests/AdaptiveShellUITests`; the upgrade and downgrade bytes to
/// the data-continuity QA. Failure contract:
/// docs/pending-tasks/ios-parity/ios-02/README.md.
final class ShellPreferencesTests: XCTestCase {
    func testAppearanceMigrationKeepsUpgradersDarkAndFreshInstallsOnSystem() {
        let cases: [(stored: String?, prior: Bool, mode: AppearanceMode, write: Bool, why: String)] = [
            (nil, true, .dark, true, "an earlier build forced dark; an upgrade keeps that look once"),
            (nil, false, .system, true, "a fresh install follows iOS, the desktop default"),
            ("light", true, .light, false, "a stored choice is kept as is"),
            ("system", true, .system, false, "a stored System is never re-migrated to dark"),
            ("dark", false, .dark, false, "a stored choice is kept as is"),
            ("Dark", true, .dark, true, "an unreadable value is decided again and rewritten"),
            ("sepia", false, .system, true, "an unknown value is decided again and rewritten"),
            ("", true, .dark, true, "an empty value is decided again and rewritten"),
        ]
        for item in cases {
            let resolved = AppearanceMigration.resolve(stored: item.stored, priorInstall: item.prior)
            XCTAssertEqual(resolved.mode, item.mode, item.why)
            XCTAssertEqual(resolved.write, item.write, item.why)
        }
    }

    func testRestoredRoutesDropAnythingThatCouldNameAnotherMacsTask() {
        let mac = String(repeating: "a1", count: 32)
        func record(mac: String?, project: String? = "project-1", filter: String = "unread",
                    query: String = "plan", task: String? = "task-1") -> ShellSceneRecord {
            ShellSceneRecord(macId: mac, projectId: project, filter: filter, query: query,
                             taskId: task, updatedAtMs: 1)
        }
        let longId = String(repeating: "t", count: ShellRestoration.maxIdBytes + 1)
        let longQuery = String(repeating: "q", count: ShellRestoration.maxQueryCharacters + 5)
        let cases: [(input: ShellSceneRecord, expected: ShellSceneRecord, why: String)] = [
            (record(mac: mac), record(mac: mac), "a well-formed route is restored unchanged"),
            (record(mac: "not-a-saved-mac-id"), record(mac: nil, project: nil, task: nil),
             "a malformed Mac id drops the Mac and every id that belonged to it"),
            (record(mac: String(repeating: "g", count: 64)), record(mac: nil, project: nil, task: nil),
             "a 64-character id that is not hex is not a saved Mac"),
            (record(mac: nil), record(mac: nil, project: nil, task: nil),
             "a task or project without its Mac is never restored"),
            (record(mac: mac, task: ""), record(mac: mac, task: nil), "an empty task id is dropped"),
            (record(mac: mac, task: longId), record(mac: mac, task: nil), "an oversized task id is dropped"),
            (record(mac: mac, project: longId), record(mac: mac, project: nil), "an oversized project id is dropped"),
            (record(mac: mac, filter: "pinned"), record(mac: mac, filter: "all"), "an unknown filter falls back to All"),
            (record(mac: mac, query: longQuery),
             record(mac: mac, query: String(repeating: "q", count: ShellRestoration.maxQueryCharacters)),
             "a search longer than the bound is cut to it"),
        ]
        for item in cases {
            XCTAssertEqual(ShellRestoration.validated(item.input), item.expected, item.why)
        }
    }
}
