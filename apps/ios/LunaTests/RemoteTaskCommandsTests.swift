import XCTest
@testable import Luna

/// RM-04 at the native boundary: the shipped enrollment (`PairedMacsModel`),
/// the shipped pinned socket transport, the UniFFI core's task commands and
/// outbox, and a real QA Mac running real providers. The orchestrator writes
/// `rm04-commands.json` into this app's Documents (a `luna://pair` code minted
/// with `tasks-operate`, the QA host's name and one of its project ids); the
/// test pairs, composes a task and its first message while suspended (a
/// draft behind its create), restarts the phone's session from the saved
/// outbox, lets the Mac admit and run them, stops a running turn, and writes
/// `rm04-native-result.json`. Without the fixture the test skips. Failure
/// contract: docs/pending-tasks/control-this-mac/rm-04/README.md (F17–F20).
@MainActor
final class RemoteTaskCommandsTests: XCTestCase {
    private var documents: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    func testPhoneCommandsReachTheMacOnceAcrossSuspensionRestartAndStop() async throws {
        let fixture = documents.appendingPathComponent("rm04-commands.json")
        guard FileManager.default.fileExists(atPath: fixture.path) else {
            throw XCTSkip("The RM-04 command fixture has not been supplied")
        }
        let fields = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: fixture)) as? [String: Any])
        let code = try XCTUnwrap(fields["code"] as? String)
        let name = try XCTUnwrap(fields["name"] as? String)
        XCTAssertTrue(name.hasPrefix("RM04 QA "), "Only an owned disposable QA host may be used")
        let projectId = try XCTUnwrap(fields["projectId"] as? String)
        let replyPrompt = fields["replyPrompt"] as? String ?? "Reply with exactly: RM04 native ok"
        let longPrompt = fields["longPrompt"] as? String
            ?? "Count slowly from 1 to 400, one number per line, and nothing else."

        // Pair through the app's own enrollment; its saved access then backs
        // a runtime session of this test (the model's own one is closed).
        let model = PairedMacsModel()
        for stale in model.hosts where stale.name.hasPrefix("RM04 QA ") { model.forget(stale) }
        model.prepare(code: code)
        XCTAssertNotNil(model.preview, "the link names a host to confirm")
        model.confirm()
        try await until("paired and connected", within: 60) { model.phase == "connected" }
        let host = try XCTUnwrap(model.hosts.first { $0.name == name })
        XCTAssertTrue(host.scopes.contains("tasks-operate"), "the pairing grants task control")
        let (status, token) = Keychain.remoteToken(account: host.id)
        XCTAssertEqual(status, errSecSuccess)
        let mac = host.session(token: try XCTUnwrap(token))
        model.disconnect()

        var (session, transport) = try open(mac)
        let info = try await session.connect(cancel: nil)
        XCTAssertTrue(info.capabilities.contains("remote-task-control-v1"))
        XCTAssertNil(session.outboxIssue())

        // F17: composed while away, the first message is a draft behind its
        // task's create; F20: a live control needs a live connection.
        session.suspend()
        // Without a mode the Mac picks Auto when its provider offers it;
        // "permissionMode": "full" in the fixture asks for Full access.
        let mode: TaskPermissionMode? = (fields["permissionMode"] as? String) == "full" ? .full : nil
        let taskId = try session.createTask(projectId: projectId, permissionMode: mode)
        let first = try await session.sendMessage(
            taskId: taskId, text: replyPrompt, steer: false, newWorktree: false, worktreeBase: nil, cancel: nil)
        let draftBeforeCreate = entry(session, first)?.status == .draft
        var stopRefusedOffline = false
        do {
            _ = try await session.stopTask(taskId: taskId, cancel: nil)
        } catch RemoteSessionError.NotOpen {
            stopRefusedOffline = true
        }

        // F18: the phone restarts from its saved outbox.
        let saved = session.outboxBytes()
        session.close()
        transport.close()
        (session, transport) = try open(mac)
        let restored = session.restoreOutbox(bytes: saved) == nil && entry(session, first)?.status == .draft
        _ = try await session.connect(cancel: nil)
        try await until("the Mac admitted and started the first message", within: 120) {
            entry(session, first)?.status == .applied
        }
        try session.followTask(taskId: taskId)
        try await until("the provider answered on the Mac", within: 240) {
            session.remoteTask(taskId: taskId)?.entries.contains {
                $0.role == "assistant" && $0.status == "complete" && !$0.text.isEmpty
            } ?? false
        }
        let users = session.remoteTask(taskId: taskId)?.entries.filter { $0.role == "user" }.map(\.entryId) ?? []
        let firstOnce = users.filter { $0 == first }.count == 1

        // Suspension does not lose or resend anything.
        session.suspend()
        session.resume()
        try await until("live again after resume") {
            session.remoteTask(taskId: taskId)?.freshness == .live
        }

        // A live stop ends a running turn.
        let long = try await session.sendMessage(
            taskId: taskId, text: longPrompt, steer: false, newWorktree: false, worktreeBase: nil, cancel: nil)
        try await until("the long turn runs on the Mac", within: 120) {
            entry(session, long)?.status == .applied
                && session.remoteTask(taskId: taskId)?.task?.status == "working"
        }
        let stop = try await session.stopTask(taskId: taskId, cancel: nil)
        try await until("the stop was admitted", within: 60) {
            entry(session, stop).map { $0.status == .accepted || $0.status == .applied } ?? false
        }
        try await until("the turn stopped", within: 120) {
            session.remoteTask(taskId: taskId)?.task?.status != "working"
        }

        // Each desktop-created task the fixture names (one per provider the
        // Mac runs) gets one message from the phone, answered once.
        var providerTasks: [[String: Any]] = []
        for other in fields["providerTaskIds"] as? [String] ?? [] {
            let sent = try await session.sendMessage(
                taskId: other, text: replyPrompt, steer: false, newWorktree: false, worktreeBase: nil, cancel: nil)
            try session.followTask(taskId: other)
            try await until("task \(other) answered on the Mac", within: 240) {
                entry(session, sent)?.status == .applied
                    && (session.remoteTask(taskId: other)?.entries.contains {
                        $0.role == "assistant" && $0.status == "complete" && !$0.text.isEmpty
                    } ?? false)
            }
            let ids = session.remoteTask(taskId: other)?.entries.filter { $0.role == "user" }.map(\.entryId) ?? []
            providerTasks.append([
                "taskId": other, "provider": session.remoteTask(taskId: other)?.task?.provider ?? "",
                "messageOnce": ids.filter { $0 == sent }.count == 1,
            ])
        }

        let commands = session.outbox()
        let neverResent = commands.allSatisfy { $0.sends <= 1 }
        let core = coreIdentity()
        try JSONSerialization.data(withJSONObject: [
            "deviceId": info.deviceId, "taskId": taskId,
            "draftBeforeCreate": draftBeforeCreate, "stopRefusedOffline": stopRefusedOffline,
            "outboxRestored": restored, "firstMessageOnce": firstOnce, "neverResent": neverResent,
            "stopApplied": true, "resultsOnMain": Thread.isMainThread, "providerTasks": providerTasks,
            "commands": commands.map {
                ["commandId": $0.commandId, "kind": $0.kind, "status": "\($0.status)",
                 "receiptStatus": $0.receiptStatus ?? "", "sends": Int($0.sends)]
            },
            "core": ["facadeVersion": Int(core.facadeVersion), "coreVersion": core.coreVersion,
                     "sdkVersion": core.sdkVersion, "apiVersion": Int(core.apiVersion)],
        ] as [String: Any]).write(to: documents.appendingPathComponent("rm04-native-result.json"), options: .atomic)
        session.close()
        transport.close()
        model.forget(host)
        XCTAssertTrue(draftBeforeCreate && stopRefusedOffline && restored && firstOnce && neverResent)
    }

    /// A runtime session over the shipped pinned transport.
    private func open(_ mac: SavedMac) throws -> (RemoteSession, PinnedSocketTransport) {
        let transport = PinnedSocketTransport()
        let session = try RemoteSession.following(
            mac: mac, endpointIndex: 0, transport: transport, listener: nil, cache: nil)
        transport.attach(session)
        return (session, transport)
    }

    /// Re-checks `condition` every 100 ms until it holds; fails after `seconds`.
    private func until(_ what: String, within seconds: Double = 30, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            guard Date() < deadline else {
                XCTFail("timed out waiting: \(what)")
                throw CommandTimeout(what: what)
            }
            try await Task.sleep(for: .milliseconds(100))
        }
    }
}

private func entry(_ session: RemoteSession, _ commandId: String) -> OutboxEntryRecord? {
    session.outbox().first { $0.commandId == commandId }
}

private struct CommandTimeout: Error {
    let what: String
}
