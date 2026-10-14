// IOS-02 local presentation preferences: light/dark/system and list density.
//
// These mirror the desktop's device-local `AppearanceMode` (crates/ui/src/
// appearance.rs: `system`, `light`, `dark`; System is its default). They are
// presentation only: nothing here is sent to a Mac, and no Mac record depends
// on them. Contract: docs/pending-tasks/ios-parity/ios-02/README.md.

import Foundation
import Observation
import SwiftUI

/// The user's appearance choice, stored as the desktop spells it.
enum AppearanceMode: String, CaseIterable, Identifiable {
    case system, light, dark

    var id: String { rawValue }

    var label: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }

    /// nil follows iOS.
    var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}

/// How much each task row shows. Comfortable shows the Mac's preview line.
enum ListDensity: String, CaseIterable, Identifiable {
    case comfortable, compact

    var id: String { rawValue }

    var label: String {
        switch self {
        case .comfortable: "Comfortable"
        case .compact: "Compact"
        }
    }
}

/// The stored keys and the one-time migration of the appearance choice.
///
/// Builds before IOS-02 forced dark and stored no appearance key. The rule:
/// - a stored valid value is kept as is;
/// - with no (or an unreadable) value, an install that an earlier build has
///   already run keeps the dark look it had, and a fresh install follows
///   iOS like the desktop default;
/// - the result is written once, so later launches never re-decide.
/// A downgrade to an earlier build ignores the key and shows dark again.
enum AppearanceMigration {
    static let modeKey = "luna.appearance.v1"
    static let densityKey = "luna.list-density.v1"
    /// Preferences an earlier build writes on its first ordinary run (the
    /// edge URL on every launch, saved Macs, the minted device id, Home's
    /// project filter, the sign-in mode and the new-session defaults).
    static let priorInstallKeys = ["edgeURL", "luna.paired-macs.v1", "deviceId",
                                   "homeSpaceFilter", "authMode", "newSessionHarness"]

    /// The mode to use and whether it must be written.
    static func resolve(stored: String?, priorInstall: Bool) -> (mode: AppearanceMode, write: Bool) {
        if let stored, let mode = AppearanceMode(rawValue: stored) {
            return (mode, false)
        }
        return (priorInstall ? .dark : .system, true)
    }

    /// Applies the rule to this app's preferences and returns the mode.
    /// `LunaApp.init` calls it first thing on every launch, paired or not,
    /// before any view or `restore()` writes this run's preferences; later
    /// calls find the stored value and write nothing. The one write is
    /// flushed at once, so terminating the app right after its first launch
    /// cannot lose it (and so re-decide it on a later launch).
    @discardableResult
    static func migrate() -> AppearanceMode {
        let defaults = UserDefaults.standard
        let prior = priorInstallKeys.contains { defaults.object(forKey: $0) != nil }
        let resolved = resolve(stored: defaults.string(forKey: modeKey), priorInstall: prior)
        if resolved.write {
            defaults.set(resolved.mode.rawValue, forKey: modeKey)
            defaults.synchronize()
        }
        return resolved.mode
    }
}

/// The app's one owner of the appearance and density choices. SwiftUI
/// creates an `@Observable` `@State` value lazily, so the migration does not
/// depend on this object: `LunaApp.init` runs it first, and this reads what
/// it decided.
@MainActor
@Observable
final class AppearancePreferences {
    private(set) var mode: AppearanceMode
    private(set) var density: ListDensity

    init() {
        mode = AppearanceMigration.migrate()
        density = ListDensity(rawValue: UserDefaults.standard.string(forKey: AppearanceMigration.densityKey) ?? "")
            ?? .comfortable
    }

    func setMode(_ mode: AppearanceMode) {
        self.mode = mode
        UserDefaults.standard.set(mode.rawValue, forKey: AppearanceMigration.modeKey)
    }

    func setDensity(_ density: ListDensity) {
        self.density = density
        UserDefaults.standard.set(density.rawValue, forKey: AppearanceMigration.densityKey)
    }
}
