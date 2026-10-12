// Luna for iOS — a viewport onto the luna mesh. The phone is a peer
// device: it joins the workspace and session doc rooms and drives remote
// engines through the durable command queue.

import SwiftUI

@main
struct LunaApp: App {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .preferredColorScheme(.dark)
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

    var body: some View {
        @Bindable var binding = model
        Group {
            switch model.phase {
            case .signedOut:
                SignInView()
            case .pickingOrg(let tokens, let orgs):
                OrgPickerView(tokens: tokens, orgs: orgs)
            case .ready:
                HomeView()
            }
        }
        .task { model.restore() }
        .sheet(isPresented: $binding.showPairedMacs) { PairedMacsView(model: model.pairedMacs) }
        .onOpenURL { url in
            guard url.scheme == "luna", url.host == "pair" else { return }
            model.pairedMacs.incomingCode = url.absoluteString
            model.showPairedMacs = true
        }
    }
}
