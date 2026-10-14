// IOS-02 shell state: one `ShellScene` per window, the organization of the
// Mac's task rows, and what the selected Mac's connection means right now.
// Contract: docs/pending-tasks/ios-parity/ios-02/README.md.
//
// Ownership:
// - Mac records (projects, tasks, history) belong to the Mac. They arrive as
//   RM-03 projections through `PairedMacsModel` and are only read here.
// - Which Mac is selected is app-wide: one runtime follows one Mac.
// - Project, filter, search and the open task are per scene and restorable
//   (`ShellRestoration`); drafts are per Mac and task (`DraftBook`).
// - Appearance and density are local presentation (`AppearancePreferences`).

import Foundation
import Observation
import SwiftUI

/// One window's navigation. Value-only; the Mac's data is never copied in.
@MainActor
@Observable
final class ShellScene {
    /// nil: every project.
    var projectId: String?
    var filter: TaskFilter = .all
    var query = ""
    /// The open task's host id. A stable id, never a row index, so rotation,
    /// a split change or a reordered list cannot open another task.
    var taskId: String?
    /// The sidebar's list selection; nil after a compact back navigation,
    /// while the content keeps showing `projectId`.
    var sidebarSelection: String?
    /// iPad: the task list beside the open task in every orientation; the
    /// sidebar (Macs, projects) is one toolbar toggle away.
    var columns: NavigationSplitViewVisibility = .doubleColumn
    var compactColumn: NavigationSplitViewColumn = .content
    /// A one-time explanation of a route that could not be restored.
    var notice: String?
    var restored = false

    static let allProjectsTag = "projects:all"

    static func projectTag(_ projectId: String?) -> String {
        projectId.map { "project:\($0)" } ?? allProjectsTag
    }

    func applySidebar(_ tag: String) {
        if tag == Self.allProjectsTag {
            projectId = nil
        } else if tag.hasPrefix("project:") {
            projectId = String(tag.dropFirst("project:".count))
        }
    }

    func record(macId: String?) -> ShellSceneRecord {
        ShellSceneRecord(macId: macId, projectId: projectId, filter: filter.rawValue, query: query,
                         taskId: taskId, updatedAtMs: Int64(Date().timeIntervalSince1970 * 1000))
    }

    /// What restoration compares, without the time stamp.
    func routeKey(macId: String?) -> String {
        [macId ?? "", projectId ?? "", filter.rawValue, query, taskId ?? ""].joined(separator: "\u{1f}")
    }
}

/// The Mac's rows, narrowed by project, filter and search. Only rows the Mac
/// listed are considered; nothing is inferred about the ones it did not.
enum ShellOrganizer {
    static func included(_ task: RemoteTaskRecord, projectId: String?, filter: TaskFilter) -> Bool {
        if let projectId, task.projectId != projectId { return false }
        switch filter {
        case .all: return !task.archived
        case .attention:
            return !task.archived && ["awaitingApproval", "awaitingInput", "errored"].contains(task.status)
        case .working: return !task.archived && task.status == "working"
        case .unread: return !task.archived && task.unseen
        case .archived: return task.archived
        }
    }

    static func matches(_ task: RemoteTaskRecord, projectName: String, query: String) -> Bool {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return true }
        return [task.title ?? "Untitled task", projectName, task.preview ?? "", task.provider ?? ""]
            .contains { $0.localizedStandardContains(query) }
    }

    /// The visible rows in the Mac's own order (pinned first, then recent).
    static func visible(_ tasks: RemoteTasksProjection, projectId: String?, filter: TaskFilter,
                        query: String) -> [RemoteTaskRecord] {
        tasks.tasks.filter { task in
            included(task, projectId: projectId, filter: filter)
                && matches(task, projectName: projectName(task.projectId, in: tasks), query: query)
        }
    }

    static func count(_ tasks: RemoteTasksProjection, projectId: String?, filter: TaskFilter) -> Int {
        tasks.tasks.filter { included($0, projectId: projectId, filter: filter) }.count
    }

    static func projectName(_ projectId: String, in tasks: RemoteTasksProjection) -> String {
        tasks.projects.first { $0.projectId == projectId }?.name ?? "Project"
    }
}

/// What the selected Mac's connection means for the shell, from RM-02's
/// verification phase and RM-03's runtime state. Every case settles: a
/// failure is `unreachable` with the reason and a retry, never an endless
/// "opening" state.
enum MacLink: Equatable {
    /// The access was revoked or expired.
    case pairAgain(String)
    /// Verifying the Mac or opening the live connection.
    case connecting(String)
    case live(RemoteHostInfo)
    /// The runtime lost the Mac and is retrying by itself.
    case interrupted(String)
    /// The verification failed or the connection ended; retry is explicit.
    case unreachable(String)
    /// Another Mac is selected, or none was connected yet.
    case notConnected

    @MainActor
    static func of(_ host: PairedMacMetadata, in macs: PairedMacsModel) -> MacLink {
        let selected = macs.selectedId == host.id
        if host.needsPairing || (selected && macs.phase == "re-pair") {
            let reason = selected && macs.phase == "re-pair" ? macs.message : nil
            return .pairAgain(reason ?? pairAgainText)
        }
        guard selected else { return .notConnected }
        let runtime = macs.projectedHostId == host.id ? macs.liveState : nil
        switch runtime {
        case .open(let info, _)?:
            return .live(info)
        case .reconnecting(let attempt, let detail)?:
            return .interrupted(attempt == 0 ? "Reconnecting…" : "Reconnecting (\(attempt) failed): \(detail)")
        case .suspended?:
            return .interrupted("Paused while LUNA is in the background")
        case .waitingForNetwork?:
            return .interrupted("Waiting for a network connection")
        case .closed(let reason, let detail)?:
            switch reason {
            case .rePair: return .pairAgain(pairAgainText)
            case .local: break
            case .cancelled, .remote, .rejected, .failed: return .unreachable("The live connection ended: \(detail)")
            }
        case .idle?, .connecting?, nil:
            break
        }
        switch macs.phase {
        case "verifying", "authenticating", "redeeming", "saving":
            return .connecting("Verifying \(host.name)…")
        case "connected":
            return .connecting("Opening the live connection…")
        case "rejected", "uncertain":
            return .unreachable(macs.message ?? "This phone could not verify the Mac.")
        default:
            // "offline" is this phone's own word: disconnected, backgrounded
            // or secure storage locked. Only a failure explains itself.
            if let message = macs.message, message != PairedMacsModel.connectedMessage,
               runtime == nil || runtime == RemoteSessionState.idle {
                return .unreachable(message)
            }
            return .notConnected
        }
    }

    static let pairAgainText = "Access to this Mac was revoked or has expired. Pair again on the Mac."

    var isLive: Bool {
        if case .live = self { return true }
        return false
    }

    var canRetry: Bool {
        switch self {
        case .unreachable, .notConnected, .interrupted: true
        case .pairAgain, .connecting, .live: false
        }
    }
}

/// Words and colors for a Mac task status. The word comes first so
/// VoiceOver and tests read it as the state.
enum TaskStatusStyle {
    static func word(_ status: String) -> String { RemoteFreshness.status(status) }

    static func color(_ status: String) -> Color {
        switch status {
        case "working": Theme.statusWorking
        case "completed": Theme.statusCompleted
        case "errored": Theme.danger
        case "awaitingApproval", "awaitingInput": Theme.warning
        default: Theme.textMuted
        }
    }

    /// The row's spoken label: title, project, status, then the flags.
    static func accessibilityLabel(_ task: RemoteTaskRecord, projectName: String) -> String {
        var parts = [task.title ?? "Untitled task", projectName, word(task.status)]
        if task.unseen { parts.append("Unread") }
        if task.pinned { parts.append("Pinned") }
        if task.archived { parts.append("Archived") }
        let at = Date(timeIntervalSince1970: Double(task.lastActivityMs ?? task.createdAtMs) / 1000)
        parts.append(at.formatted(.relative(presentation: .named)))
        return parts.joined(separator: ", ")
    }
}
