import XCTest

/// UI→native core→pinned host→device-only persistence. Fixture input lives in
/// the test runner's private Documents directory; no secret enters app launch
/// arguments, source control or published failure messages.
final class RemotePairingUITests: XCTestCase {
    override func setUpWithError() throws { continueAfterFailure = false }

    func testManualPairingRestartAndLocalForget() throws {
        let fixture = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("rm02-pairing.json")
        guard FileManager.default.fileExists(atPath: fixture.path) else {
            throw XCTSkip("Physical host pairing fixture has not been supplied")
        }
        let fields = try JSONSerialization.jsonObject(with: Data(contentsOf: fixture)) as! [String: String]
        let code = try XCTUnwrap(fields["code"])
        let name = try XCTUnwrap(fields["name"])
        let secondCode = try XCTUnwrap(fields["secondCode"])
        let secondName = try XCTUnwrap(fields["secondName"])
        let wrongPinCode = try XCTUnwrap(fields["wrongPinCode"])
        let wrongProfileCode = try XCTUnwrap(fields["wrongProfileCode"])
        XCTAssertTrue(name.hasPrefix("RM02 QA "), "Only an owned disposable host fixture may be used")
        XCTAssertTrue(secondName.hasPrefix("RM02 QA "))
        let app = XCUIApplication()
        app.launch()
        openConnections(app)
        app.buttons["remote.add-mac"].tap()
        // A simulator has no camera. Denied/unavailable capture must leave a
        // visible manual-entry escape rather than a stuck scanner sheet.
        app.buttons["remote.scan"].tap()
        let system = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let deny = system.alerts.buttons.matching(NSPredicate(format:
            "label CONTAINS[c] 'Allow' AND label != 'Allow'")).firstMatch
        if deny.waitForExistence(timeout: 3) { deny.tap() }
        let close = app.buttons["Close"]
        XCTAssertTrue(close.waitForExistence(timeout: 5))
        let fallback = app.staticTexts.matching(NSPredicate(format:
            "label CONTAINS[c] 'manually'")).firstMatch
        XCTAssertTrue(fallback.waitForExistence(timeout: 5))
        close.tap()
        for (badCode, rejection) in [(wrongPinCode, "TLS trust was refused"),
            (wrongProfileCode, "Host identity, pin or protocol does not match")] {
            startPair(app, code: badCode)
            let error = app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", rejection)).firstMatch
            XCTAssertTrue(error.waitForExistence(timeout: 15))
            XCTAssertEqual(app.otherElements.matching(identifier: "remote.saved-mac").count, 0)
            app.buttons["Cancel"].tap()
        }
        pair(app, code: code)
        let card = app.otherElements.matching(identifier: "remote.saved-mac").containing(.staticText, identifier: name).firstMatch
        XCTAssertTrue(card.exists)
        // IOS-00: after authentication the app opens the pinned SDK socket;
        // its typed engine read must name this host, not just any listener.
        if let deviceId = fields["deviceId"] {
            let live = card.staticTexts.matching(NSPredicate(format:
                "identifier == 'remote.live' AND label BEGINSWITH %@",
                "Live · host \(deviceId.prefix(8))")).firstMatch
            XCTAssertTrue(live.waitForExistence(timeout: 20), "the live socket's typed read names the paired Mac")
        }
        app.terminate()
        app.launch()
        openConnections(app)
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        card.buttons["remote.connect"].tap()
        XCTAssertTrue(app.staticTexts["Connected"].waitForExistence(timeout: 20))
        app.buttons["remote.add-mac"].tap()
        pair(app, code: secondCode)
        let second = app.otherElements.matching(identifier: "remote.saved-mac")
            .containing(.staticText, identifier: secondName).firstMatch
        XCTAssertTrue(second.exists)
        card.buttons["remote.forget"].tap()
        app.buttons["Forget from this phone"].tap()
        XCTAssertFalse(card.exists, "Only the owned host was removed; other saved hosts are preserved")
        XCTAssertTrue(second.exists)
        second.buttons["remote.connect"].tap()
        XCTAssertTrue(app.staticTexts["Connected"].waitForExistence(timeout: 20))
        let directory = fixture.deletingLastPathComponent()
        try Data("{}".utf8).write(to: directory.appendingPathComponent("rm02-revoke-ready.json"), options: .atomic)
        let revoked = directory.appendingPathComponent("rm02-revoked.json")
        let acknowledgement = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            FileManager.default.fileExists(atPath: revoked.path)
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [acknowledgement], timeout: 20), .completed)
        second.buttons["remote.connect"].tap()
        XCTAssertTrue(app.staticTexts["Pair again"].waitForExistence(timeout: 20))
        second.buttons["remote.forget"].tap()
        app.buttons["Forget from this phone"].tap()
        XCTAssertFalse(second.exists)
        let result = fixture.deletingLastPathComponent().appendingPathComponent("rm02-native-result.json")
        try JSONSerialization.data(withJSONObject: ["paired": true, "restored": true, "forgotten": true,
            "cameraFallback": true, "forgetPreservedOtherHost": true, "revokedDenied": true,
            "wrongPinRejected": true, "wrongProfileRejected": true,
            "name": name]).write(to: result, options: .atomic)
    }
    private func pair(_ app: XCUIApplication, code: String) {
        startPair(app, code: code)
        XCTAssertTrue(app.staticTexts["Connected"].waitForExistence(timeout: 20))
    }
    private func startPair(_ app: XCUIApplication, code: String) {
        let input = app.secureTextFields["remote.pairing-code"]
        XCTAssertTrue(input.waitForExistence(timeout: 5))
        input.tap(); input.typeText(code)
        app.buttons["remote.review"].tap()
        let confirm = app.buttons["remote.confirm"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        confirm.tap()
    }
    private func openConnections(_ app: XCUIApplication) {
        let button = app.buttons["remote.open"]
        if !button.waitForExistence(timeout: 3) {
            app.buttons["person.circle"].tap()
        }
        XCTAssertTrue(button.waitForExistence(timeout: 5))
        button.tap()
        XCTAssertTrue(app.navigationBars["Paired Macs"].waitForExistence(timeout: 5))
    }
}
