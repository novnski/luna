import XCTest

@MainActor
final class ProjectSessionUITests: XCTestCase {
    private var app: XCUIApplication!

    override func setUp() {
        continueAfterFailure = false
        app = XCUIApplication()
        XCUIDevice.shared.orientation = .portrait
    }

    private func launch(_ arguments: [String] = [], filter: String = "") {
        app.launchArguments = ["-demo", "-sethomefilter", filter] + arguments
        app.launch()
    }

    private var composer: XCUIElement {
        app.descendants(matching: .any)["composer-input"].firstMatch
    }

    private func expectContext(_ label: String) {
        let context = app.staticTexts["new-session-context"]
        let matches = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", label),
                                                object: context)
        XCTAssertEqual(XCTWaiter.wait(for: [matches], timeout: 5), .completed)
    }

    func testAccountWithoutProjectsOffersProjectCreation() {
        launch(["-no-projects"])
        let newSession = app.buttons["new-session"]
        XCTAssertTrue(newSession.waitForExistence(timeout: 5))
        newSession.tap()
        XCTAssertTrue(app.buttons["New space…"].exists)
        XCTAssertFalse(app.buttons["new-projectless-session"].exists)
    }

    func testProjectSessionStillUsesItsCheckoutAndCanSend() {
        launch(filter: "space-luna")
        let newSession = app.buttons["new-session"]
        XCTAssertTrue(newSession.waitForExistence(timeout: 5))
        newSession.tap()
        app.buttons["New session in luna"].tap()
        expectContext("luna · MacBook Pro")
        XCTAssertTrue(app.buttons["session-checkout"].exists)
        XCTAssertTrue(app.buttons["session-ref"].exists)
        XCTAssertFalse(app.buttons["session-host"].exists)
        composer.tap()
        composer.typeText("Inspect this project.")
        app.buttons["composer-send"].tap()
        XCTAssertTrue(app.otherElements["transcript"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["luna @ MacBook Pro"].exists)
    }
}
