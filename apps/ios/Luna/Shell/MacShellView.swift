// IOS-02: the paired-Mac shell. One adaptive `NavigationSplitView`: on iPad
// a sidebar (Macs, projects), the Mac's task list and the open task side by
// side; on iPhone (and a narrow iPad split) the same columns as one stack,
// opening on the task list. Everything shown comes from the selected Mac's
// RM-03 projections with its freshness, or is a truthful connecting,
// unreachable, removed or offline state. No demo or fixture data reaches
// this view. Contract: docs/pending-tasks/ios-parity/ios-02/README.md.

import SwiftUI
import UIKit

struct MacShellView: View {
    @Environment(AppModel.self) private var model
    @SceneStorage("luna.shell.scene") private var sceneId = ""
    @State private var scene = ShellScene()
    @State private var showSettings = false
    @FocusState private var searchFocused: Bool

    private var macs: PairedMacsModel { model.pairedMacs }

    private var selectedMac: PairedMacMetadata? {
        guard let id = macs.selectedId else { return nil }
        return macs.hosts.first { $0.id == id }
    }

    var body: some View {
        @Bindable var bound = scene
        NavigationSplitView(columnVisibility: $bound.columns, preferredCompactColumn: $bound.compactColumn) {
            ShellSidebar(scene: scene, choose: choose, openSettings: { showSettings = true })
        } content: {
            ShellTaskList(scene: scene, searchFocused: $searchFocused, choose: choose, retry: retry,
                          openSettings: { showSettings = true })
        } detail: {
            // One identity per task: another task gets its own column state
            // (wait timer, draft focus) and never relies on an in-place
            // update of the previous task's view.
            ShellTaskDetailView(scene: scene, step: step, retry: retry, openSettings: { showSettings = true })
                .id(scene.taskId ?? "")
        }
        .navigationSplitViewStyle(.balanced)
        .background { commands }
        .task { restore() }
        .onChange(of: scene.routeKey(macId: macs.selectedId)) { _, _ in persist() }
        .onChange(of: macs.selectedId) { previous, current in
            // Another Mac: this window's route named the previous Mac's ids.
            guard scene.restored, previous != nil, previous != current else { return }
            scene.taskId = nil
            scene.projectId = nil
            scene.query = ""
            scene.filter = .all
            scene.sidebarSelection = ShellScene.allProjectsTag
            scene.compactColumn = .content
        }
        .onChange(of: scene.sidebarSelection) { _, tag in
            if let tag { scene.applySidebar(tag) }
        }
        .onChange(of: scene.projectId) { _, projectId in
            scene.sidebarSelection = ShellScene.projectTag(projectId)
        }
        .sheet(isPresented: $showSettings) { ShellSettingsView() }
    }

    /// Hardware-keyboard commands for the whole shell: search, settings and
    /// reconnect, as SwiftUI shortcuts. Hidden from VoiceOver; the same
    /// actions are visible controls. Next and previous task (Command-] and
    /// Command-[) are on the task column's own Previous/Next buttons, and
    /// `ShellKeyCommands` carries them for a cold relaunch, when iOS has no
    /// first responder yet. A key press reaches only the first match, so an
    /// action never runs twice.
    private var commands: some View {
        ZStack {
            VStack {
                Button("Search tasks") {
                    // The key-command view steps aside before SwiftUI moves
                    // the keyboard to the search field.
                    NotificationCenter.default.post(name: .shellYieldKeyboard, object: nil)
                    searchFocused = true
                }
                .keyboardShortcut("f", modifiers: .command)
                Button("Settings") { showSettings = true }
                    .keyboardShortcut(",", modifiers: .command)
                Button("Reconnect") { retry() }
                    .keyboardShortcut("r", modifiers: .command)
            }
            .frame(width: 0, height: 0)
            .clipped()
            .opacity(0)
            ShellKeyCommands(commands: [
                ShellKeyCommand(title: "Next Task", input: "]", modifiers: .command) { step(1) },
                ShellKeyCommand(title: "Previous Task", input: "[", modifiers: .command) { step(-1) },
            ])
            .frame(width: 1, height: 1)
        }
        .accessibilityHidden(true)
    }

    // MARK: Route ownership

    /// Restores this window's route once: its own record, else the last
    /// changed one. A route naming a Mac that is no longer paired is dropped
    /// with a notice; a task id is restored only under the Mac it came from,
    /// and what it shows is whatever that Mac says about it now.
    private func restore() {
        guard !scene.restored else { return }
        if sceneId.isEmpty { sceneId = UUID().uuidString.lowercased() }
        let record = ShellRestoration.load(scene: sceneId)
        var mac: PairedMacMetadata?
        if let id = record?.macId {
            mac = macs.hosts.first { $0.id == id }
            if mac == nil {
                scene.notice = "The Mac this window had open is no longer paired with this phone."
            }
        }
        if mac == nil { mac = selectedMac }
        if mac == nil {
            mac = macs.hosts.filter { !$0.needsPairing }
                .max { ($0.lastAuthenticated ?? .distantPast) < ($1.lastAuthenticated ?? .distantPast) }
                ?? macs.hosts.first
        }
        // A Mac already selected is being connected or paired; leave it be.
        if let mac, macs.selectedId != mac.id { open(mac) }
        if let record, let mac, record.macId == mac.id {
            scene.projectId = record.projectId
            scene.filter = TaskFilter(rawValue: record.filter) ?? .all
            scene.query = record.query
            scene.taskId = record.taskId
        }
        scene.sidebarSelection = ShellScene.projectTag(scene.projectId)
        scene.compactColumn = scene.taskId == nil ? .content : .detail
        scene.restored = true
    }

    private func persist() {
        guard scene.restored, !sceneId.isEmpty else { return }
        ShellRestoration.save(scene.record(macId: macs.selectedId), scene: sceneId)
    }

    /// The user picked a Mac in the sidebar or the chooser.
    private func choose(_ host: PairedMacMetadata) {
        if macs.selectedId == host.id, MacLink.of(host, in: macs).isLive { return }
        open(host)
        scene.compactColumn = .content
    }

    /// One runtime follows one Mac. A Mac that must pair again is selected
    /// without connecting, after the previous Mac's runtime is closed, so its
    /// tasks can never show under this Mac's name.
    private func open(_ host: PairedMacMetadata) {
        if host.needsPairing {
            if macs.selectedId != host.id { macs.disconnect() }
            macs.selectedId = host.id
        } else {
            macs.connect(host)
        }
    }

    private func retry() {
        guard let mac = selectedMac else { return }
        if mac.needsPairing {
            model.showPairedMacs = true
        } else {
            macs.connect(mac)
        }
    }

    /// The next or previous task in the order the list shows.
    private func step(_ delta: Int) {
        guard let mac = selectedMac, macs.projectedHostId == mac.id, let tasks = macs.remoteTasks else { return }
        let order = ShellOrganizer.visible(tasks, projectId: scene.projectId, filter: scene.filter,
                                           query: scene.query).map(\.taskId)
        guard !order.isEmpty else { return }
        guard let current = scene.taskId, let index = order.firstIndex(of: current) else {
            scene.taskId = order.first
            scene.compactColumn = .detail
            return
        }
        let next = index + delta
        guard order.indices.contains(next) else { return }
        scene.taskId = order[next]
    }
}

// MARK: - Sidebar

/// Macs, the selected Mac's projects and Settings.
struct ShellSidebar: View {
    @Environment(AppModel.self) private var model
    @Bindable var scene: ShellScene
    let choose: (PairedMacMetadata) -> Void
    let openSettings: () -> Void

    var body: some View {
        let macs = model.pairedMacs
        let tasks = macs.selectedId.flatMap { id in macs.projectedHostId == id ? macs.remoteTasks : nil }
        // nil after a compact back navigation; the content keeps its project.
        List(selection: $scene.sidebarSelection) {
            Section("Macs") {
                ForEach(macs.hosts) { host in
                    Button { choose(host) } label: {
                        ShellMacRow(host: host, link: MacLink.of(host, in: macs),
                                    selected: macs.selectedId == host.id)
                    }
                    .accessibilityIdentifier("shell.mac.\(host.id.prefix(12))")
                }
                Button { model.showPairedMacs = true } label: {
                    Label("Manage paired Macs", systemImage: "laptopcomputer.and.iphone")
                        .font(Theme.sans(15))
                }
                .accessibilityIdentifier("shell.manage-macs")
            }
            if let tasks {
                Section("Projects") {
                    projectRow(tag: ShellScene.allProjectsTag, name: "All projects",
                               count: ShellOrganizer.count(tasks, projectId: nil, filter: .all))
                    ForEach(tasks.projects, id: \.projectId) { project in
                        projectRow(tag: ShellScene.projectTag(project.projectId), name: project.name,
                                   count: ShellOrganizer.count(tasks, projectId: project.projectId, filter: .all))
                    }
                }
            }
            Section {
                Button(action: openSettings) {
                    Label("Settings", systemImage: "gearshape").font(Theme.sans(15))
                }
                .accessibilityIdentifier("shell.sidebar.settings")
            }
        }
        .listStyle(.sidebar)
        .tint(Theme.accent)
        .navigationTitle("LUNA")
    }

    private func projectRow(tag: String, name: String, count: Int) -> some View {
        ShellProjectRow(name: name, count: count)
            .tag(tag)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(name), \(count) tasks")
            .accessibilityIdentifier("shell.sidebar.\(tag)")
    }
}

/// A project in the sidebar with how many of its tasks are not archived.
struct ShellProjectRow: View {
    let name: String
    let count: Int
    @Environment(\.backgroundProminence) private var prominence

    var body: some View {
        let styles = RowStyles(prominence)
        HStack(alignment: .firstTextBaseline) {
            Text(name).font(Theme.sans(15)).foregroundStyle(styles.primary).lineLimit(3)
            Spacer(minLength: 8)
            Text("\(count)").font(Theme.sans(13)).foregroundStyle(styles.secondary).monospacedDigit()
        }
    }
}

/// Row text styles: hierarchical on a selected (increased-prominence)
/// background, where the system makes them legible on the selection tint;
/// the theme's text and muted colors otherwise, which meet contrast on the
/// shell's backgrounds in light and dark (the system's secondary does not
/// on the light background).
struct RowStyles {
    let primary: AnyShapeStyle
    let secondary: AnyShapeStyle

    init(_ prominence: BackgroundProminence) {
        let prominent = prominence == .increased
        primary = prominent ? AnyShapeStyle(HierarchicalShapeStyle.primary) : AnyShapeStyle(Theme.text)
        secondary = prominent ? AnyShapeStyle(HierarchicalShapeStyle.secondary) : AnyShapeStyle(Theme.textMuted)
    }
}

/// A saved Mac and what its connection means now.
struct ShellMacRow: View {
    let host: PairedMacMetadata
    let link: MacLink
    let selected: Bool
    @Environment(\.backgroundProminence) private var prominence

    var body: some View {
        let styles = RowStyles(prominence)
        HStack(spacing: 10) {
            Image(systemName: "laptopcomputer")
                .foregroundStyle(styles.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(host.name).font(Theme.sans(15, weight: .medium)).foregroundStyle(styles.primary).lineLimit(2)
                Text(state).font(Theme.sans(12)).foregroundStyle(styles.secondary).lineLimit(2)
            }
            Spacer(minLength: 4)
            if selected {
                Image(systemName: "checkmark").foregroundStyle(styles.secondary).accessibilityHidden(true)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(host.name), \(state)")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var state: String {
        switch link {
        case .pairAgain: "Pair again"
        case .connecting: "Connecting…"
        case .live: "Live"
        case .interrupted: "Reconnecting…"
        case .unreachable: "Can't reach"
        case .notConnected: selected ? "Not connected" : "Saved"
        }
    }
}

// MARK: - Task list

/// The selected Mac's tasks: identity, freshness, filters and rows, or the
/// state that explains why there are none to show.
struct ShellTaskList: View {
    @Environment(AppModel.self) private var model
    @Environment(AppearancePreferences.self) private var appearance
    @Environment(\.horizontalSizeClass) private var sizeClass
    @Bindable var scene: ShellScene
    /// The list's own selection. It follows the route (`scene.taskId`) for
    /// the highlight, and a selection the list makes itself opens that task:
    /// the route never depends on how `List(selection:)` treats a tap in a
    /// given size class (IOS-02 rounds 1-7: on iPad a row tap opened
    /// nothing).
    @State private var listSelection: String?
    var searchFocused: FocusState<Bool>.Binding
    let choose: (PairedMacMetadata) -> Void
    let retry: () -> Void
    let openSettings: () -> Void

    var body: some View {
        let macs = model.pairedMacs
        let mac = macs.selectedId.flatMap { id in macs.hosts.first { $0.id == id } }
        let tasks = mac.flatMap { mac in macs.projectedHostId == mac.id ? macs.remoteTasks : nil }
        Group {
            if let mac {
                let link = MacLink.of(mac, in: macs)
                if let tasks {
                    list(tasks, mac: mac, link: link)
                } else {
                    ShellConnectionState(mac: mac, link: link, issue: macs.tasksIssue, retry: retry,
                                         pairAgain: { model.showPairedMacs = true })
                }
            } else {
                chooser(macs)
            }
        }
        .background(Theme.bg)
        .onChange(of: scene.taskId, initial: true) { _, taskId in
            if listSelection != taskId { listSelection = taskId }
        }
        .onChange(of: listSelection) { _, selection in
            if let selection {
                open(selection)
            } else if sizeClass == .compact, scene.taskId != nil {
                // Back from the task on iPhone: the user left it.
                scene.taskId = nil
            }
        }
        .searchable(text: $scene.query, prompt: "Search tasks")
        .searchFocused(searchFocused)
        .navigationTitle(title(tasks))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                if let tasks { projectMenu(tasks) }
                Button { model.showPairedMacs = true } label: {
                    Image(systemName: "laptopcomputer")
                }
                .accessibilityLabel("Paired Macs")
                .accessibilityIdentifier("remote.open")
                Button(action: openSettings) { Image(systemName: "gearshape") }
                    .accessibilityLabel("Settings")
                    .accessibilityIdentifier("shell.settings")
            }
        }
    }

    private func title(_ tasks: RemoteTasksProjection?) -> String {
        guard let projectId = scene.projectId else { return "Tasks" }
        return tasks?.projects.first(where: { $0.projectId == projectId })?.name ?? "Project"
    }

    private func list(_ tasks: RemoteTasksProjection, mac: PairedMacMetadata, link: MacLink) -> some View {
        let rows = ShellOrganizer.visible(tasks, projectId: scene.projectId, filter: scene.filter, query: scene.query)
        let pinned = rows.filter(\.pinned)
        let rest = rows.filter { !$0.pinned }
        let density = appearance.density
        return List(selection: $listSelection) {
            Section {
                ShellListHeader(mac: mac, tasks: tasks, link: link, retry: retry)
                if let notice = scene.notice {
                    ShellNotice(text: notice) { scene.notice = nil }
                }
                ShellFilterBar(scene: scene, tasks: tasks) { searchFocused.wrappedValue = false }
                if let projectId = scene.projectId, tasks.freshness == .live,
                   !tasks.projects.contains(where: { $0.projectId == projectId }) {
                    Text("This project is no longer on \(mac.name).")
                        .font(Theme.sans(14)).foregroundStyle(Theme.text)
                        .accessibilityIdentifier("shell.project.missing")
                }
                if rows.isEmpty {
                    emptyRow(tasks: tasks)
                }
            }
            if !pinned.isEmpty {
                Section {
                    ForEach(pinned, id: \.taskId) { task in row(task, tasks: tasks, density: density) }
                } header: {
                    Text("Pinned").font(Theme.sans(12, weight: .semibold))
                }
            }
            if !rest.isEmpty {
                Section {
                    ForEach(rest, id: \.taskId) { task in row(task, tasks: tasks, density: density) }
                } header: {
                    if !pinned.isEmpty { Text("Tasks").font(Theme.sans(12, weight: .semibold)) }
                }
            }
            if tasks.totalTasks > UInt32(tasks.tasks.count) {
                Text("\(tasks.totalTasks - UInt32(tasks.tasks.count)) older tasks are on the Mac and not listed here.")
                    .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
            }
        }
        .listStyle(.plain)
        .tint(Theme.accent)
        .scrollContentBackground(.hidden)
    }

    /// A tagged row (the list's highlight) opened by an explicit full-row
    /// button; whichever of the two receives the tap opens the task.
    private func row(_ task: RemoteTaskRecord, tasks: RemoteTasksProjection, density: ListDensity) -> some View {
        let projectName = ShellOrganizer.projectName(task.projectId, in: tasks)
        return Button {
            open(task.taskId)
        } label: {
            ShellTaskRow(task: task, projectName: projectName, density: density)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(TaskStatusStyle.accessibilityLabel(task, projectName: projectName))
        .accessibilityIdentifier("shell.task.\(task.taskId)")
        .tag(task.taskId)
        .contextMenu {
            Button {
                open(task.taskId)
            } label: { Label("Open", systemImage: "arrow.right.circle") }
            Button {
                UIPasteboard.general.string = task.title ?? "Untitled task"
            } label: { Label("Copy Title", systemImage: "doc.on.doc") }
        }
    }

    /// The one way the list opens a task: the route, then the compact
    /// column (what restoring a route does too).
    private func open(_ taskId: String) {
        if scene.taskId != taskId { scene.taskId = taskId }
        scene.compactColumn = .detail
    }

    private func emptyRow(tasks: RemoteTasksProjection) -> some View {
        let filtered = scene.filter != .all || scene.projectId != nil
            || !scene.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return Text(filtered ? "No tasks match. Clear the filters to see every task."
                    : tasks.tasks.isEmpty ? "No tasks on this Mac yet." : "Every task here is archived.")
            .font(Theme.sans(14)).foregroundStyle(Theme.textMuted)
            .accessibilityIdentifier("shell.list.empty")
    }

    private func projectMenu(_ tasks: RemoteTasksProjection) -> some View {
        Menu {
            Button { scene.projectId = nil } label: {
                if scene.projectId == nil {
                    Label("All projects", systemImage: "checkmark")
                } else {
                    Text("All projects")
                }
            }
            ForEach(tasks.projects, id: \.projectId) { project in
                Button { scene.projectId = project.projectId } label: {
                    if scene.projectId == project.projectId {
                        Label(project.name, systemImage: "checkmark")
                    } else {
                        Text(project.name)
                    }
                }
            }
        } label: {
            Image(systemName: scene.projectId == nil ? "folder" : "folder.fill")
        }
        .accessibilityLabel("Project: \(title(tasks))")
        .accessibilityIdentifier("shell.project")
    }

    private func chooser(_ macs: PairedMacsModel) -> some View {
        ContentUnavailableView {
            Label("Choose a Mac", systemImage: "laptopcomputer")
        } description: {
            Text(scene.notice ?? "Pick one of the Macs paired with this phone.")
        } actions: {
            ForEach(macs.hosts) { host in
                Button(host.name) { choose(host) }
                    .buttonStyle(.bordered)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("shell.state.choose")
    }
}

/// Which Mac this is, how fresh its list is, and what the phone can do.
struct ShellListHeader: View {
    let mac: PairedMacMetadata
    let tasks: RemoteTasksProjection
    let link: MacLink
    let retry: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("\(mac.name) · host \(tasks.deviceId.prefix(8))")
                .font(Theme.sans(14, weight: .semibold)).foregroundStyle(Theme.text)
                .accessibilityIdentifier("shell.mac.identity")
            Text(RemoteFreshness.text(tasks.freshness, confirmedAtMs: tasks.confirmedAtMs, connection: connection))
                .font(Theme.mono(12)).foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("shell.freshness")
            if let problem {
                Text(problem).font(Theme.sans(13)).foregroundStyle(Theme.text)
                    .accessibilityIdentifier("shell.connection.problem")
                if link.canRetry {
                    Button("Retry", action: retry)
                        .buttonStyle(.bordered)
                        .accessibilityIdentifier("shell.retry")
                }
            }
            Text(tasks.capabilities.contains("remote-task-control-v1")
                 ? "This version of LUNA for iPhone shows tasks only."
                 : "Read-only: this Mac does not accept task changes from the phone yet.")
                .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("shell.read-only")
        }
        .padding(.vertical, 4)
    }

    /// The live connection's word for a stale list (never RM-03's
    /// "opening" text when the Mac could not be verified).
    private var connection: String? {
        switch link {
        case .interrupted(let text), .connecting(let text): text
        case .unreachable: "Can't reach the Mac"
        case .pairAgain: "Pair again"
        case .live, .notConnected: nil
        }
    }

    private var problem: String? {
        switch link {
        case .unreachable(let reason): "Can't reach \(mac.name). \(reason)"
        case .pairAgain(let reason): reason
        case .notConnected: "Not connected to \(mac.name)."
        case .interrupted, .connecting, .live: nil
        }
    }
}

/// Organization filters with their counts, and one way back to everything.
struct ShellFilterBar: View {
    @Bindable var scene: ShellScene
    let tasks: RemoteTasksProjection
    let dismissSearch: () -> Void
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Wraps instead of scrolling sideways, so at the largest text
            // sizes every filter stays on screen and reachable.
            FlowLayout(spacing: 8) {
                ForEach(TaskFilter.allCases) { filter in
                    chip(filter)
                }
            }
            .padding(.vertical, 2)
            if scene.filter != .all || scene.projectId != nil || !scene.query.isEmpty {
                Button("Clear filters") {
                    scene.filter = .all
                    scene.projectId = nil
                    scene.query = ""
                    dismissSearch()
                }
                .buttonStyle(.borderless)
                .font(Theme.sans(13))
                .frame(minHeight: 44)
                .accessibilityIdentifier("shell.clear-filters")
            }
        }
    }

    private func chip(_ filter: TaskFilter) -> some View {
        let count = ShellOrganizer.count(tasks, projectId: scene.projectId, filter: filter)
        let selected = scene.filter == filter
        return Button { scene.filter = filter } label: {
            Text("\(filter.label) \(count)")
                .font(Theme.sans(13, weight: selected ? .semibold : .regular))
                .foregroundStyle(Theme.text)
                .padding(.horizontal, 12)
                .padding(.vertical, typeSize.isAccessibilitySize ? 10 : 6)
                .frame(minHeight: 44)
                .background(selected ? Theme.elementActive : Theme.surfaceRaised.opacity(0.6), in: Capsule())
                .overlay(Capsule().strokeBorder(selected ? Theme.borderStrong : Theme.border, lineWidth: 1))
        }
        .buttonStyle(.borderless)
        .accessibilityLabel("\(filter.label), \(count) tasks")
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityIdentifier("shell.filter.\(filter.rawValue)")
    }
}

/// Places its subviews left to right, starting a new line when one does
/// not fit the proposed width.
struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var line: CGFloat = 0
        var widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                y += line + spacing
                x = 0
                line = 0
            }
            widest = max(widest, x + size.width)
            x += size.width + spacing
            line = max(line, size.height)
        }
        return CGSize(width: min(widest, width), height: y + line)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX
        var y = bounds.minY
        var line: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                y += line + spacing
                x = bounds.minX
                line = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            line = max(line, size.height)
        }
    }
}

/// A one-time explanation, dismissible.
struct ShellNotice: View {
    let text: String
    let dismiss: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(text).font(Theme.sans(13)).foregroundStyle(Theme.text)
            Spacer(minLength: 8)
            Button("Dismiss", action: dismiss).buttonStyle(.borderless).font(Theme.sans(13))
                .frame(minHeight: 44)
        }
        .accessibilityIdentifier("shell.notice")
    }
}

/// One Mac task row. Text uses hierarchical styles, so a selected row on
/// an increased-prominence background stays legible in light and dark.
struct ShellTaskRow: View {
    let task: RemoteTaskRecord
    let projectName: String
    let density: ListDensity
    @Environment(\.backgroundProminence) private var prominence
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        let prominent = prominence == .increased
        let styles = RowStyles(prominence)
        let large = typeSize.isAccessibilitySize
        let details = large ? AnyLayout(VStackLayout(alignment: .leading, spacing: 2))
            : AnyLayout(HStackLayout(alignment: .firstTextBaseline, spacing: 6))
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                if task.unseen {
                    Circle().fill(prominent ? Color.primary : Theme.accent)
                        .frame(width: 7, height: 7)
                }
                Text(task.title ?? "Untitled task")
                    .font(Theme.sans(16, weight: .medium))
                    .foregroundStyle(styles.primary)
                    .lineLimit(large ? 5 : 2)
                if task.pinned {
                    Image(systemName: "pin.fill").font(.caption2).foregroundStyle(styles.secondary)
                }
            }
            details {
                Text(projectName).foregroundStyle(styles.secondary).lineLimit(large ? 3 : 1)
                Text(TaskStatusStyle.word(task.status))
                    .foregroundStyle(prominent ? styles.primary : AnyShapeStyle(TaskStatusStyle.color(task.status)))
                if task.archived { Text("Archived").foregroundStyle(styles.secondary) }
                Text(relativeTime(task.lastActivityMs ?? task.createdAtMs)).foregroundStyle(styles.secondary)
            }
            .font(Theme.sans(13))
            if density == .comfortable, let preview = task.preview {
                Text(preview).font(Theme.sans(13)).foregroundStyle(styles.secondary).lineLimit(large ? 4 : 2)
            }
        }
        .padding(.vertical, density == .compact ? 2 : 6)
    }
}

/// Why the selected Mac has no list to show yet. Each state settles:
/// connecting ends in a list, a reason with Retry, or Pair again.
struct ShellConnectionState: View {
    let mac: PairedMacMetadata
    let link: MacLink
    let issue: String?
    let retry: () -> Void
    let pairAgain: () -> Void

    var body: some View {
        switch link {
        case .pairAgain(let reason):
            ContentUnavailableView {
                Label("Pair \(mac.name) again", systemImage: "key")
            } description: {
                Text(reason)
            } actions: {
                Button("Open Paired Macs", action: pairAgain).buttonStyle(.bordered)
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("shell.state.pair-again")
        case .connecting(let text), .interrupted(let text):
            VStack(spacing: 12) {
                ProgressView()
                Text(text).font(Theme.sans(15)).foregroundStyle(Theme.text).multilineTextAlignment(.center)
                if link.canRetry {
                    Button("Retry", action: retry).buttonStyle(.bordered).accessibilityIdentifier("shell.retry")
                }
            }
            .padding()
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("shell.state.connecting")
        case .unreachable(let reason):
            ContentUnavailableView {
                Label("Can't reach \(mac.name)", systemImage: "wifi.exclamationmark")
            } description: {
                Text("\(reason) Nothing from this Mac is saved on this phone yet.")
            } actions: {
                Button("Retry", action: retry).buttonStyle(.bordered).accessibilityIdentifier("shell.retry")
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("shell.state.unreachable")
        case .live:
            if let issue {
                ContentUnavailableView {
                    Label("Tasks unavailable", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(issue)
                } actions: {
                    Button("Retry", action: retry).buttonStyle(.bordered).accessibilityIdentifier("shell.retry")
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("shell.state.issue")
            } else {
                VStack(spacing: 12) {
                    ProgressView()
                    Text("Loading tasks from \(mac.name)…").font(Theme.sans(15)).foregroundStyle(Theme.text)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("shell.state.loading")
            }
        case .notConnected:
            ContentUnavailableView {
                Label("Not connected", systemImage: "laptopcomputer")
            } description: {
                Text("Connect to see the tasks on \(mac.name).")
            } actions: {
                Button("Connect", action: retry).buttonStyle(.bordered).accessibilityIdentifier("shell.retry")
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("shell.state.not-connected")
        }
    }
}
