import CryptoKit
import Foundation
import Observation
import OSLog
import Security
import UIKit

/// Ordinary local route metadata. The bearer is intentionally absent.
struct PairedMacMetadata: Codable, Identifiable {
    let id: String
    var name: String
    let hostTicket: String
    let sessionId: String
    let expiresAtMs: UInt64
    let scopes: [String]
    let clientLabel: String
    var lastAuthenticated: Date?
    var route: String?
    var needsPairing = false

    func session(token: String) -> SavedMac {
        SavedMac(hostName: name, hostTicket: hostTicket, sessionId: sessionId, sessionToken: token,
                 expiresAtMs: expiresAtMs, scopes: scopes, clientLabel: clientLabel)
    }
}

@MainActor
@Observable
final class PairedMacsModel {
    var hosts: [PairedMacMetadata] = []
    var preview: PairingPreview?
    var message: String?
    var phase = "idle"
    var selectedId: String?
    var incomingCode: String?
    @ObservationIgnored private var owner: Enrollment?
    @ObservationIgnored private var transport: PinnedEnrollmentTransport?
    @ObservationIgnored private var generation: UInt64 = 0
    /// The selected Mac's live connection once a connect authenticated: the
    /// RM-03 runtime, whose typed engine read names the host (IOS-00). It
    /// reconnects, follows the Mac's tasks whether or not a view shows them,
    /// and survives closing this sheet and backgrounding. Only its bounded
    /// projection cache is persisted (`RemoteTaskCache`).
    var liveStatus: String?
    /// RM-03: the selected Mac's projects and tasks with their freshness.
    var remoteTasks: RemoteTasksProjection?
    /// Why the Mac's tasks are not followed, when they are not.
    var tasksIssue: String?
    /// RM-03: the opened task's detail (followed, or cached).
    var openTask: RemoteTaskProjection?
    var openTaskId: String?
    var showingTask = false
    /// IOS-02: the selected Mac's runtime state (nil without a runtime), so
    /// the shell tells connecting, interrupted and unreachable apart.
    private(set) var liveState: RemoteSessionState?
    /// IOS-02: the saved Mac whose runtime the projections above came from.
    /// The shell shows them only under that Mac, never under another.
    private(set) var projectedHostId: String?
    /// IOS-02: moves whenever projections were re-read, so a view that reads
    /// a task's detail by id observes one value.
    private(set) var projectionStamp: UInt64 = 0
    /// IOS-02: the tasks the shell's scenes show, followed again whenever the
    /// selected Mac's runtime is rebuilt.
    @ObservationIgnored private var shellFollows: (host: String, tasks: [String])?
    @ObservationIgnored private var live: RemoteHostSession?
    @ObservationIgnored private var liveHostId: String?
    @ObservationIgnored private var path: NetworkPathObserver?
    /// The access this connect authenticated with, for its live socket only.
    @ObservationIgnored private var access: SavedMac?
    private static let metadataKey = "luna.paired-macs.v1"
    /// RM-02's success message; the shell never reads it as a failure.
    static let connectedMessage = "Connected. This phone verified the Mac and its access."

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.metadataKey), data.count < 1_048_576,
           let saved = try? JSONDecoder().decode([PairedMacMetadata].self, from: data) {
            hosts = saved
        }
    }
    func prepare(code: String, address: String = "") {
        disconnect()
        do {
            let core = address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? try Enrollment(code: code)
                : try Enrollment.manual(code: code, endpoint: address)
            owner = core
            preview = core.preview()
            phase = "confirm"
            message = nil
        } catch {
            preview = nil
            phase = "rejected"
            message = "This pairing code is invalid, expired, or uses an unsupported route. Create a fresh code on the Mac."
        }
    }
    func confirm() {
        guard let owner, preview != nil else { return }
        begin(owner)
    }
    func connect(_ host: PairedMacMetadata) {
        // RM-03: the same Mac's runtime (and what it has shown) is kept.
        cancelEnrollmentTransport()
        if liveHostId != host.id { closeLive() }
        access = nil
        phase = "offline"
        preview = nil
        incomingCode = nil
        selectedId = host.id
        let (status, token) = Keychain.remoteToken(account: host.id)
        guard status == errSecSuccess, let token else {
            if status == errSecItemNotFound || status == errSecDecode {
                phase = "re-pair"
                message = "Access is no longer saved on this phone. Pair again on the Mac."
                update(host.id) { $0.needsPairing = true }
                closeLive(save: false)
                RemoteTaskCache.remove(account: host.id)
            } else {
                phase = "offline"
                message = "Access is unavailable while this phone is locked. Unlock it and reconnect."
            }
            return
        }
        do {
            let mac = host.session(token: token)
            let core = try Enrollment.restore(mac: mac)
            owner = core
            access = mac
            // Cached tasks show at once, labelled cached, while RM-02's check
            // and then the runtime's own verification run.
            prepareLive(mac, hostId: host.id)
            begin(core)
        } catch {
            phase = "re-pair"
            message = "This saved connection is invalid. Pair again on the Mac."
        }
    }
    func backgrounded() {
        let interrupted = ["redeeming", "saving"].contains(phase) && preview != nil
        cancelEnrollmentTransport()
        // RM-03: the runtime suspends (accepted Mac work continues) and its
        // projections, now stale, are saved for the next run.
        if let live, let liveHostId {
            live.suspend()
            RemoteTaskCache.save(live.cacheBytes, account: liveHostId)
        }
        phase = "offline"
        preview = nil
        incomingCode = nil
        if interrupted {
            phase = "uncertain"
            message = "Pairing was interrupted. Check the Mac's paired access and create a fresh code."
        }
    }
    func foregrounded() {
        // The runtime re-verifies the Mac by itself; RM-02's check runs too.
        live?.resume()
        if let selectedId, let saved = hosts.first(where: { $0.id == selectedId }), preview == nil {
            connect(saved)
        }
    }
    /// RM-03 network adapter: the device path changed (`NetworkPathObserver`).
    func networkChanged(available: Bool) {
        live?.networkChanged(available: available)
    }
    func disconnect() {
        cancelEnrollmentTransport()
        closeLive()
        access = nil
        phase = "offline"
    }
    /// Ends an enrollment or a review. The selected Mac's runtime keeps
    /// following its tasks: delivery does not depend on this sheet.
    func cancelEnrollment() {
        cancelEnrollmentTransport()
        if phase != "connected" { phase = "offline" }
        preview = nil
        incomingCode = nil
        message = nil
    }
    func forget(_ host: PairedMacMetadata) {
        if selectedId == host.id { disconnect(); selectedId = nil }
        if liveHostId == host.id { closeLive(save: false) }
        let status = Keychain.forgetRemoteToken(account: host.id)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            message = "Could not remove this saved connection. Unlock the phone and try again."
            return
        }
        RemoteTaskCache.remove(account: host.id)
        DraftBook.shared.forget(account: host.id)
        hosts.removeAll { $0.id == host.id }
        persistMetadata()
        message = "Removed from this phone. To end its grant, revoke access on the Mac."
    }
    /// RM-03: open a task of the selected Mac. The runtime follows it from
    /// now on (bounded); closing the view does not stop delivery.
    func openTask(_ taskId: String) {
        openTaskId = taskId
        showingTask = true
        guard let live else { openTask = nil; return }
        try? live.follow(taskId: taskId)
        openTask = live.task(taskId)
    }
    /// IOS-02: one task of the selected Mac as last confirmed (followed or
    /// cached), read from the runtime; nil without one.
    func taskDetail(_ taskId: String) -> RemoteTaskProjection? {
        _ = projectionStamp
        return live?.task(taskId)
    }
    /// IOS-02: follow the task a shell scene shows, without the sheet's
    /// navigation. The four most recent are kept (the core's own bound) and
    /// followed again whenever this Mac's runtime is rebuilt.
    func follow(_ taskId: String) {
        guard let host = selectedId else { return }
        var tasks = shellFollows?.host == host ? shellFollows?.tasks ?? [] : []
        tasks.removeAll { $0 == taskId }
        tasks.append(taskId)
        if tasks.count > 4 { tasks.removeFirst(tasks.count - 4) }
        shellFollows = (host: host, tasks: tasks)
        if liveHostId == host { try? live?.follow(taskId: taskId) }
    }
    private func cancelEnrollmentTransport() {
        generation &+= 1
        transport?.cancel()
        transport = nil
        owner = nil
    }
    private func begin(_ core: Enrollment) {
        generation &+= 1
        let current = generation
        transport?.cancel()
        transport = PinnedEnrollmentTransport(owner: core) { [weak self] action in
            Task { @MainActor in
                guard let self, self.generation == current else { return }
                self.receive(action, core: core)
            }
        }
        var label = UIDevice.current.name
        while label.utf8.count > 100 { label.removeLast() }
        if label.isEmpty { label = "iPhone" }
        receive(core.start(endpointIndex: 0, clientLabel: label), core: core)
    }
    private func receive(_ action: EnrollmentAction, core: Enrollment) {
        phase = core.phase()
        switch action {
        case .request(let request):
            message = nil
            transport?.send(request)
        case .save(let requestId, let mac):
            let id = account(core.preview())
            let status = Keychain.saveRemoteToken(mac.sessionToken, account: id)
            if status != errSecSuccess {
                Logger(subsystem: Bundle.main.bundleIdentifier ?? "Luna", category: "Connections")
                    .error("Secure storage write failed, OSStatus \(status, privacy: .public)")
            }
            if status == errSecSuccess {
                let previous = hosts.first(where: { $0.id == id })
                let metadata = PairedMacMetadata(id: id, name: mac.hostName,
                    hostTicket: mac.hostTicket, sessionId: mac.sessionId, expiresAtMs: mac.expiresAtMs,
                    scopes: mac.scopes, clientLabel: mac.clientLabel,
                    lastAuthenticated: previous?.lastAuthenticated, route: previous?.route)
                hosts.removeAll { $0.id == id }
                hosts.append(metadata)
                persistMetadata()
                selectedId = id
                access = mac
                // The grant is now saved. Authentication failures belong to
                // this selected session, and bootstrap input is no longer needed.
                preview = nil
                incomingCode = nil
            }
            receive(core.persisted(requestId: requestId, success: status == errSecSuccess), core: core)
        case .connected(let sessionId, let route):
            if let host = hosts.first(where: { $0.sessionId == sessionId }) {
                selectedId = host.id
                update(host.id) {
                    $0.lastAuthenticated = Date()
                    $0.route = [route.interface, route.remoteAddress].compactMap { $0 }.joined(separator: " · ")
                    if route.proxied { $0.route = "Configured proxy · " + ($0.route ?? "") }
                    $0.needsPairing = false
                }
            }
            preview = nil
            incomingCode = nil
            message = Self.connectedMessage
            if let access, let selectedId { startLive(access, hostId: selectedId) }
        case .rePair(let reason):
            if let selectedId, preview == nil {
                _ = Keychain.forgetRemoteToken(account: selectedId)
                update(selectedId) { $0.needsPairing = true }
                if liveHostId == selectedId { closeLive(save: false) }
                RemoteTaskCache.remove(account: selectedId)
            }
            message = reason
        case .rejected(let reason):
            if preview != nil && core.retryAvailable() { phase = "confirm" }
            message = reason
        case .uncertain(let reason):
            message = reason
        case .ignore: break
        }
    }
    /// RM-03: the selected Mac's runtime, restoring its saved projections
    /// (shown as cached). Only the current runtime may change what shows.
    private func prepareLive(_ access: SavedMac, hostId: String) {
        if let live, liveHostId == hostId, !Self.isClosed(live.state) { return }
        closeLive()
        let session: RemoteHostSession
        do {
            session = try RemoteHostSession(following: access, cache: RemoteTaskCache.load(account: hostId))
        } catch {
            liveStatus = "Live connection unavailable for this saved access."
            return
        }
        live = session
        liveHostId = hostId
        projectedHostId = hostId
        liveState = session.state
        liveStatus = Self.liveText(session.state)
        session.onChange = { [weak self, weak session] state in
            guard let self, let session, self.live === session else { return }
            self.liveChanged(state)
        }
        session.onProjections = { [weak self, weak session] in
            guard let self, let session, self.live === session else { return }
            self.readProjections(session)
        }
        readProjections(session)
        if let follows = shellFollows, follows.host == hostId {
            for taskId in follows.tasks { try? session.follow(taskId: taskId) }
        }
        if path == nil {
            path = NetworkPathObserver { [weak self] available in self?.networkChanged(available: available) }
        }
    }
    /// The authenticated connect: start (or resume) the runtime. Failures
    /// arrive as its state through onChange; it keeps retrying by itself.
    private func startLive(_ access: SavedMac, hostId: String) {
        prepareLive(access, hostId: hostId)
        guard let live else { return }
        live.resume()
        Task { _ = try? await live.connect() }
    }
    private func liveChanged(_ state: RemoteSessionState) {
        liveState = state
        liveStatus = Self.liveText(state)
        // A revoked or expired grant never comes back: forget it like RM-02.
        if case .closed(.rePair, _) = state, let hostId = liveHostId {
            _ = Keychain.forgetRemoteToken(account: hostId)
            update(hostId) { $0.needsPairing = true }
            RemoteTaskCache.remove(account: hostId)
            if selectedId == hostId { phase = "re-pair" }
            message = "Access to this Mac was revoked or has expired. Pair again on the Mac."
        }
    }
    private func readProjections(_ session: RemoteHostSession) {
        remoteTasks = session.tasks
        tasksIssue = session.projectionIssue
        if let openTaskId { openTask = session.task(openTaskId) }
        projectionStamp &+= 1
    }
    private func closeLive(save: Bool = true) {
        if save, let live, let liveHostId { RemoteTaskCache.save(live.cacheBytes, account: liveHostId) }
        live?.close()
        live = nil
        liveHostId = nil
        projectedHostId = nil
        liveState = nil
        projectionStamp &+= 1
        liveStatus = nil
        remoteTasks = nil
        tasksIssue = nil
        openTask = nil
        openTaskId = nil
        showingTask = false
    }
    private static func isClosed(_ state: RemoteSessionState) -> Bool {
        if case .closed = state { return true }
        return false
    }
    private static func liveText(_ state: RemoteSessionState) -> String? {
        switch state {
        case .idle, .connecting:
            return "Opening live connection…"
        case .open(let host, _):
            return "Live · host \(host.deviceId.prefix(8)) · API \(host.apiVersion)"
        case .reconnecting(let attempt, let detail):
            return attempt == 0 ? "Reconnecting…" : "Reconnecting (\(attempt) failed): \(detail)"
        case .suspended:
            return "Paused while LUNA is in the background"
        case .waitingForNetwork:
            return "Waiting for a network connection"
        case .closed(let reason, let detail):
            switch reason {
            case .local: return nil
            case .rePair: return "Live connection refused. Pair again on the Mac."
            case .cancelled, .remote, .rejected, .failed: return "Live connection ended: \(detail)"
            }
        }
    }
    private func account(_ preview: PairingPreview) -> String {
        let input = preview.deviceId + "\0" + preview.environmentId + "\0" + preview.workspaceScope
        return SHA256.hash(data: Data(input.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    private func update(_ id: String, _ change: (inout PairedMacMetadata) -> Void) {
        guard let index = hosts.firstIndex(where: { $0.id == id }) else { return }
        change(&hosts[index]); persistMetadata()
    }
    private func persistMetadata() {
        if let data = try? JSONEncoder().encode(hosts) { UserDefaults.standard.set(data, forKey: Self.metadataKey) }
    }
}
