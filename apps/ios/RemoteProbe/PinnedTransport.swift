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

    struct Decision {
        let accepted: Bool
        let presented: String?
        let detail: String
    }

    /// DER prefix of a P-256 SubjectPublicKeyInfo; the raw key follows it.
    private static let p256SpkiPrefix = Data([
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01,
        0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ])

    static func evaluate(trust: SecTrust, challengeHost: String, expectedHost: String, pin: String) -> Decision {
        guard challengeHost.caseInsensitiveCompare(expectedHost) == .orderedSame else {
            return Decision(accepted: false, presented: nil, detail: "challenge for \(challengeHost), expected \(expectedHost)")
        }
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate], let leaf = chain.first else {
            return Decision(accepted: false, presented: nil, detail: "no server certificate")
        }
        guard let key = SecCertificateCopyKey(leaf),
              let attributes = SecKeyCopyAttributes(key) as? [CFString: Any],
              attributes[kSecAttrKeyType] as? String == kSecAttrKeyTypeECSECPrimeRandom as String,
              attributes[kSecAttrKeySizeInBits] as? Int == 256,
              let raw = SecKeyCopyExternalRepresentation(key, nil) as Data?
        else {
            return Decision(accepted: false, presented: nil, detail: "server key is not ECDSA P-256")
        }
        let presented = SHA256.hash(data: p256SpkiPrefix + raw).map { String(format: "%02x", $0) }.joined()
        guard constantTimeEqual(presented, pin) else {
            return Decision(accepted: false, presented: presented, detail: "server key does not match the pin")
        }
        // The pinned leaf is the only anchor: it must be a valid, in-date
        // certificate signed by the pinned key itself.
        SecTrustSetPolicies(trust, SecPolicyCreateBasicX509())
        SecTrustSetAnchorCertificates(trust, [leaf] as CFArray)
        SecTrustSetAnchorCertificatesOnly(trust, true)
        var error: CFError?
        guard SecTrustEvaluateWithError(trust, &error) else {
            let reason = error.map { CFErrorCopyDescription($0) as String } ?? "unknown"
            return Decision(accepted: false, presented: presented, detail: "pinned certificate is not valid: \(reason)")
        }
        return Decision(accepted: true, presented: presented, detail: "pinned key and valid certificate")
    }

    private static func constantTimeEqual(_ a: String, _ b: String) -> Bool {
        let (x, y) = (Array(a.utf8), Array(b.utf8))
        guard x.count == y.count else { return false }
        return zip(x, y).reduce(0) { $0 | ($1.0 ^ $1.1) } == 0
    }

    private static func describe(_ version: tls_protocol_version_t) -> String {
        switch version {
        case .TLSv13: "TLSv1.3"
        case .TLSv12: "TLSv1.2"
        default: "other(\(version.rawValue))"
        }
    }
}
