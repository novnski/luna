import UIKit
import XCTest

/// IOS-02 P03 at the UI boundary: the shipped app and its paired-Mac shell,
/// the UniFFI core and a real engine gateway. The native-only engine campaign
/// `remote_access::gateway::ios02_native_shell_...` (driven by
/// `scripts/ios/qa-native-session-sim.py --campaign ios02`) seeds two
/// projects and five tasks, writes a `luna://pair` code into this runner's
/// Documents, and deletes the open task, stops and restarts on cue. The app
/// pairs through its own Paired Macs UI; nothing here injects data, and
/// without the fixture the test skips. Run on an iPhone and an iPad
/// Simulator. Failure contract: docs/pending-tasks/ios-parity/ios-02/README.md.
@MainActor
final class AdaptiveShellUITests: XCTestCase {
    private var app: XCUIApplication!
    private var documents: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }
    private var iPad: Bool { UIDevice.current.userInterfaceIdiom == .pad }

    override func setUp() {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
    }

    override func tearDown() {
        XCUIDevice.shared.orientation = .portrait
    }

    func testShellFollowsARealMacThroughFiltersRotationRestartDeletionAndOffline() throws {
        let fixture = documents.appendingPathComponent("ios02-shell.json")
        guard FileManager.default.fileExists(atPath: fixture.path) else {
            throw XCTSkip("The engine's IOS-02 shell fixture has not been supplied")
        }
        let fields = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: fixture)) as? [String: Any])
        func field(_ key: String) throws -> String {
            try XCTUnwrap(fields[key] as? String, "fixture field \(key)")
        }
        let code = try field("code")
        let name = try field("name")
        XCTAssertTrue(name.hasPrefix("IOS02 QA "), "Only an owned disposable host fixture may be used")
        let deviceId = try field("deviceId")
        let alpha = try field("alphaProject")
        let beta = try field("betaProject")
        let pinned = try field("pinnedTaskId")
        let selected = try field("selectTaskId")
        let selectedTitle = try field("selectTitle")
        let deleted = try field("deleteTaskId")
        let deletedTitle = try field("deleteTitle")
        let archived = try field("archivedTaskId")
        let other = try field("otherTaskId")
        let entryIds = try XCTUnwrap(fields["selectEntryIds"] as? [String])
        let entries = try XCTUnwrap(fields["selectEntries"] as? [String])
        let firstDraft = "IOS02 draft survives rotation and relaunch"
        let secondDraft = "IOS02 draft kept after the Mac deleted its task"
        var observed: [String: Any] = ["idiom": iPad ? "pad" : "phone", "deviceId": deviceId]

        app = XCUIApplication()
        app.launch()
        forgetStaleQAMacs()

        // P03.1: pair through the app's own Paired Macs UI.
        pair(code: code)
        observed["pairedThroughTheApp"] = true

        // P03.2: the Mac's own rows, live, under its identity; archived rows
        // stay out of All; the Mac's order (pinned first, then newest).
        expect(element("shell.freshness"), "label BEGINSWITH 'Live'", within: 30,
               because: "a live list from the paired Mac")
        XCTAssertTrue(label("shell.mac.identity").contains("host \(deviceId.prefix(8))"),
                      "the connected Mac's identity is visible")
        for id in [pinned, selected, deleted, other] {
            XCTAssertTrue(row(id).waitForExistence(timeout: 10), "the Mac lists \(id)")
        }
        XCTAssertFalse(row(archived).exists, "All leaves archived tasks out")
        observed["listedHostTasks"] = true
        XCTAssertLessThan(row(pinned).frame.minY, row(selected).frame.minY)
        XCTAssertLessThan(row(selected).frame.minY, row(other).frame.minY)
        observed["pinnedFirst"] = true
        let selectedLabel = row(selected).label
        XCTAssertTrue(selectedLabel.hasPrefix(selectedTitle), selectedLabel)
        XCTAssertTrue(selectedLabel.contains(alpha) && selectedLabel.contains("Unread"), selectedLabel)
        XCTAssertTrue(row(pinned).label.contains("Pinned"), row(pinned).label)
        XCTAssertTrue(label("shell.filter.archived").hasPrefix("Archived, 1"), label("shell.filter.archived"))
        observed["voiceOverLabels"] = true
        capture("list-live")

        // P03.3: organization over the same rows: filters, project, search.
        tap("shell.filter.archived")
        expect(row(archived), "exists == true", because: "Archived shows the archived task")
        expectGone(row(selected), because: "Archived hides active tasks")
        tap("shell.filter.unread")
        expect(row(selected), "exists == true", because: "Unread shows the unseen task")
        expectGone(row(other), because: "Unread hides tasks that were seen")
        tap("shell.clear-filters")
        expect(row(other), "exists == true", because: "clearing returns every task")
        observed["filters"] = true
        app.buttons["shell.project"].tap()
        let betaItem = app.buttons[beta]
        XCTAssertTrue(betaItem.waitForExistence(timeout: 5), "the project menu lists the Mac's projects")
        betaItem.tap()
        expect(row(other), "exists == true", because: "the Beta project lists its tasks")
        expectGone(row(selected), because: "the Beta project hides Alpha's tasks")
        tap("shell.clear-filters")
        expect(row(selected), "exists == true", because: "every project again")
        observed["projectScope"] = true
        app.typeKey("f", modifierFlags: .command)
        // typeText needs keyboard focus: Command-F must have focused search.
        app.searchFields.firstMatch.typeText("delete")
        expect(row(deleted), "exists == true", because: "search finds the task by its title")
        expectGone(row(other), because: "search hides tasks that do not match")
        tap("shell.clear-filters")
        expect(row(other), "exists == true", because: "clearing the search returns every task")
        observed["keyboardSearch"] = true

        // P03.4: open a task by its host id and read the Mac's history.
        open(selected, title: selectedTitle)
        expect(element("shell.detail.freshness"), "label BEGINSWITH 'Live'", within: 30,
               because: "a live task detail")
        for (id, text) in zip(entryIds, entries) {
            let entry = element("remote.entry.\(id)")
            XCTAssertTrue(entry.waitForExistence(timeout: 10), "entry \(id) from the Mac")
            XCTAssertTrue(entry.label.contains(text), entry.label)
        }
        observed["liveDetail"] = true
        XCTAssertTrue(label("shell.detail.meta").contains("Unread"),
                      "opening a task on the phone does not pretend the Mac saw it")
        observed["unreadKept"] = true
        if iPad {
            XCTAssertTrue(row(selected).exists, "iPad shows the list beside the open task")
            observed["splitShowsListAndTask"] = true
        }

        // P03.5: the draft and the selection survive rotation.
        let draft = element("shell.draft")
        draft.tap()
        draft.typeText(firstDraft)
        dismissKeyboard()
        XCTAssertEqual(draftText(), firstDraft)
        for (orientation, tag) in [(UIDeviceOrientation.landscapeLeft, "landscape"), (.portrait, "portrait")] {
            XCUIDevice.shared.orientation = orientation
            expect(element("shell.detail.title"), "label == %@", args: [selectedTitle],
                   because: "rotation keeps the same task")
            XCTAssertEqual(draftText(), firstDraft, "rotation keeps the draft")
            capture("detail-\(tag)")
        }
        observed["selectionSurvivedRotation"] = true
        observed["draftSurvivedRotation"] = true

        // P03.6: a relaunch reopens the same task and draft, confirmed by the Mac.
        app.terminate()
        app.launch()
        expect(element("shell.detail.title"), "label == %@", args: [selectedTitle], within: 30,
               because: "the restored route opens the same task")
        XCTAssertEqual(draftText(), firstDraft, "the draft survived the relaunch")
        expect(element("shell.detail.freshness"), "label BEGINSWITH 'Live'", within: 30,
               because: "the restored task is confirmed live")
        observed["restoredSelectionAndDraft"] = true

        // P03.7: Command-] opens the next task; the Mac deletes it while open.
        app.typeKey("]", modifierFlags: .command)
        expect(element("shell.detail.title"), "label == %@", args: [deletedTitle],
               because: "Command-] opens the next task in the list's order")
        observed["keyboardNextTask"] = true
        let doomed = element("shell.draft")
        doomed.tap()
        doomed.typeText(secondDraft)
        dismissKeyboard()
        try mark("ios02-native-delete.json")
        cue("ios02-host-deleted.json")
        expect(element("shell.detail.removed"), "exists == true", within: 30,
               because: "the Mac's removal shows as removed")
        XCTAssertFalse(element("shell.detail.title").exists, "nothing stale stands in for a removed task")
        XCTAssertEqual(label("shell.detail.kept-draft"), secondDraft)
        observed["deletedWhileOpenTruthful"] = true
        observed["draftKeptAfterDelete"] = true
        capture("detail-removed")

        // P03.8: relaunching on the deleted route settles on the Mac's answer,
        // not a spinner or a copy, and the draft is still there.
        app.terminate()
        app.launch()
        expect(element("shell.detail.removed"), "exists == true", within: 30,
               because: "a restored route to a deleted task settles as removed")
        XCTAssertEqual(label("shell.detail.kept-draft"), secondDraft)
        observed["restoredDeletedRouteTruthful"] = true
        tap("shell.draft.discard")
        expectGone(element("shell.detail.kept-draft"), because: "Discard removes the kept draft")
        tap("shell.detail.back")

        // P03.9: the Mac stops; the open task turns stale, still labelled.
        open(selected, title: selectedTitle)
        expect(element("shell.detail.freshness"), "label BEGINSWITH 'Live'", within: 30,
               because: "live before the Mac stops")
        // Backgrounding is when RM-03 saves the projections for a later run.
        background()
        app.activate()
        expect(element("shell.detail.freshness"), "label BEGINSWITH 'Live'", within: 30,
               because: "live again after returning to the foreground")
        try mark("ios02-native-stop.json")
        cue("ios02-host-stopped.json")
        expect(element("shell.detail.freshness"), "label BEGINSWITH 'Stale'", within: 60,
               because: "the stopped Mac's task turns stale")
        XCTAssertEqual(label("shell.detail.title"), selectedTitle, "the same task, labelled, never replaced")
        observed["staleWhenMacStopped"] = true
        capture("detail-stale")

        // P03.10: relaunched while the Mac is down: cached, labelled, Retry.
        background()
        app.terminate()
        app.launch()
        expect(element("shell.detail.freshness"), "label BEGINSWITH 'Cached'", within: 30,
               because: "a relaunch offline shows the saved task as cached")
        XCTAssertEqual(label("shell.detail.title"), selectedTitle)
        XCTAssertEqual(draftText(), firstDraft)
        expect(element("shell.detail.retry"), "exists == true", within: 30,
               because: "an unreachable Mac settles with Retry, never an endless opening state")
        observed["cachedWhenRelaunchedOffline"] = true
        capture("detail-cached-offline")
        tap("shell.detail.retry")
        expect(element("shell.detail.retry"), "exists == true", within: 30,
               because: "a failed retry settles with Retry again")
        XCTAssertTrue(label("shell.detail.freshness").hasPrefix("Cached"), "a failed retry never looks live")
        observed["retryStaysTruthful"] = true

        // P03.11: the Mac is back; Command-R reconnects and confirms it live.
        try mark("ios02-native-restart.json")
        cue("ios02-host-restarted.json", within: 120)
        app.typeKey("r", modifierFlags: .command)
        expect(element("shell.detail.freshness"), "label BEGINSWITH 'Live'", within: 60,
               because: "the restarted Mac confirms the task live again")
        observed["keyboardReconnect"] = true
        observed["liveAfterRestart"] = true

        // P03.12: appearance by keyboard; Light is kept across a relaunch.
        app.typeKey(",", modifierFlags: .command)
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 10), "Command-, opens Settings")
        observed["keyboardSettings"] = true
        app.buttons["Light"].tap()
        expect(element("settings.appearance.effective"), "label BEGINSWITH 'Showing Light'",
               because: "Light applies at once")
        capture("settings-light")
        tap("settings.done")
        capture("detail-light")
        app.terminate()
        app.launch()
        expect(element("shell.detail.title"), "label == %@", args: [selectedTitle], within: 30,
               because: "restored after the relaunch")
        capture("detail-light-relaunched")
        app.typeKey(",", modifierFlags: .command)
        expect(element("settings.appearance.effective"), "label BEGINSWITH 'Showing Light'",
               because: "Light was kept across the relaunch")
        app.buttons["Dark"].tap()
        expect(element("settings.appearance.effective"), "label BEGINSWITH 'Showing Dark'",
               because: "Dark applies at once")
        capture("settings-dark")
        app.buttons["System"].tap()
        expect(element("settings.appearance.effective"), "label CONTAINS 'following iOS'",
               because: "System follows iOS")
        tap("settings.done")
        observed["appearancePersisted"] = true

        // P03.13: the largest accessibility text size keeps every control
        // reachable, and the shell's own elements pass the native audit.
        app.terminate()
        app.launchArguments = ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityXXXL"]
        app.launch()
        expect(element("shell.detail.title"), "label == %@", args: [selectedTitle], within: 30,
               because: "restored at the largest text size")
        expect(element("shell.detail.freshness"), "label BEGINSWITH 'Live'", within: 30,
               because: "live at the largest text size")
        XCTAssertTrue(element("shell.detail.next").isHittable && element("shell.detail.previous").isHittable,
                      "task navigation stays reachable")
        let largeDraft = element("shell.draft")
        for _ in 0..<8 where !largeDraft.isHittable { app.swipeUp() }
        XCTAssertTrue(largeDraft.isHittable, "the draft is reachable at the largest text size")
        XCTAssertEqual(draftText(), firstDraft)
        capture("detail-ax-xxxl")
        try audit()
        showList()
        let largeRow = row(selected)
        XCTAssertTrue(largeRow.waitForExistence(timeout: 10))
        for _ in 0..<8 where !largeRow.isHittable { app.swipeUp() }
        XCTAssertTrue(largeRow.isHittable, "rows stay reachable at the largest text size")
        for _ in 0..<8 where !element("shell.filter.archived").isHittable { app.swipeDown() }
        XCTAssertTrue(element("shell.filter.archived").isHittable, "every filter stays on screen")
        capture("list-ax-xxxl")
        try audit()
        observed["largeTextControlsReachable"] = true
        observed["accessibilityAudit"] = true

        // P03.14: Forget removes the Mac (with its cache and drafts) here.
        app.terminate()
        app.launchArguments = []
        app.launch()
        openPairedMacs()
        let card = app.otherElements.matching(identifier: "remote.saved-mac")
            .containing(.staticText, identifier: name).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 10))
        card.buttons["remote.forget"].tap()
        app.buttons["Forget from this phone"].tap()
        expectGone(card, because: "Forget removes the saved Mac")
        observed["forgotten"] = true
        app.navigationBars["Paired Macs"].buttons["Done"].tap()

        try JSONSerialization.data(withJSONObject: observed)
            .write(to: documents.appendingPathComponent("ios02-native-result.json"), options: .atomic)
    }

    // MARK: Steps

    private func pair(code: String) {
        openPairedMacs()
        app.buttons["remote.add-mac"].tap()
        let input = app.secureTextFields["remote.pairing-code"]
        XCTAssertTrue(input.waitForExistence(timeout: 5))
        input.tap()
        input.typeText(code)
        app.buttons["remote.review"].tap()
        let confirm = app.buttons["remote.confirm"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 10))
        confirm.tap()
        XCTAssertTrue(app.staticTexts["Connected"].waitForExistence(timeout: 30), "the app paired and verified the Mac")
        app.navigationBars["Paired Macs"].buttons["Done"].tap()
    }

    /// A Mac from an earlier, interrupted run would answer for another
    /// engine; remove it through the same UI first.
    private func forgetStaleQAMacs() {
        openPairedMacs()
        let stale = app.otherElements.matching(identifier: "remote.saved-mac")
            .containing(NSPredicate(format: "label BEGINSWITH 'IOS02 QA '"))
        while stale.firstMatch.waitForExistence(timeout: 2) {
            stale.firstMatch.buttons["remote.forget"].tap()
            app.buttons["Forget from this phone"].tap()
        }
        app.navigationBars["Paired Macs"].buttons["Done"].tap()
    }

    private func openPairedMacs() {
        let button = app.buttons["remote.open"]
        if !button.waitForExistence(timeout: 5), !iPad {
            // A restored task on iPhone covers the list and its toolbar.
            app.navigationBars.buttons.element(boundBy: 0).tap()
        }
        XCTAssertTrue(button.waitForExistence(timeout: 10))
        button.tap()
        XCTAssertTrue(app.navigationBars["Paired Macs"].waitForExistence(timeout: 5))
    }

    /// iPhone: back from the task to the list (the same columns, stacked).
    private func showList() {
        guard !iPad, !element("shell.freshness").exists else { return }
        app.navigationBars.buttons.element(boundBy: 0).tap()
        expect(element("shell.freshness"), "exists == true", within: 10, because: "the task list is shown")
    }

    private func open(_ taskId: String, title: String) {
        showList()
        let target = row(taskId)
        XCTAssertTrue(target.waitForExistence(timeout: 10), "row \(taskId)")
        target.tap()
        expect(element("shell.detail.title"), "label == %@", args: [title], within: 30,
               because: "the task opened by its host id")
    }

    /// Home, then wait until iOS suspended the app: its background handlers
    /// (RM-03 saves the projections) have run by then.
    private func background() {
        XCUIDevice.shared.press(.home)
        _ = app.wait(for: .runningBackgroundSuspended, timeout: 20)
    }

    private func dismissKeyboard() {
        let done = app.buttons["shell.draft.done"]
        if done.exists, done.isHittable { done.tap() }
    }

    /// The native accessibility audit, failing only on this shell's own
    /// elements (system chrome is reported by the system's own owners).
    private func audit() throws {
        try app.performAccessibilityAudit(for: [.dynamicType, .textClipped, .sufficientElementDescription, .contrast]) { issue in
            let id = issue.element?.identifier ?? ""
            let owned = ["shell.", "settings.", "remote.entry."].contains { id.hasPrefix($0) }
            return !owned
        }
    }

    // MARK: Helpers

    private func element(_ id: String) -> XCUIElement {
        app.descendants(matching: .any)[id].firstMatch
    }

    private func row(_ taskId: String) -> XCUIElement {
        element("shell.task.\(taskId)")
    }

    private func label(_ id: String) -> String {
        let target = element(id)
        XCTAssertTrue(target.waitForExistence(timeout: 10), id)
        return target.label
    }

    private func draftText() -> String {
        let field = element("shell.draft")
        XCTAssertTrue(field.waitForExistence(timeout: 10), "the draft field")
        return field.value as? String ?? ""
    }

    private func tap(_ id: String) {
        let target = element(id)
        XCTAssertTrue(target.waitForExistence(timeout: 10), id)
        target.tap()
    }

    private func expect(_ element: XCUIElement, _ format: String, args: [Any] = [],
                        within seconds: TimeInterval = 20, because what: String) {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: format, argumentArray: args), object: element)
        XCTAssertEqual(XCTWaiter.wait(for: [expectation], timeout: seconds), .completed, what)
    }

    private func expectGone(_ element: XCUIElement, within seconds: TimeInterval = 20, because what: String) {
        expect(element, "exists == false", within: seconds, because: what)
    }

    private func capture(_ name: String) {
        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = "\(iPad ? "ipad" : "iphone")-\(name)"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func mark(_ name: String) throws {
        try Data("{}".utf8).write(to: documents.appendingPathComponent(name), options: .atomic)
    }

    private func cue(_ name: String, within seconds: TimeInterval = 90) {
        let file = documents.appendingPathComponent(name)
        let arrived = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            FileManager.default.fileExists(atPath: file.path)
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [arrived], timeout: seconds), .completed, "host cue \(name)")
    }
}
