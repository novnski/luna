import CryptoKit
import Foundation
import Security

/// The platform half of the RM-00 split: `URLSession`, TLS trust and route
/// metrics live here; the Rust core decides what a reply means.
///
/// Trust is scoped to one request: the challenge host must be the host the
/// core asked for, the server key must be P-256 with the pinned
/// SubjectPublicKeyInfo hash, and the certificate must be a valid, in-date
/// self-signed certificate for that key. Nothing else is ever accepted.
final class PinnedTransport: NSObject, HostTransport, URLSessionDataDelegate, @unchecked Sendable {
    /// Weak: the core owns this transport. A completion that arrives after
    /// the core was released is dropped.
    weak var core: DirectHostProbe?

    private let lock = NSLock()
    private var tasks: [Int: Pending] = [:]
    private var taskByRequest: [UInt64: URLSessionDataTask] = [:]
    private var events: [[String: String]] = []
    private let interfaceHint: String
    private lazy var session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.requestCachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        configuration.urlCache = nil
        configuration.httpCookieStorage = nil
        configuration.waitsForConnectivity = false
        configuration.timeoutIntervalForRequest = 10
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        return URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
    }()

    private struct Pending {
        let requestId: UInt64
        let host: String
        let pin: String
        let maxBody: Int
        var status: Int = 0
        var body = Data()
        var tooLarge = false
        var trustFailure: String?
        var route: RouteObservation?
    }

    init(interfaceHint: String) {
        self.interfaceHint = interfaceHint
    }

    /// What the transport observed, in order: trust decisions, completions,
    /// dropped replies.
    func observedEvents() -> [[String: String]] {
        lock.withLock { events }
    }

    func invalidate() {
        session.invalidateAndCancel()
    }

    // MARK: HostTransport

    func get(request: TransportRequest) {
        guard let url = URL(string: request.url) else {
            report(request.requestId) { core in
                core.fail(requestId: request.requestId, failure: .network(detail: "unparseable URL"), route: nil)
            }
            return
        }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = "GET"
        let task = session.dataTask(with: urlRequest)
        lock.withLock {
            tasks[task.taskIdentifier] = Pending(
                requestId: request.requestId,
                host: request.host,
                pin: request.spkiSha256,
                maxBody: Int(request.maxBodyBytes)
            )
            taskByRequest[request.requestId] = task
        }
        task.resume()
    }

    func cancel(requestId: UInt64) {
        let task = lock.withLock { taskByRequest[requestId] }
        record(["event": "cancel", "request": String(requestId)])
        task?.cancel()
    }

    // MARK: URLSession delegate

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust,
              let pending = lock.withLock({ tasks[task.taskIdentifier] })
        else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        let decision = Self.evaluate(
            trust: trust, challengeHost: challenge.protectionSpace.host,
            expectedHost: pending.host, pin: pending.pin)
        record([
            "event": "trust",
            "request": String(pending.requestId),
            "accepted": decision.accepted ? "true" : "false",
            "presentedSpkiSha256": decision.presented ?? "",
            "detail": decision.detail,
        ])
        if decision.accepted {
            completionHandler(.useCredential, URLCredential(trust: trust))
        } else {
            lock.withLock { tasks[task.taskIdentifier]?.trustFailure = decision.detail }
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }

    func urlSession(
        _ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
        completionHandler: @escaping @Sendable (URLSession.ResponseDisposition) -> Void
    ) {
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        lock.withLock { tasks[dataTask.taskIdentifier]?.status = status }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        let overflow = lock.withLock { () -> Bool in
            guard var pending = tasks[dataTask.taskIdentifier] else { return false }
            if pending.body.count + data.count > pending.maxBody {
                pending.tooLarge = true
                pending.body = Data()
                tasks[dataTask.taskIdentifier] = pending
                return true
            }
            pending.body.append(data)
            tasks[dataTask.taskIdentifier] = pending
            return false
        }
        if overflow { dataTask.cancel() }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didFinishCollecting metrics: URLSessionTaskMetrics) {
        guard let transaction = metrics.transactionMetrics.last else { return }
        let interface: String
        if transaction.isCellular {
            interface = "cellular"
        } else if transaction.remoteAddress == nil {
            interface = "unknown"
        } else {
            interface = interfaceHint
        }
        let route = RouteObservation(
            remoteAddress: transaction.remoteAddress.map { "\($0):\(transaction.remotePort ?? 0)" },
            localAddress: transaction.localAddress.map { "\($0):\(transaction.localPort ?? 0)" },
            interface: interface,
            proxied: transaction.isProxyConnection,
            tlsVersion: transaction.negotiatedTLSProtocolVersion.map(Self.describe)
        )
        lock.withLock { tasks[task.taskIdentifier]?.route = route }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let pending = lock.withLock({ () -> Pending? in
            let pending = tasks.removeValue(forKey: task.taskIdentifier)
            if let pending { taskByRequest.removeValue(forKey: pending.requestId) }
            return pending
        }) else { return }
        let id = pending.requestId
        if let failure = pending.trustFailure {
            report(id) { $0.fail(requestId: id, failure: .trustRejected(detail: failure), route: pending.route) }
        } else if pending.tooLarge {
            report(id) { $0.fail(requestId: id, failure: .tooLarge, route: pending.route) }
        } else if let error = error as NSError? {
            let detail = "\(error.domain) \(error.code): \(error.localizedDescription)"
            report(id) { $0.fail(requestId: id, failure: .network(detail: detail), route: pending.route) }
        } else {
            let route = pending.route ?? RouteObservation(
                remoteAddress: nil, localAddress: nil, interface: "unknown", proxied: false, tlsVersion: nil)
            report(id) { $0.complete(requestId: id, status: UInt16(clamping: pending.status), body: pending.body, route: route) }
        }
    }

    // MARK: Helpers

    /// Hand a result to the core, or record that it was dropped because the
    /// core no longer exists.
    private func report(_ requestId: UInt64, _ deliver: (DirectHostProbe) -> Void) {
        if let core {
            record(["event": "delivered", "request": String(requestId)])
            deliver(core)
        } else {
            record(["event": "dropped", "request": String(requestId), "detail": "core released"])
        }
    }

    private func record(_ event: [String: String]) {
        lock.withLock { events.append(event) }
    }

    typealias Decision = PinnedTrust.Decision
    static func evaluate(trust: SecTrust, challengeHost: String, expectedHost: String, pin: String) -> Decision {
        PinnedTrust.evaluate(trust: trust, challengeHost: challengeHost, expectedHost: expectedHost, pin: pin)
    }

    private static func describe(_ version: tls_protocol_version_t) -> String {
        switch version {
        case .TLSv13: "TLSv1.3"
        case .TLSv12: "TLSv1.2"
        default: "other(\(version.rawValue))"
        }
    }
}
