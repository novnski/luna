import XCTest
@testable import Luna

final class ChangeRequestTrackingTests: XCTestCase {
    private func chat(
        id: String,
        device: String,
        cwd: String? = "/repo",
        branch: String? = "feature/pr",
        checkout: String? = "checkout"
    ) -> Chat {
        Chat(id: id, deviceId: device, title: nil, archived: false, cwd: cwd,
             branch: branch, checkoutId: checkout, config: nil,
             lastMessagePreview: nil, lastMessageAt: nil, createdAt: 0,
             spaceId: "space", lastSeenAt: nil)
    }

    private func summary(state: ChangeRequestState = .open) -> ChangeRequestSummary {
        ChangeRequestSummary(provider: "github", number: 90, title: "Show host pull requests",
                             url: "https://github.com/acme/luna/pull/90", state: state,
                             baseRef: "main", headRef: "feature/pr")
    }

    private func status(
        device: String = "local",
        cwd: String = "/repo",
        branch: String = "feature/pr",
        checkout: String = "checkout",
        changeRequest: ChangeRequestSummary? = nil
    ) -> CheckoutChangeRequestStatus {
        CheckoutChangeRequestStatus(checkoutId: checkout, deviceId: device, cwd: cwd,
                                    branch: branch, changeRequest: changeRequest,
                                    updatedAt: "2026-08-15T12:30:00Z")
    }

    func testContractDecodesEveryState() throws {
        // The host's camelCase wire shape, as pinned by crates/proto entities.rs
        // checkout_change_request_status_round_trips_all_states_as_camel_case.
        let states: [(String, ChangeRequestState)] = [("open", .open), ("merged", .merged), ("closed", .closed)]
        for (wire, state) in states {
            let json = #"""
            {"checkoutId":"checkout","deviceId":"local","cwd":"/repo","branch":"feature/pr",
             "changeRequest":{"provider":"github","number":90,"title":"Show host pull requests",
              "url":"https://github.com/acme/luna/pull/90","state":"\#(wire)",
              "baseRef":"main","headRef":"feature/pr"},
             "updatedAt":"2026-08-15T12:30:00Z"}
            """#
            let decoded = try JSONDecoder().decode(CheckoutChangeRequestStatus.self, from: Data(json.utf8))
            XCTAssertEqual(decoded, status(changeRequest: summary(state: state)))
        }
    }

    func testLocalAndRemoteChatsDeduplicateByHostCheckoutPath() {
        let chats = [
            chat(id: "local-a", device: "local"),
            chat(id: "local-b", device: "local"),
            chat(id: "remote", device: "remote"),
        ]
        let targets = desiredChangeRequestTargets(chats: chats, spaces: [], unsupportedDevices: [])
        XCTAssertEqual(targets, [
            ChangeRequestWatchKey(deviceId: "local", cwd: "/repo"),
            ChangeRequestWatchKey(deviceId: "remote", cwd: "/repo"),
        ])
    }

    func testDemandRequiresActiveBranchAndFallsBackToOwningSpace() {
        var archived = chat(id: "archived", device: "local")
        archived.archived = true
        let noBranch = chat(id: "no-branch", device: "local", branch: nil)
        let fallback = chat(id: "fallback", device: "local", cwd: nil, checkout: nil)
        let wrongDeviceSpace = Space(id: "space", deviceId: "remote", path: "/wrong", name: nil,
                                     gitDetected: true, gitCheckedAt: nil, checkoutId: nil, createdAt: 0)
        let owningSpace = Space(id: "space", deviceId: "local", path: "/project", name: nil,
                                gitDetected: true, gitCheckedAt: nil, checkoutId: nil, createdAt: 0)

        XCTAssertEqual(
            desiredChangeRequestTargets(
                chats: [archived, noBranch, fallback], spaces: [wrongDeviceSpace, owningSpace],
                unsupportedDevices: []
            ),
            [ChangeRequestWatchKey(deviceId: "local", cwd: "/project")]
        )
        XCTAssertTrue(desiredChangeRequestTargets(
            chats: [fallback], spaces: [owningSpace], unsupportedDevices: ["local"]
        ).isEmpty)
    }

    func testSnapshotMustMatchHostBranchAndCheckout() {
        let target = ChangeRequestWatchKey(deviceId: "remote", cwd: "/repo")
        let targetChat = chat(id: "chat", device: "remote")
        var snapshots = [target: status(device: "remote", changeRequest: summary())]
        XCTAssertEqual(resolvedChangeRequest(for: targetChat, spaces: [], snapshots: snapshots)?.number, 90)

        snapshots[target]?.branch = "other"
        XCTAssertNil(resolvedChangeRequest(for: targetChat, spaces: [], snapshots: snapshots))
        snapshots[target] = status(device: "remote", checkout: "old", changeRequest: summary())
        XCTAssertNil(resolvedChangeRequest(for: targetChat, spaces: [], snapshots: snapshots))

        let legacyChat = chat(id: "legacy", device: "remote", checkout: nil)
        XCTAssertEqual(resolvedChangeRequest(for: legacyChat, spaces: [], snapshots: snapshots)?.number, 90)
    }

    func testSuccessfulNoneReplacesVisiblePullRequest() {
        let target = ChangeRequestWatchKey(deviceId: "local", cwd: "/repo")
        let targetChat = chat(id: "chat", device: "local")
        var snapshots = [target: status(changeRequest: summary())]
        XCTAssertNotNil(resolvedChangeRequest(for: targetChat, spaces: [], snapshots: snapshots))

        snapshots[target] = status(changeRequest: nil)
        XCTAssertNil(resolvedChangeRequest(for: targetChat, spaces: [], snapshots: snapshots))
    }

    func testUnknownMethodAndAccessibilityLabelsAreExplicit() {
        XCTAssertTrue(isUnknownChangeRequestMethod(RelayError.rpc(
            "unknown method: WatchCheckoutChangeRequest"
        )))
        XCTAssertFalse(isUnknownChangeRequestMethod(RelayError.hostOffline))
        XCTAssertEqual(summary(state: .merged).accessibilityLabel,
                       "Pull request 90, Merged, Show host pull requests")
    }
}
