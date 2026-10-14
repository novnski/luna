// IOS-02 Settings for the paired-Mac shell: appearance and list density
// (local presentation), what iOS's own accessibility settings are doing, and
// the connected Mac's identity. Contract:
// docs/pending-tasks/ios-parity/ios-02/README.md.

import SwiftUI

struct ShellSettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(AppearancePreferences.self) private var appearance
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var showMacs = false
    @State private var showSignIn = false

    var body: some View {
        let macs = model.pairedMacs
        let mac = macs.selectedId.flatMap { id in macs.hosts.first { $0.id == id } }
        NavigationStack {
            Form {
                Section {
                    Picker("Appearance", selection: Binding(get: { appearance.mode },
                                                            set: { appearance.setMode($0) })) {
                        ForEach(AppearanceMode.allCases) { mode in
                            Text(mode.label).tag(mode)
                        }
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("settings.appearance")
                    Text("Showing \(colorScheme == .dark ? "Dark" : "Light")"
                         + (appearance.mode == .system ? ", following iOS" : ""))
                        .font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
                        .accessibilityIdentifier("settings.appearance.effective")
                } header: {
                    Text("Appearance")
                } footer: {
                    Text("Only this phone's look changes. The app icon follows the Home Screen's appearance.")
                }
                Section("Task list") {
                    Picker("Density", selection: Binding(get: { appearance.density },
                                                         set: { appearance.setDensity($0) })) {
                        ForEach(ListDensity.allCases) { density in
                            Text(density.label).tag(density)
                        }
                    }
                    .accessibilityIdentifier("settings.density")
                }
                Section {
                    LabeledContent("Text size", value: textSizeName)
                        .accessibilityIdentifier("settings.text-size")
                    LabeledContent("Reduce Motion", value: reduceMotion ? "On" : "Off")
                        .accessibilityIdentifier("settings.reduce-motion")
                } header: {
                    Text("Accessibility")
                } footer: {
                    Text("These follow iOS Settings → Accessibility and Display & Brightness.")
                }
                Section("This Mac") {
                    if let mac {
                        LabeledContent("Name", value: mac.name)
                        if let tasks = macs.projectedHostId == mac.id ? macs.remoteTasks : nil {
                            LabeledContent("Host", value: String(tasks.deviceId.prefix(8)))
                                .accessibilityIdentifier("settings.mac.host")
                        }
                        if let status = macs.liveStatus {
                            Text(status).font(Theme.mono(12)).foregroundStyle(Theme.textMuted)
                        }
                        if let route = mac.route {
                            Text(route).font(Theme.mono(12)).foregroundStyle(Theme.textMuted)
                        }
                        LabeledContent("Access expires", value: Date(timeIntervalSince1970: Double(mac.expiresAtMs) / 1000)
                            .formatted(date: .abbreviated, time: .omitted))
                    } else {
                        Text("No Mac selected.").foregroundStyle(Theme.textMuted)
                    }
                    Button("Manage paired Macs") { showMacs = true }
                        .accessibilityIdentifier("settings.manage-macs")
                }
                if Endpoints.edgeURL != nil {
                    Section("LUNA sync") {
                        Button("Log in to LUNA sync") { showSignIn = true }
                    }
                }
                Section("About") {
                    Text(Self.coreLine).font(Theme.mono(12)).foregroundStyle(Theme.textMuted)
                        .accessibilityIdentifier("settings.core")
                }
            }
            .font(Theme.sans(15))
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .accessibilityIdentifier("settings.done")
                }
            }
        }
        .sheet(isPresented: $showMacs) { PairedMacsView(model: model.pairedMacs) }
        .sheet(isPresented: $showSignIn) { SignInView() }
    }

    private var textSizeName: String {
        switch typeSize {
        case .xSmall: return "Extra small"
        case .small: return "Small"
        case .medium: return "Medium"
        case .large: return "Default"
        case .xLarge: return "Large"
        case .xxLarge: return "Extra large"
        case .xxxLarge: return "Largest"
        case .accessibility1: return "Accessibility 1"
        case .accessibility2: return "Accessibility 2"
        case .accessibility3: return "Accessibility 3"
        case .accessibility4: return "Accessibility 4"
        case .accessibility5: return "Accessibility 5"
        @unknown default: return "Custom"
        }
    }

    private static let coreLine: String = {
        let core = coreIdentity()
        return "LUNA core \(core.coreVersion) · SDK \(core.sdkVersion) · API \(core.apiVersion)"
    }()
}
