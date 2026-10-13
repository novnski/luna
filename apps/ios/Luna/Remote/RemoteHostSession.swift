import Foundation

/// The app's owner of one paired Mac's SDK session (IOS-00), and of its
/// reconnecting runtime (RM-03).
///
/// The Rust core owns what the socket means and how long it lives; this type
/// owns the Swift side of that contract:
/// - results of every call resume on the main actor;
/// - cancelling the awaiting Swift task cancels the core operation (UniFFI
///   alone would keep waiting for it);
/// - state changes are re-read on the main actor, so a late or reordered
///   notification can never show an older state;
/// - RM-03: projections are re-read only when the core's projection revision
///   moved, so a burst of Mac updates costs one main-actor read. They are
///   value copies: a view's data never changes under it.
/// Releasing it releases the core, which closes the socket and ends its
/// watches. `init(mac:)` never reconnects (IOS-00); `init(following:cache:)`
/// is the runtime, which reconnects, suspends and resumes, and follows the
/// Mac's tasks whether or not a view shows them.
@MainActor
final class RemoteHostSession {
    /// Holds the transport and the relay; neither refers back strongly.
    private let core: RemoteSession
    private(set) var state: RemoteSessionState = .idle
    /// RM-03: the Mac's projects and tasks as last confirmed, with freshness.
    private(set) var tasks: RemoteTasksProjection?
    private var projectionRevision: UInt64 = 0
    /// Called on the main actor after each change of `state`.
    var onChange: ((RemoteSessionState) -> Void)?
    /// RM-03: called on the main actor after projections changed.
    var onProjections: (() -> Void)?

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

    /// RM-03: the runtime. `cache` is what `cacheBytes` returned on an
    /// earlier run for this same Mac; it shows as `cached` until confirmed.
    init(following mac: SavedMac, cache: Data?, endpointIndex: UInt32 = 0) throws {
        let transport = PinnedSocketTransport()
        let relay = StateRelay()
        do {
            core = try RemoteSession.following(mac: mac, endpointIndex: endpointIndex,
                                               transport: transport, listener: relay, cache: cache)
        } catch {
            transport.close()
            throw error
        }
        transport.attach(core)
        relay.owner = self
        readProjections()
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

    // MARK: RM-03 lifecycle and projections

    /// Backgrounded: the socket closes, accepted Mac work continues, and
    /// projections turn stale. Save `cacheBytes` before the app suspends.
    func suspend() {
        core.suspend()
        refresh()
    }

    /// Foregrounded: reconnect now, with full verification.
    func resume() {
        core.resume()
        refresh()
    }

    func networkChanged(available: Bool) {
        core.networkChanged(available: available)
        refresh()
    }

    /// Follow a task's detail from now on, whether or not a view shows it.
    func follow(taskId: String) throws {
        try core.followTask(taskId: taskId)
        refresh()
    }

    /// One task's detail as last confirmed (followed or cached).
    func task(_ taskId: String) -> RemoteTaskProjection? {
        core.remoteTask(taskId: taskId)
    }

    var projectionIssue: String? { core.projectionIssue() }
    var cacheNote: String? { core.cacheNote() }
    /// Bounded projections to keep in protected storage for the next run.
    var cacheBytes: Data { core.cacheBytes() }

    fileprivate func refresh() {
        let next = core.state()
        if next != state {
            state = next
            onChange?(next)
        }
        if readProjections() { onProjections?() }
    }

    /// Re-reads projections when the core's revision moved.
    @discardableResult
    private func readProjections() -> Bool {
        let revision = core.projectionRevision()
        guard revision != projectionRevision else { return false }
        projectionRevision = revision
        tasks = core.remoteTasks()
        return true
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
/// main actor before anything reads the state. The core notifies again only
/// after the state was read, so notifications never pile up.
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
