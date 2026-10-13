import Foundation

/// The app's owner of one paired Mac's SDK session (IOS-00).
///
/// The Rust core owns what the socket means and how long it lives; this type
/// owns the Swift side of that contract:
/// - results of every call resume on the main actor;
/// - cancelling the awaiting Swift task cancels the core operation (UniFFI
///   alone would keep waiting for it);
/// - state changes are re-read on the main actor, so a late or reordered
///   notification can never show an older state.
/// Releasing it releases the core, which closes the socket and ends its
/// watches. It never reconnects; RM-03 owns reconnect.
@MainActor
final class RemoteHostSession {
    /// Holds the transport and the relay; neither refers back strongly.
    private let core: RemoteSession
    private(set) var state: RemoteSessionState = .idle
    /// Called on the main actor after each change of `state`.
    var onChange: ((RemoteSessionState) -> Void)?

    init(mac: SavedMac, endpointIndex: UInt32 = 0) throws {
        let transport = PinnedSocketTransport()
        let relay = StateRelay()
        do {
            core = try RemoteSession(mac: mac, endpointIndex: endpointIndex,
                                     transport: transport, listener: relay)
        } catch {
            transport.close()
            throw error
        }
        transport.attach(core)
        relay.owner = self
    }

    /// Verify the Mac and open its socket; the typed SDK read names the host.
    func connect() async throws -> RemoteHostInfo {
        let core = self.core
        let host = try await cancellable { try await core.connect(cancel: $0) }
        refresh()
        return host
    }

    func engineInfo() async throws -> RemoteHostInfo {
        let core = self.core
        return try await cancellable { try await core.engineInfo(cancel: $0) }
    }

    func watchTaskCommands(taskId: String) async throws -> TaskCommandWatch {
        let core = self.core
        let handle = try await cancellable { try await core.watchTaskCommands(taskId: taskId, cancel: $0) }
        return TaskCommandWatch(handle: handle)
    }

    func close() {
        core.close()
        refresh()
    }

    fileprivate func refresh() {
        let next = core.state()
        guard next != state else { return }
        state = next
        onChange?(next)
    }
}

/// One task-command watch. Cancelling the task awaiting `next()` ends the
/// watch and its engine stream, as `cancel()` does.
final class TaskCommandWatch: Sendable {
    private let handle: TaskCommandWatchHandle

    init(handle: TaskCommandWatchHandle) {
        self.handle = handle
    }

    /// The next change; nil once the watch ended.
    func next() async throws -> TaskCommandChange? {
        let handle = self.handle
        return try await withTaskCancellationHandler {
            try await handle.next()
        } onCancel: {
            handle.cancel()
        }
    }

    func cancel() {
        handle.cancel()
    }
}

/// Runs `operation` with a fresh `Cancellation` that the current Swift task's
/// cancellation fires.
private func cancellable<T>(_ operation: (Cancellation) async throws -> T) async throws -> T {
    let cancellation = Cancellation()
    return try await withTaskCancellationHandler {
        try await operation(cancellation)
    } onCancel: {
        cancellation.cancel()
    }
}

/// Receives the core's change notifications on its threads and hops to the
/// main actor before anything reads the state.
private final class StateRelay: RemoteSessionListener, @unchecked Sendable {
    private let lock = NSLock()
    private weak var _owner: RemoteHostSession?

    var owner: RemoteHostSession? {
        get { lock.withLock { _owner } }
        set { lock.withLock { _owner = newValue } }
    }

    func onChange() {
        guard let owner else { return }
        Task { @MainActor in owner.refresh() }
    }
}
