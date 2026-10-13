import XCTest
@testable import Luna

/// IOS-00 P01 at the native boundary: the shipped `PinnedSocketTransport`
/// and `RemoteHostSession`, the UniFFI core and the shared SDK against a real
/// engine gateway. The engine's `remote_access` campaign
/// (`LUNA_NATIVE_SESSION_FIXTURE`, driven by
/// `scripts/ios/qa-native-session-sim.py`) writes a private fixture into this
/// app's Documents and reads back the public result; without the fixture the
/// test skips. Failure contract: docs/pending-tasks/ios-parity/ios-00/README.md.
@MainActor
final class RemoteSessionTests: XCTestCase {
    private var documents: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    func testTypedReadCancellationReleaseAndRevokeOverTheNativeSocket() async throws {
        let fixture = documents.appendingPathComponent("ios00-session.json")
        guard FileManager.default.fileExists(atPath: fixture.path) else {
            throw XCTSkip("The engine's native session fixture has not been supplied")
        }
        let fields = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: fixture)) as? [String: Any])
        let name = try XCTUnwrap(fields["hostName"] as? String)
        XCTAssertTrue(name.hasPrefix("IOS00 QA "), "Only an owned disposable host fixture may be used")
        let deviceId = try XCTUnwrap(fields["deviceId"] as? String)
        let taskId = try XCTUnwrap(fields["taskId"] as? String)
        let mac = SavedMac(hostName: name,
                           hostTicket: try XCTUnwrap(fields["hostTicket"] as? String),
                           sessionId: try XCTUnwrap(fields["sessionId"] as? String),
                           sessionToken: try XCTUnwrap(fields["sessionToken"] as? String),
                           expiresAtMs: try XCTUnwrap((fields["expiresAtMs"] as? NSNumber)?.uint64Value),
                           scopes: try XCTUnwrap(fields["scopes"] as? [String]),
                           clientLabel: "IOS-00 native")
        let core = coreIdentity()

        // F6/F10: the descriptor and the typed SDK read over URLSession name
        // this host, and every result and state change lands on the main actor.
        let live = try RemoteHostSession(mac: mac)
        var states: [String] = []
        var changedOffMain = false
        live.onChange = { state in
            if !Thread.isMainThread { changedOffMain = true }
            states.append(Self.name(state))
        }
        let host = try await live.connect()
        XCTAssertTrue(Thread.isMainThread)
        XCTAssertEqual(host.deviceId, deviceId)
        XCTAssertEqual(host.apiVersion, core.apiVersion)
        guard case .open(_, let route) = live.state else {
            return XCTFail("a verified socket is open: \(live.state)")
        }

        // F8: each watch holds one of the gateway's 32 in-flight slots until
        // its engine stream is cancelled; a lost cancellation fails by round 33.
        let rounds = 40
        for round in 1...rounds {
            let watch = try await live.watchTaskCommands(taskId: taskId)
            guard case .snapshot(let state, _)? = try await watch.next() else {
                return XCTFail("round \(round): a watch starts with its snapshot")
            }
            XCTAssertEqual(state.taskId, taskId)
            watch.cancel()
            let ended = try await within(5) { try await watch.next() }
            XCTAssertNil(ended, "round \(round): a cancelled watch ends")
        }

        // UniFFI never cancels a Rust future for a cancelled Swift task; the
        // app's bridge must, or this pending read would wait forever.
        let pending = try await live.watchTaskCommands(taskId: taskId)
        guard case .snapshot? = try await pending.next() else {
            return XCTFail("the pending watch starts with its snapshot")
        }
        let waiter = Task { try await pending.next() }
        waiter.cancel()
        let cancelled = try await within(5) { try await waiter.value }
        XCTAssertNil(cancelled, "cancelling the Swift task ended the pending read")

        // F9: every cancellation left the one socket usable.
        let again = try await live.engineInfo()
        XCTAssertEqual(again.deviceId, deviceId)
        live.close()
        guard case .closed(.local, _) = live.state else {
            return XCTFail("closing on the phone is a local close: \(live.state)")
        }

        // F11: releasing a session ends its watch (and closes its socket).
        var released: RemoteHostSession? = try RemoteHostSession(mac: mac)
        _ = try await released?.connect()
        let orphan = try await XCTUnwrap(released).watchTaskCommands(taskId: taskId)
        guard case .snapshot? = try await orphan.next() else {
            return XCTFail("the orphan watch starts with its snapshot")
        }
        released = nil
        let afterRelease = try await within(5) { try await orphan.next() }
        XCTAssertNil(afterRelease, "a released session ends its watch")

        // F12: the Mac's revoke closes an open socket, delivered on the main
        // actor; the same saved access must then pair again.
        let watched = try RemoteHostSession(mac: mac)
        _ = try await watched.connect()
        let closedRemotely = expectation(description: "the Mac's revoke closes the socket")
        var closedOnMain = false
        watched.onChange = { state in
            if case .closed(.remote, _) = state {
                closedOnMain = Thread.isMainThread
                closedRemotely.fulfill()
            }
        }
        try Data("{}".utf8).write(to: documents.appendingPathComponent("ios00-revoke-ready.json"), options: .atomic)
        let revoked = documents.appendingPathComponent("ios00-revoked.json")
        await fulfillment(of: [XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            FileManager.default.fileExists(atPath: revoked.path)
        }, object: nil)], timeout: 60)
        await fulfillment(of: [closedRemotely], timeout: 20)
        XCTAssertTrue(closedOnMain)
        var rePair = false
        do {
            _ = try await RemoteHostSession(mac: mac).connect()
            XCTFail("revoked access must not open a socket")
        } catch let error as RemoteSessionError {
            if case .RePair = error { rePair = true } else { XCTFail("expected re-pair, got \(error)") }
        }
        XCTAssertFalse(changedOffMain, "state changes are delivered on the main actor")

        let result = documents.appendingPathComponent("ios00-native-result.json")
        try JSONSerialization.data(withJSONObject: [
            "typedRead": true, "deviceId": host.deviceId, "apiVersion": Int(host.apiVersion),
            "catalogVersion": Int(host.catalogVersion), "directProtocol": host.directProtocol,
            "core": ["facadeVersion": Int(core.facadeVersion), "coreVersion": core.coreVersion,
                     "sdkVersion": core.sdkVersion, "apiVersion": Int(core.apiVersion),
                     "uniffiContractVersion": Int(core.uniffiContractVersion), "target": core.target,
                     "debugBuild": core.debugBuild],
            "route": ["interface": route?.interface ?? "unknown", "tlsVersion": route?.tlsVersion ?? "unknown",
                      "proxied": route?.proxied ?? false],
            "resumedOnMain": true, "watchCycles": rounds, "swiftCancelEndedRead": true,
            "socketReusedAfterCancel": true, "releaseEndedWatch": true,
            "remoteCloseDeliveredOnMain": closedOnMain, "rePairAfterRevoke": rePair,
            "states": states,
        ] as [String: Any]).write(to: result, options: .atomic)
    }

    private static func name(_ state: RemoteSessionState) -> String {
        switch state {
        case .idle: "idle"
        case .connecting: "connecting"
        case .open: "open"
        case .closed(let reason, _): "closed:\(reason)"
        }
    }
}

private enum Bound: Error { case exceeded }

/// `operation`'s result, or a failure once `seconds` pass: a hang is a test
/// failure, never a stuck run. It does not wait for a hung operation, which
/// may ignore cancellation (the very failure being caught).
private func within<T: Sendable>(_ seconds: Double,
                                 _ operation: @escaping @Sendable () async throws -> T) async throws -> T {
    try await withCheckedThrowingContinuation { continuation in
        let first = FirstResult(continuation)
        Task {
            do { first.resume(.success(try await operation())) } catch { first.resume(.failure(error)) }
        }
        Task {
            try? await Task.sleep(for: .seconds(seconds))
            first.resume(.failure(Bound.exceeded))
        }
    }
}

/// Resumes its continuation with the first result only.
private final class FirstResult<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<T, Error>?

    init(_ continuation: CheckedContinuation<T, Error>) {
        self.continuation = continuation
    }

    func resume(_ result: Result<T, Error>) {
        let pending = lock.withLock { () -> CheckedContinuation<T, Error>? in
            defer { continuation = nil }
            return continuation
        }
        pending?.resume(with: result)
    }
}
