// IOS-02: the open task in the paired-Mac shell. It shows the Mac's bounded
// history (RM-03) with its freshness, or exactly why it cannot: the Mac
// removed the task, the task has not arrived yet, or the Mac is out of
// reach and nothing of the task is saved here. The draft is this phone's
// and survives all of those. The full native transcript is IOS-01's and the
// composer that sends is IOS-03's. Contract:
// docs/pending-tasks/ios-parity/ios-02/README.md.

import SwiftUI
import UIKit

/// What a task route can show. Decided from the Mac's projection first; a
/// list row only names a task while its own detail has not arrived.
enum TaskRouteState {
    case shown(RemoteTaskProjection)
    /// The Mac answered that it no longer lists this task.
    case removed
    /// Connected or connecting: the Mac's answer is on its way.
    case loading(row: RemoteTaskRecord?)
    /// Not connected, and nothing of the task is saved on this phone.
    case unavailable(row: RemoteTaskRecord?, reason: String)

    static func resolve(detail: RemoteTaskProjection?, row: RemoteTaskRecord?, link: MacLink) -> TaskRouteState {
        if let detail {
            return detail.removed ? .removed : .shown(detail)
        }
        switch link {
        case .live, .connecting:
            return .loading(row: row)
        case .interrupted(let reason), .unreachable(let reason), .pairAgain(let reason):
            return .unavailable(row: row, reason: reason)
        case .notConnected:
            return .unavailable(row: row, reason: "Not connected to this Mac.")
        }
    }
}

struct ShellTaskDetailView: View {
    @Environment(AppModel.self) private var model
    @Bindable var scene: ShellScene
    let step: (Int) -> Void
    let retry: () -> Void
    let openSettings: () -> Void
    @Environment(\.horizontalSizeClass) private var sizeClass
    @State private var waitedLong = false
    @State private var tooLong = false
    @FocusState private var draftFocused: Bool

    var body: some View {
        let macs = model.pairedMacs
        let mac = macs.selectedId.flatMap { id in macs.hosts.first { $0.id == id } }
        Group {
            if let mac, let taskId = scene.taskId {
                route(mac: mac, taskId: taskId, macs: macs)
            } else {
                ContentUnavailableView {
                    Label("Select a task", systemImage: "list.bullet.rectangle")
                } description: {
                    Text(mac == nil ? "Choose a Mac, then one of its tasks." : "Choose a task to see its recent messages.")
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("shell.detail.empty")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bg)
        .task(id: "\(macs.selectedId ?? "")/\(scene.taskId ?? "")") {
            waitedLong = false
            tooLong = false
            guard let taskId = scene.taskId else { return }
            // The core follows it from now on, whether or not this view stays.
            macs.follow(taskId)
            try? await Task.sleep(for: .seconds(15))
            if !Task.isCancelled { waitedLong = true }
        }
    }

    @ViewBuilder
    private func route(mac: PairedMacMetadata, taskId: String, macs: PairedMacsModel) -> some View {
        let projected = macs.projectedHostId == mac.id
        let detail = projected ? macs.taskDetail(taskId) : nil
        let listed = projected ? macs.remoteTasks?.tasks.first(where: { $0.taskId == taskId }) : nil
        let link = MacLink.of(mac, in: macs)
        let draft = DraftBook.shared.text(account: mac.id, taskId: taskId)
        switch TaskRouteState.resolve(detail: detail, row: listed, link: link) {
        case .shown(let task):
            shown(task, mac: mac, taskId: taskId, link: link, draft: draft,
                  projectName: macs.remoteTasks.map { ShellOrganizer.projectName(task.task?.projectId ?? "", in: $0) })
        case .removed:
            removed(mac: mac, taskId: taskId, draft: draft)
        case .loading(let row):
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    VStack(spacing: 12) {
                        ProgressView()
                        Text(waitedLong ? "Still waiting for \(mac.name) to send this task."
                             : "Loading this task from \(mac.name)…")
                            .font(Theme.sans(15)).foregroundStyle(Theme.text).multilineTextAlignment(.center)
                        if let row {
                            Text("Listed on the Mac as “\(row.title ?? "Untitled task")”")
                                .font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
                        }
                        if waitedLong {
                            Button("Retry", action: retry).buttonStyle(.bordered)
                                .keyboardShortcut("r", modifiers: .command)
                                .accessibilityIdentifier("shell.detail.retry")
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("shell.detail.loading")
                    draftEditor(mac: mac, taskId: taskId, draft: draft)
                }
                .padding(20)
            }
            .navigationTitle(row?.title ?? "Task")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { stepButtons }
        case .unavailable(let row, let reason):
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ContentUnavailableView {
                        Label("Not available on this phone", systemImage: "wifi.slash")
                    } description: {
                        Text("\(reason) This task's messages are not saved on this phone, so nothing is shown in their place.")
                    } actions: {
                        Button("Retry", action: retry).buttonStyle(.bordered)
                            .keyboardShortcut("r", modifiers: .command)
                            .accessibilityIdentifier("shell.detail.retry")
                    }
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("shell.detail.unavailable")
                    draftEditor(mac: mac, taskId: taskId, draft: draft)
                }
                .padding(20)
            }
            .navigationTitle(row?.title ?? "Task")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { stepButtons }
        }
    }

    private func shown(_ task: RemoteTaskProjection, mac: PairedMacMetadata, taskId: String, link: MacLink,
                       draft: String, projectName: String?) -> some View {
        let title = task.task?.title ?? "Untitled task"
        return ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(title)
                    .font(Theme.sans(22, weight: .semibold)).foregroundStyle(Theme.text)
                    .accessibilityAddTraits(.isHeader)
                    .accessibilityIdentifier("shell.detail.title")
                if let row = task.task {
                    Text([projectName, TaskStatusStyle.word(row.status), row.provider, row.unseen ? "Unread" : nil]
                        .compactMap { $0 }.joined(separator: " · "))
                        .font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
                        .accessibilityIdentifier("shell.detail.meta")
                }
                Text(RemoteFreshness.text(task.freshness, confirmedAtMs: task.confirmedAtMs,
                                          connection: connectionWord(link)))
                    .font(Theme.mono(12)).foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("shell.detail.freshness")
                if task.freshness != .live, link.canRetry {
                    Button("Retry", action: retry).buttonStyle(.bordered)
                        .keyboardShortcut("r", modifiers: .command)
                        .accessibilityIdentifier("shell.detail.retry")
                }
                RemoteEntriesList(task: task)
                draftEditor(mac: mac, taskId: taskId, draft: draft)
            }
            .padding(20)
            .frame(maxWidth: 760, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { stepButtons }
    }

    /// The Mac's answer is final: the task is gone there. Nothing stale is
    /// shown in its place; the draft is kept until the user chooses.
    private func removed(mac: PairedMacMetadata, taskId: String, draft: String) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                ContentUnavailableView {
                    Label("This task is no longer on \(mac.name)", systemImage: "trash")
                } description: {
                    Text("It was deleted on the Mac, or its project was removed.")
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("shell.detail.removed")
                if !draft.isEmpty {
                    Text("Your draft is still on this phone:")
                        .font(Theme.sans(13, weight: .semibold)).foregroundStyle(Theme.textMuted)
                    Text(draft)
                        .font(Theme.sans(15)).foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: 10))
                        .accessibilityIdentifier("shell.detail.kept-draft")
                    ViewThatFits(in: .horizontal) {
                        HStack { draftActions(mac: mac, taskId: taskId, draft: draft) }
                        VStack(alignment: .leading) { draftActions(mac: mac, taskId: taskId, draft: draft) }
                    }
                }
                Button("Back to tasks") {
                    scene.taskId = nil
                    scene.compactColumn = .content
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("shell.detail.back")
            }
            .padding(20)
        }
        .navigationTitle("Task removed")
        .navigationBarTitleDisplayMode(.inline)
    }

    @ViewBuilder
    private func draftActions(mac: PairedMacMetadata, taskId: String, draft: String) -> some View {
        Button("Copy draft") { UIPasteboard.general.string = draft }
            .buttonStyle(.bordered)
            .accessibilityIdentifier("shell.draft.copy")
        Button("Discard draft", role: .destructive) {
            DraftBook.shared.discard(account: mac.id, taskId: taskId)
        }
        .buttonStyle(.bordered)
        .accessibilityIdentifier("shell.draft.discard")
    }

    /// This phone's draft for the task. It is saved as typed and never sent
    /// from here: the Mac cannot accept messages from the phone yet.
    private func draftEditor(mac: PairedMacMetadata, taskId: String, draft: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Draft").font(Theme.sans(13, weight: .semibold)).foregroundStyle(Theme.textMuted)
            TextField("Write a draft for this task", text: Binding(get: { draft }, set: { text in
                tooLong = !DraftBook.shared.setText(text, account: mac.id, taskId: taskId)
            }), axis: .vertical)
                .lineLimit(3...10)
                .font(Theme.sans(15))
                .foregroundStyle(Theme.text)
                .padding(10)
                .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: 10))
                .focused($draftFocused)
                .accessibilityLabel("Draft for this task")
                .accessibilityIdentifier("shell.draft")
                .toolbar {
                    ToolbarItemGroup(placement: .keyboard) {
                        Spacer()
                        Button("Done") { draftFocused = false }
                            .accessibilityIdentifier("shell.draft.done")
                    }
                }
            Text(tooLong ? "This draft is too long to keep on this phone (64 KB)."
                 : "Kept on this phone. \(mac.name) can't receive messages from the phone yet.")
                .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("shell.draft.note")
        }
    }

    private func connectionWord(_ link: MacLink) -> String? {
        switch link {
        case .interrupted(let text), .connecting(let text): text
        case .unreachable: "Can't reach the Mac"
        case .pairAgain: "Pair again"
        case .live, .notConnected: nil
        }
    }

    /// The task column's own controls carry its keyboard shortcuts, so they
    /// work whichever column iOS considers current (on iPhone this one is on
    /// top of the stack). Settings appears here only in compact width, where
    /// the list's toolbar is not on screen.
    @ToolbarContentBuilder
    private var stepButtons: some ToolbarContent {
        ToolbarItemGroup(placement: .topBarTrailing) {
            Button { step(-1) } label: { Image(systemName: "chevron.up") }
                .keyboardShortcut("[", modifiers: .command)
                .accessibilityLabel("Previous task")
                .accessibilityIdentifier("shell.detail.previous")
            Button { step(1) } label: { Image(systemName: "chevron.down") }
                .keyboardShortcut("]", modifiers: .command)
                .accessibilityLabel("Next task")
                .accessibilityIdentifier("shell.detail.next")
            if sizeClass == .compact {
                Button(action: openSettings) { Image(systemName: "gearshape") }
                    .keyboardShortcut(",", modifiers: .command)
                    .accessibilityLabel("Settings")
                    .accessibilityIdentifier("shell.detail.settings")
            }
        }
    }
}
