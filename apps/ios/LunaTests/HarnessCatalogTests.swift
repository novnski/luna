import XCTest
@testable import Luna

final class HarnessCatalogTests: XCTestCase {
    func testDefaultReasoningMatchesDesktopPreference() {
        let astra = HarnessCatalog.defaultModel(for: "codex")
        XCTAssertEqual(HarnessCatalog.defaultReasoning(for: astra), "high")

        let short = ModelInfo(id: "short", label: "Short", description: nil,
                              reasoningLevels: ["low", "medium"])
        XCTAssertEqual(HarnessCatalog.defaultReasoning(for: short), "medium")
    }

    func testChoiceFallsBackToAdvertisedDefault() {
        let option = HarnessCatalog.defaultModel(for: "codex").options[0]
        XCTAssertEqual(HarnessCatalog.selectedChoice(for: option, selectedId: "fast").label, "Fast")
        XCTAssertEqual(HarnessCatalog.selectedChoice(for: option, selectedId: "stale").id, "default")
    }
}
