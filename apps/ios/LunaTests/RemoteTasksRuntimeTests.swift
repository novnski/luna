import Observation
import XCTest
@testable import Luna

/// RM-03 P12 at the native boundary: the shipped `PairedMacsModel`,
/// `RemoteHostSession`, `PinnedSocketTransport`, `RemoteTaskCache` and
/// lifecycle adapters, the UniFFI core and the shared SDK against a real
/// engine gateway. The engine campaign
/// `remote_access::gateway::mobile_runtime_follows_mac_tasks_...`
/// (`LUNA_NATIVE_RUNTIME_FIXTURE`, driven by
/// `scripts/ios/qa-native-session-sim.py --campaign rm03`) mints a
/// `luna://pair` code with CreateRemotePairing, writes it into this app's
/// Documents, changes and restarts the Mac on cue and checks the public
/// result. Without the fixture the test skips. Failure contract:
/// docs/pending-tasks/control-this-mac/rm-03/README.md.
@MainActor
final class RemoteTasksRuntimeTests: XCTestCase {
    private var documents: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }
    private var onMain = true

    func testMacTasksFollowSuspendNetworkRestartAndRepair() async throws {
        let fixture = documents.appendingPathComponent("rm03-runtime.json")
        guard FileManager.default.fileExists(atPath: fixture.path) else {
            throw XCTSkip("The engine's RM-03 runtime fixture has not been supplied")
        }
        let fields = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: fixture)) as? [String: Any])
        let code = try XCTUnwrap(fields["code"] as? String)
        let name = try XCTUnwrap(fields["name"] as? String)
        XCTAssertTrue(name.hasPrefix("RM03 QA "), "Only an owned disposable host fixture may be used")
        let deviceId = try XCTUnwrap(fields["deviceId"] as? String)
        let rows = try XCTUnwrap(fields["tasks"] as? [[String: Any]])
        let taskIds = Set(rows.compactMap { $0["taskId"] as? String })
        let followId = try XCTUnwrap(fields["followTaskId"] as? String)
        let entryIds = try XCTUnwrap(fields["entryIds"] as? [String])
        let renameId = try XCTUnwrap(fields["renameTaskId"] as? String)
        let renamedTitle = try XCTUnwrap(fields["renamedTitle"] as? String)
        let awayId = try XCTUnwrap(fields["awayTaskId"] as? String)
        let awayTitle = try XCTUnwrap(fields["awayTitle"] as? String)
        let core = coreIdentity()

        var model = PairedMacsModel()
        for stale in model.hosts where stale.name.hasPrefix("RM03 QA ") { model.forget(stale) }

        // P12.1: pair from the host's luna://pair link through the app's own
        // enrollment; the runtime opens after RM-02's verified connect.
        model.prepare(code: code)
        XCTAssertNotNil(model.preview, "the link names a host to confirm")
        model.confirm()
        try await until("paired and connected") { model.phase == "connected" }
        let host = try XCTUnwrap(model.hosts.first { $0.name == name })

        // P12.2: the same Mac task identities, live.
        try await until("a live task list") { model.remoteTasks?.freshness == .live }
        let first = try XCTUnwrap(model.remoteTasks)
        XCTAssertEqual(first.deviceId, deviceId)
        XCTAssertEqual(Set(first.tasks.map(\.taskId)), taskIds)
        XCTAssertTrue((model.liveStatus ?? "").hasPrefix("Live · host \(deviceId.prefix(8))"))

        // P12.3: the opened task's history by the host's entry ids.
        model.openTask(followId)
        try await until("a live task detail") {
            model.openTask?.freshness == .live && model.openTask?.entries.count == entryIds.count
        }
        XCTAssertEqual(model.openTask?.entries.map(\.entryId), entryIds)

        // P12.4: a Mac rename arrives as a delta.
        try mark("rm03-native-live.json")
        try await cue("rm03-host-renamed.json")
        try await until("the rename arrives") { title(model.remoteTasks, renameId) == renamedTitle }
        let liveDelta = model.remoteTasks?.lastChange == .delta

        // P12.5: backgrounding suspends, saves the projections, labels stale.
        model.backgrounded()
        try await until("stale while backgrounded") { model.remoteTasks?.freshness == .stale }
        let cacheFile = try XCTUnwrap(RemoteTaskCache.url(account: host.id))
        XCTAssertTrue(FileManager.default.fileExists(atPath: cacheFile.path), "suspending saved the projections")
        try mark("rm03-native-suspended.json")
        try await cue("rm03-host-changed.json")

        // P12.6: foregrounding re-verifies; the change made while away is
        // repaired by snapshot.
        model.foregrounded()
        try await until("repaired after foreground") {
            model.remoteTasks?.freshness == .live && title(model.remoteTasks, awayId) == awayTitle
        }
        let repaired = model.remoteTasks?.lastChange == .snapshot

        // P12.7: no network closes and waits; its return resumes the cursor.
        model.networkChanged(available: false)
        try await until("waiting without a network") {
            model.remoteTasks?.freshness == .stale && model.liveStatus == "Waiting for a network connection"
        }
        model.networkChanged(available: true)
        try await until("live after the network returns") { model.remoteTasks?.freshness == .live }
        let currentAfterNetwork = model.remoteTasks?.lastChange == .current

        // P12.8: restart the phone's model. The saved projections show as
        // cached before any connection, then are confirmed without a resend.
        let beforeRestart = try XCTUnwrap(model.remoteTasks)
        model.disconnect()
        model = PairedMacsModel()
        model.connect(host)
        let cached = try XCTUnwrap(model.remoteTasks, "the saved projections show before any connection")
        let cachedAfterRestart = cached.freshness == .cached && cached.tasks == beforeRestart.tasks
        try await until("confirmed after restart") { model.remoteTasks?.freshness == .live }
        let confirmedAfterRestart = model.remoteTasks?.lastChange == .current

        // P12.9: the Mac restarts; the runtime reconnects by itself and the
        // new generation is repaired by snapshot with the same identities.
        try mark("rm03-native-restarted.json")
        try await cue("rm03-host-restarted.json", within: 120)
        try await until("repaired after the Mac restarted", within: 120) {
            model.remoteTasks?.freshness == .live && model.remoteTasks?.generation != beforeRestart.generation
        }
        let reborn = try XCTUnwrap(model.remoteTasks)
        let repairedAfterMacRestart = reborn.lastChange == .snapshot && Set(reborn.tasks.map(\.taskId)) == taskIds

        // P12.10: Forget removes the saved projections with the access.
        model.forget(host)
        let cacheRemoved = !FileManager.default.fileExists(atPath: cacheFile.path)

        try JSONSerialization.data(withJSONObject: [
            "pairedFromLink": true, "sameTaskIdentities": true, "detailEntries": true,
            "liveDelta": liveDelta, "staleWhileBackgrounded": true, "repairedAfterForeground": repaired,
            "waitingWithoutNetwork": true, "currentAfterNetwork": currentAfterNetwork,
            "cachedAfterRestart": cachedAfterRestart, "confirmedAfterRestart": confirmedAfterRestart,
            "repairedAfterMacRestart": repairedAfterMacRestart, "resultsOnMain": onMain,
            "cacheRemovedOnForget": cacheRemoved, "deviceId": first.deviceId,
            "generations": [first.generation, reborn.generation],
            "core": ["facadeVersion": Int(core.facadeVersion), "coreVersion": core.coreVersion,
                     "sdkVersion": core.sdkVersion, "apiVersion": Int(core.apiVersion),
                     "target": core.target, "debugBuild": core.debugBuild],
        ] as [String: Any]).write(to: documents.appendingPathComponent("rm03-native-result.json"), options: .atomic)
        XCTAssertTrue(liveDelta && repaired && currentAfterNetwork && cachedAfterRestart
            && confirmedAfterRestart && repairedAfterMacRestart && cacheRemoved && onMain)
    }

    /// Waits until `condition` holds, re-checking whenever the observed model
    /// changes (and at least every half second); fails after `seconds`.
    private func until(_ what: String, within seconds: Double = 30, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            guard Date() < deadline else {
                XCTFail("timed out waiting: \(what)")
                throw Timeout(what: what)
            }
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let once = Once(continuation)
                withObservationTracking { _ = condition() } onChange: { once.resume() }
                Task {
                    try? await Task.sleep(for: .milliseconds(500))
                    once.resume()
                }
            }
        }
        if !Thread.isMainThread { onMain = false }
    }

    private func mark(_ name: String) throws {
        try Data("{}".utf8).write(to: documents.appendingPathComponent(name), options: .atomic)
    }

    private func cue(_ name: String, within seconds: TimeInterval = 60) async throws {
        let file = documents.appendingPathComponent(name)
        await fulfillment(of: [XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            FileManager.default.fileExists(atPath: file.path)
        }, object: nil)], timeout: seconds)
    }
}

private func title(_ tasks: RemoteTasksProjection?, _ taskId: String) -> String? {
    tasks?.tasks.first { $0.taskId == taskId }?.title
}

private struct Timeout: Error {
    let what: String
}

/// Resumes its continuation once.
private final class Once: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Never>?

    init(_ continuation: CheckedContinuation<Void, Never>) {
        self.continuation = continuation
    }

    func resume() {
        let pending = lock.withLock { () -> CheckedContinuation<Void, Never>? in
            defer { continuation = nil }
            return continuation
        }
        pending?.resume()
    }
}
