import Network
import os
import SwiftUI

/// RM-00 probe: runs a plan of direct-host descriptor reads against real
/// hosts on the LAN and records raw outcomes. The Mac-side checker judges
/// them; this app never decides pass or fail.
///
/// The plan arrives base64-encoded in `LUNA_RM00_PLAN`. Results are printed
/// as `RM00 {json}` lines and written to Documents/rm00-result.json. With
/// `LUNA_RM00_EXIT=1` the app exits when the plan finishes.
@main
struct RemoteProbeApp: App {
    @StateObject private var runner = ProbeRunner()

    var body: some Scene {
        WindowGroup {
            NavigationStack {
                List(runner.lines, id: \.self) { line in
                    Text(line).font(.caption.monospaced())
                }
                .navigationTitle("LUNA RM-00 probe")
            }
            .task { await runner.start() }
        }
    }
}

struct Plan: Decodable {
    struct Step: Decodable {
        let id: String
        /// Raw ticket text, deliberately unparsed here.
        let ticket: String
        let endpoint: UInt32
        /// `read` (default), `cancelAfterMs` or `releaseAfterMs`.
        let action: String?
        let ms: UInt64?
        let waitMs: UInt64?
    }

    let runId: String
    let prime: Step
    let primeTimeoutSecs: UInt64
    let cases: [Step]
}

/// Records every state the core reports, on whatever thread it reports it.
final class StateLog: ProbeListener, @unchecked Sendable {
    private let lock = NSLock()
    private var names: [String] = []

    func onState(state: ProbeState) {
        let name = switch state {
        case .idle: "idle"
        case .reading: "reading"
        case .verified: "verified"
        case .rejected(let reason, _, _): "rejected:\(reason)"
        case .cancelled: "cancelled"
        }
        let thread = Thread.isMainThread ? "main" : "background"
        lock.withLock { names.append("\(name)@\(thread)") }
    }

    func all() -> [String] {
        lock.withLock { names }
    }
}

@MainActor
final class ProbeRunner: ObservableObject {
    @Published var lines: [String] = ["waiting for a plan"]
    private var started = false
    private var results: [[String: Any]] = []

    func start() async {
        guard !started else { return }
        started = true
        let environment = ProcessInfo.processInfo.environment
        guard let encoded = environment["LUNA_RM00_PLAN"], let data = Data(base64Encoded: encoded),
              let plan = try? JSONDecoder().decode(Plan.self, from: data)
        else {
            emit(["event": "error", "detail": "no valid LUNA_RM00_PLAN"])
            return
        }
        let interface = await Self.currentInterface()
        emit(["event": "start", "runId": plan.runId, "interface": interface, "cases": plan.cases.count])
        let deadline = Date().addingTimeInterval(TimeInterval(plan.primeTimeoutSecs))
        var attempt = 0
        while true {
            attempt += 1
            let primed = await run(plan.prime, interface: interface)
            if primed["outcome"] as? String == "verified" {
                emit(["event": "primed", "attempts": attempt])
                break
            }
            guard Date() < deadline else {
                emit(["event": "error", "detail": "prime failed", "last": primed])
                finish(plan, environment)
                return
            }
            if attempt == 1 {
                emit(["event": "waiting", "detail": "allow local network access on this iPhone"])
            }
            try? await Task.sleep(for: .seconds(2))
        }
        for step in plan.cases {
            let result = await run(step, interface: interface)
            results.append(result)
            emit(["event": "case"].merging(result) { a, _ in a })
        }
        finish(plan, environment)
    }

    private func run(_ step: Plan.Step, interface: String) async -> [String: Any] {
        let transport = PinnedTransport(interfaceHint: interface)
        defer { transport.invalidate() }
        let states = StateLog()
        let started = Date()
        var result: [String: Any] = ["id": step.id]
        var core: DirectHostProbe?
        do {
            core = try DirectHostProbe(ticketJson: step.ticket, transport: transport, listener: states)
        } catch let error as ProbeError {
            result["outcome"] = "constructorError"
            result["error"] = Self.describe(error)
            return result
        } catch {
            result["outcome"] = "constructorError"
            result["error"] = "\(error)"
            return result
        }
        transport.core = core
        do {
            _ = try core?.readDescriptor(index: step.endpoint)
        } catch {
            result["outcome"] = "readError"
            result["error"] = "\(error)"
            return result
        }
        switch step.action {
        case "cancelAfterMs":
            try? await Task.sleep(for: .milliseconds(step.ms ?? 0))
            core?.cancel()
            try? await Task.sleep(for: .milliseconds(step.waitMs ?? 0))
        case "releaseAfterMs":
            try? await Task.sleep(for: .milliseconds(step.ms ?? 0))
            core = nil
            try? await Task.sleep(for: .milliseconds(step.waitMs ?? 0))
            result["outcome"] = "released"
            result["coreDeallocated"] = transport.core == nil
            result["transportEvents"] = transport.observedEvents()
            result["states"] = states.all()
            result["durationMs"] = Int(Date().timeIntervalSince(started) * 1000)
            return result
        default:
            let deadline = Date().addingTimeInterval(20)
            while case .reading = core?.state(), Date() < deadline {
                try? await Task.sleep(for: .milliseconds(25))
            }
        }
        guard let core else { return result }
        result.merge(Self.describe(core.state())) { _, new in new }
        result["states"] = states.all()
        result["transportEvents"] = transport.observedEvents()
        result["durationMs"] = Int(Date().timeIntervalSince(started) * 1000)
        return result
    }

    private func finish(_ plan: Plan, _ environment: [String: String]) {
        let document: [String: Any] = ["runId": plan.runId, "results": results]
        if let data = try? JSONSerialization.data(withJSONObject: document, options: [.sortedKeys]),
           let directory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first {
            try? data.write(to: directory.appendingPathComponent("rm00-result.json"))
        }
        emit(["event": "done", "runId": plan.runId])
        if environment["LUNA_RM00_EXIT"] == "1" {
            exit(0)
        }
    }

    private func emit(_ object: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
              let text = String(data: data, encoding: .utf8)
        else { return }
        print("RM00 \(text)")
        fflush(stdout)
        lines.append(text)
    }

    private static func describe(_ state: ProbeState) -> [String: Any] {
        switch state {
        case .idle:
            return ["outcome": "idle"]
        case .reading(_, let url):
            return ["outcome": "timeout", "url": url]
        case .verified(let device, let environment, let catalog, let url, let route):
            return ["outcome": "verified", "deviceId": device, "environmentId": environment,
                    "catalogVersion": catalog, "url": url, "route": describe(route)]
        case .rejected(let reason, let detail, let route):
            var value: [String: Any] = ["outcome": "rejected", "reason": "\(reason)", "detail": detail]
            if let route { value["route"] = describe(route) }
            return value
        case .cancelled:
            return ["outcome": "cancelled"]
        }
    }

    private static func describe(_ route: RouteObservation) -> [String: Any] {
        ["remoteAddress": route.remoteAddress ?? NSNull(), "localAddress": route.localAddress ?? NSNull(),
         "interface": route.interface, "proxied": route.proxied, "tlsVersion": route.tlsVersion ?? NSNull()]
    }

    private static func describe(_ error: ProbeError) -> String {
        switch error {
        case .Ticket(let detail): "ticket: \(detail)"
        case .Endpoint(let detail): "endpoint: \(detail)"
        case .Busy: "busy"
        }
    }

    /// The interface the current default path uses, from the system itself.
    private static func currentInterface() async -> String {
        await withCheckedContinuation { continuation in
            let monitor = NWPathMonitor()
            let resumed = OSAllocatedUnfairLock(initialState: false)
            monitor.pathUpdateHandler = { path in
                // The handler can fire more than once; resume exactly once.
                guard resumed.withLock({ done in defer { done = true }; return !done }) else { return }
                let name = if path.usesInterfaceType(.wifi) { "wifi" }
                    else if path.usesInterfaceType(.cellular) { "cellular" }
                    else if path.usesInterfaceType(.wiredEthernet) { "wired" }
                    else { "other" }
                monitor.cancel()
                continuation.resume(returning: name)
            }
            monitor.start(queue: DispatchQueue(label: "rm00.path"))
        }
    }
}
