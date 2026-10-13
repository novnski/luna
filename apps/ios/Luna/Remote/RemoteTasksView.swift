import SwiftUI

/// RM-03's minimal native read slice: the paired Mac's own tasks and one
/// task's recent history, straight from the core's projections. Every list
/// says how fresh it is; cached or stale data is never shown as live. The
/// complete native transcript and shell are IOS-01 and IOS-02.
struct RemoteTasksSection: View {
    @Bindable var model: PairedMacsModel

    var body: some View {
        if let tasks = model.remoteTasks {
            VStack(alignment: .leading, spacing: 8) {
                Text("Tasks on this Mac").font(Theme.sans(14, weight: .semibold))
                Text(RemoteFreshness.text(tasks.freshness, confirmedAtMs: tasks.confirmedAtMs,
                                          connection: model.liveStatus))
                    .font(Theme.mono(11)).foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("remote.tasks.freshness")
                if tasks.tasks.isEmpty {
                    Text("No tasks yet.").font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
                }
                ForEach(tasks.tasks, id: \.taskId) { task in
                    Button { model.openTask(task.taskId) } label: {
                        HStack(alignment: .firstTextBaseline) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(task.title ?? "Untitled task").font(Theme.sans(14))
                                    .foregroundStyle(Theme.text).lineLimit(1)
                                Text(projectName(task.projectId, in: tasks))
                                    .font(Theme.sans(11)).foregroundStyle(Theme.textMuted).lineLimit(1)
                            }
                            Spacer()
                            Text(RemoteFreshness.status(task.status))
                                .font(Theme.sans(11)).foregroundStyle(Theme.textMuted)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("remote.task.\(task.taskId)")
                }
                if tasks.totalTasks > UInt32(tasks.tasks.count) {
                    Text("\(tasks.totalTasks - UInt32(tasks.tasks.count)) older tasks are on the Mac")
                        .font(Theme.sans(11)).foregroundStyle(Theme.textMuted)
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("remote.tasks")
        } else if let issue = model.tasksIssue {
            Text(issue).font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("remote.tasks.issue")
        }
    }

    private func projectName(_ projectId: String, in tasks: RemoteTasksProjection) -> String {
        tasks.projects.first { $0.projectId == projectId }?.name ?? "Project"
    }
}

/// One task's recent history (the core's bounded tail), oldest first.
struct RemoteTaskDetailView: View {
    @Bindable var model: PairedMacsModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let task = model.openTask {
                    Text(RemoteFreshness.text(task.freshness, confirmedAtMs: task.confirmedAtMs,
                                              connection: model.liveStatus))
                        .font(Theme.mono(11)).foregroundStyle(Theme.textMuted)
                        .accessibilityIdentifier("remote.task.freshness")
                    if task.removed {
                        Text("This task is no longer on the Mac.").font(Theme.sans(14))
                            .accessibilityIdentifier("remote.task.removed")
                    }
                    if task.totalEntries > UInt32(task.entries.count) {
                        Text("Showing the latest \(task.entries.count) of \(task.totalEntries) messages")
                            .font(Theme.sans(11)).foregroundStyle(Theme.textMuted)
                    }
                    ForEach(task.entries, id: \.entryId) { entry in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(role(entry)).font(Theme.sans(11, weight: .semibold))
                                .foregroundStyle(Theme.textMuted)
                            if !entry.text.isEmpty {
                                Text(entry.text + (entry.textTruncated ? " …" : "")).font(Theme.sans(14))
                                    .textSelection(.enabled)
                            }
                            if !entry.tools.isEmpty || entry.otherParts > 0 {
                                Text(extras(entry)).font(Theme.mono(11)).foregroundStyle(Theme.textMuted)
                            }
                        }
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: 12))
                        .accessibilityElement(children: .combine)
                        .accessibilityIdentifier("remote.entry.\(entry.entryId)")
                    }
                } else {
                    Text("Waiting for this task from the Mac…").font(Theme.sans(13))
                        .foregroundStyle(Theme.textMuted)
                }
            }
            .padding(20)
        }
        .background(Theme.bg)
        .navigationTitle(model.openTask?.task?.title ?? "Task")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func role(_ entry: RemoteEntryRecord) -> String {
        let who = switch entry.role { case "user": "You"; case "assistant": "Agent"; default: "System" }
        return entry.status == "streaming" ? "\(who) · writing…" : who
    }

    private func extras(_ entry: RemoteEntryRecord) -> String {
        var parts = entry.tools.map { "tool: \($0)" }
        if entry.otherParts > 0 { parts.append("+\(entry.otherParts) more") }
        if entry.error { parts.append("error") }
        return parts.joined(separator: " · ")
    }
}

/// How a projection's freshness reads. The first word is the state, so
/// tests and VoiceOver see `Live`, `Stale` or `Cached` first.
enum RemoteFreshness {
    static func text(_ freshness: ProjectionFreshness, confirmedAtMs: Int64, connection: String?) -> String {
        let confirmed = Date(timeIntervalSince1970: Double(confirmedAtMs) / 1000)
            .formatted(date: .omitted, time: .shortened)
        switch freshness {
        case .live:
            return "Live · confirmed \(confirmed)"
        case .stale:
            return "Stale · last confirmed \(confirmed)" + (connection.map { " · \($0)" } ?? "")
        case .cached:
            return "Cached on this phone · last confirmed \(confirmed)"
        }
    }

    static func status(_ status: String) -> String {
        switch status {
        case "working": "Working"
        case "awaitingApproval": "Needs approval"
        case "awaitingInput": "Needs input"
        case "errored": "Error"
        case "completed": "Done"
        default: "Idle"
        }
    }
}
