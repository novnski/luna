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
    /// The SDK socket opened once this connect authenticated (IOS-00): its
    /// typed engine read names the host. Never persisted.
    var liveStatus: String?
    @ObservationIgnored private var live: RemoteHostSession?
    /// The access this connect authenticated with, for its live socket only.
    @ObservationIgnored private var access: SavedMac?
    private static let metadataKey = "luna.paired-macs.v1"

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
        disconnect()
        preview = nil
        incomingCode = nil
        selectedId = host.id
        let (status, token) = Keychain.remoteToken(account: host.id)
        guard status == errSecSuccess, let token else {
            if status == errSecItemNotFound || status == errSecDecode {
                phase = "re-pair"
                message = "Access is no longer saved on this phone. Pair again on the Mac."
                update(host.id) { $0.needsPairing = true }
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
            begin(core)
        } catch {
            phase = "re-pair"
            message = "This saved connection is invalid. Pair again on the Mac."
        }
    }
    func backgrounded() {
        let interrupted = ["redeeming", "saving"].contains(phase) && preview != nil
        disconnect()
        preview = nil
        incomingCode = nil
        if interrupted {
            phase = "uncertain"
            message = "Pairing was interrupted. Check the Mac's paired access and create a fresh code."
        }
    }
    func foregrounded() {
        if let selectedId, let saved = hosts.first(where: { $0.id == selectedId }), preview == nil {
            connect(saved)
        }
    }
    func disconnect() {
        generation &+= 1
        transport?.cancel()
        transport = nil
        owner = nil
        closeLive()
        access = nil
        phase = "offline"
    }
    func cancelEnrollment() {
        disconnect()
        preview = nil
        incomingCode = nil
        message = nil
    }
    func forget(_ host: PairedMacMetadata) {
        if selectedId == host.id { disconnect(); selectedId = nil }
        let status = Keychain.forgetRemoteToken(account: host.id)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            message = "Could not remove this saved connection. Unlock the phone and try again."
            return
        }
        hosts.removeAll { $0.id == host.id }
        persistMetadata()
        message = "Removed from this phone. To end its grant, revoke access on the Mac."
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
            message = "Connected. This phone verified the Mac and its access."
            if let access { openLive(access) }
        case .rePair(let reason):
            if let selectedId, preview == nil {
                _ = Keychain.forgetRemoteToken(account: selectedId)
                update(selectedId) { $0.needsPairing = true }
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
    /// Opens this connect's SDK socket. Only the current generation may change
    /// what it shows; disconnecting closes it (cancelling anything in flight).
    private func openLive(_ access: SavedMac) {
        closeLive()
        let session: RemoteHostSession
        do {
            session = try RemoteHostSession(mac: access)
        } catch {
            liveStatus = "Live connection unavailable for this saved access."
            return
        }
        live = session
        liveStatus = Self.liveText(session.state)
        let current = generation
        session.onChange = { [weak self, weak session] state in
            guard let self, let session, self.generation == current, self.live === session else { return }
            self.liveStatus = Self.liveText(state)
        }
        // Failures arrive as the session's closed state through onChange.
        Task { _ = try? await session.connect() }
    }
    private func closeLive() {
        live?.close()
        live = nil
        liveStatus = nil
    }
    private static func liveText(_ state: RemoteSessionState) -> String? {
        switch state {
        case .idle, .connecting:
            return "Opening live connection…"
        case .open(let host, _):
            return "Live · host \(host.deviceId.prefix(8)) · API \(host.apiVersion)"
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
