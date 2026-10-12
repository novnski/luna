import Foundation
import Security

/// The platform's one bounded, pinned HTTP transport for an enrollment owner.
/// The portable core owns decisions; no credential or response body is logged.
final class PinnedEnrollmentTransport: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private struct Pending {
        let request: EnrollmentRequest
        var status: UInt16 = 0
        var body = Data()
        var tooLarge = false
        var trustRejected = false
        var route: RouteObservation?
    }
    private let lock = NSLock()
    private var requests: [Int: Pending] = [:]
    private var tasks: [UInt64: URLSessionDataTask] = [:]
    private let owner: Enrollment
    private let deliver: @Sendable (EnrollmentAction) -> Void
    private lazy var session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.urlCache = nil
        config.httpCookieStorage = nil
        config.requestCachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        config.waitsForConnectivity = false
        config.timeoutIntervalForRequest = 10
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        return URLSession(configuration: config, delegate: self, delegateQueue: queue)
    }()
    init(owner: Enrollment, deliver: @escaping @Sendable (EnrollmentAction) -> Void) {
        self.owner = owner
        self.deliver = deliver
    }
    func send(_ request: EnrollmentRequest) {
        guard let url = URL(string: request.url), url.scheme == "https" else {
            deliver(owner.fail(requestId: request.requestId, failure: .network(detail: "Invalid route")))
            return
        }
        var input = URLRequest(url: url)
        input.httpMethod = request.method
        if !request.body.isEmpty {
            input.httpBody = request.body
            input.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let auth = request.authorization { input.setValue(auth, forHTTPHeaderField: "Authorization") }
        let task = session.dataTask(with: input)
        lock.withLock {
            requests[task.taskIdentifier] = Pending(request: request)
            tasks[request.requestId] = task
        }
        task.resume()
    }
    func cancel() {
        _ = owner.cancel()
        session.invalidateAndCancel()
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust,
              let pending = lock.withLock({ requests[task.taskIdentifier] }) else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        let result = PinnedTrust.evaluate(trust: trust, challengeHost: challenge.protectionSpace.host,
                                         expectedHost: pending.request.host, pin: pending.request.spkiSha256)
        if result.accepted { completionHandler(.useCredential, URLCredential(trust: trust)) }
        else {
            lock.withLock { requests[task.taskIdentifier]?.trustRejected = true }
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        lock.withLock { requests[dataTask.taskIdentifier]?.status = UInt16(clamping: (response as? HTTPURLResponse)?.statusCode ?? 0) }
        completionHandler(.allow)
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        let overflow = lock.withLock { () -> Bool in
            guard var item = requests[dataTask.taskIdentifier] else { return false }
            if item.body.count + data.count > Int(item.request.maxBodyBytes) {
                item.body = Data(); item.tooLarge = true
            } else { item.body.append(data) }
            requests[dataTask.taskIdentifier] = item
            return item.tooLarge
        }
        if overflow { dataTask.cancel() }
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didFinishCollecting metrics: URLSessionTaskMetrics) {
        guard let transaction = metrics.transactionMetrics.last else { return }
        let route = RouteObservation(remoteAddress: transaction.remoteAddress, localAddress: transaction.localAddress,
            interface: transaction.isCellular ? "cellular" : "non-cellular", proxied: transaction.isProxyConnection,
            tlsVersion: transaction.negotiatedTLSProtocolVersion.map { String(describing: $0) })
        lock.withLock { requests[task.taskIdentifier]?.route = route }
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let item = lock.withLock({ () -> Pending? in
            let result = requests.removeValue(forKey: task.taskIdentifier)
            if let result { tasks.removeValue(forKey: result.request.requestId) }
            return result
        }) else { return }
        if item.trustRejected {
            deliver(owner.fail(requestId: item.request.requestId, failure: .trustRejected(detail: "Pin refused")))
        } else if item.tooLarge {
            deliver(owner.fail(requestId: item.request.requestId, failure: .tooLarge))
        } else if error != nil {
            deliver(owner.fail(requestId: item.request.requestId, failure: .network(detail: "Route unavailable")))
        } else {
            let route = item.route ?? RouteObservation(remoteAddress: nil, localAddress: nil,
                interface: "unknown", proxied: false, tlsVersion: nil)
            deliver(owner.complete(requestId: item.request.requestId, status: item.status, body: item.body, route: route))
        }
    }
}
