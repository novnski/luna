// Luna for iOS — a viewport onto the luna mesh. The phone is a peer
// device: it joins the workspace and session doc rooms and drives remote
// engines through the durable command queue.

import SwiftUI

@main
struct LunaApp: App {
    @State private var appearance = AppearancePreferences()
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    init() {
        // IOS-02: decide (once) and store the appearance before anything
        // else runs, paired or not. `@State` creates the objects above
        // lazily, after `restore()` may already have written this run's
        // preferences, so the migration cannot wait for them.
        AppearanceMigration.migrate()
    }

    /// The paired-Mac shell follows the user's appearance choice; sign-in and
    /// the edge-backed screens keep the dark design they were built for.
    private var showsMacShell: Bool {
        if case .signedOut = model.phase { return !model.pairedMacs.hosts.isEmpty }
        return false
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .environment(appearance)
                .preferredColorScheme(showsMacShell ? appearance.mode.colorScheme : .dark)
                // Monochrome controls: glass buttons, toolbar icons, and
                // toggles render white like the desktop — accent stays paint
                // for status/markdown, never chrome.
                .tint(Theme.text)
                .background(Theme.bg)
                .onChange(of: scenePhase) { _, phase in
                    if phase == .background {
                        model.flushDocs()
                        model.pairedMacs.backgrounded()
                    } else if phase == .active {
                        // Suspension kills sockets without running any
                        // failure path — without this kick the workspace
                        // room stays dead after foregrounding while chat
                        // views reconnect on open (frozen sidebar/Working
                        // indicators against live transcripts, 2026-08-04).
                        model.foregrounded()
                        model.pairedMacs.foregrounded()
                    }
                }
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var model
    /// Until the saved session is restored, an edge account must not flash
    /// (and start connecting) the paired-Mac shell.
    @State private var restored = false

    var body: some View {
        @Bindable var binding = model
        Group {
            switch model.phase {
            case .signedOut:
                if !restored {
                    Theme.bg.ignoresSafeArea()
                } else if model.pairedMacs.hosts.isEmpty {
                    SignInView()
                } else {
                    // IOS-02: direct paired-Mac V1 opens on that Mac's tasks.
                    MacShellView()
                }
            case .pickingOrg(let tokens, let orgs):
                OrgPickerView(tokens: tokens, orgs: orgs)
            case .ready:
                HomeView()
            }
        }
        .task {
            model.restore()
            restored = true
        }
        .sheet(isPresented: $binding.showPairedMacs) { PairedMacsView(model: model.pairedMacs) }
        .onOpenURL { url in
            guard url.scheme == "luna", url.host == "pair" else { return }
            model.pairedMacs.incomingCode = url.absoluteString
            model.showPairedMacs = true
        }
    }
}
