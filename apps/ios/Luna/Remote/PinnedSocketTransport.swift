import Foundation
import Security

/// The platform half of a `RemoteSession` (IOS-00): pinned HTTPS requests and
/// one pinned WebSocket, all through one ephemeral `URLSession`. The Rust core
/// decides what every result means; this type moves bytes, evaluates the pin
/// with `PinnedTrust` and reports each result once. It never logs a bearer,
/// socket ticket or frame, and never calls the core while holding its lock.
final class PinnedSocketTransport: NSObject, SocketTransport, URLSessionDataDelegate,
    URLSessionWebSocketDelegate, @unchecked Sendable {
    private struct Request {
        let request: EnrollmentRequest
        var status: UInt16 = 0
        var body = Data()
        var tooLarge = false
        var trustRejected = false
        var route: RouteObservation?
    }
    private struct Socket {
        let id: UInt64
        let host: String
        let pin: String
        let task: URLSessionWebSocketTask
        var trustRejected = false
        var ended = false
    }
    /// Keeps an idle socket's traffic inside `URLSession`'s idle timeout and
    /// finds a Mac that stopped answering. It never reopens anything.
    private static let pingInterval: Duration = .seconds(10)
    private let lock = NSLock()
    private var requests: [Int: Request] = [:]
    private var socket: Socket?
    private var closed = false
    /// The core owns this transport; the transport only refers back weakly.
    private weak var core: RemoteSession?
    private var urlSession: URLSession!

    override init() {
        super.init()
        let config = URLSessionConfiguration.ephemeral
        config.urlCache = nil
        config.httpCookieStorage = nil
        config.urlCredentialStorage = nil
        config.requestCachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        config.waitsForConnectivity = false
        config.timeoutIntervalForRequest = 10
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        urlSession = URLSession(configuration: config, delegate: self, delegateQueue: queue)
    }

    /// Set once, before the core's first request.
    func attach(_ core: RemoteSession) {
        lock.withLock { self.core = core }
    }

    private var attached: RemoteSession? { lock.withLock { core } }

    // MARK: SocketTransport (called by the core, from any thread)

    func sendRequest(request: EnrollmentRequest) {
        guard let url = URL(string: request.url), url.scheme == "https" else {
            attached?.requestFailed(requestId: request.requestId, failure: .network(detail: "Invalid route"))
            return
        }
        var input = URLRequest(url: url)
        input.httpMethod = request.method
        if !request.body.isEmpty {
            input.httpBody = request.body
            input.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let authorization = request.authorization {
            input.setValue(authorization, forHTTPHeaderField: "Authorization")
        }
        let task = urlSession.dataTask(with: input)
        let accepted = lock.withLock { () -> Bool in
            guard !closed else { return false }
            requests[task.taskIdentifier] = Request(request: request)
            return true
        }
        guard accepted else { task.cancel(); return }
        task.resume()
    }

    func openSocket(request: SocketOpenRequest) {
        guard let url = URL(string: request.url), url.scheme == "wss" else {
            attached?.socketClosed(socketId: request.socketId, failure: .network(detail: "Invalid route"))
            return
        }
        var input = URLRequest(url: url)
        input.timeoutInterval = 30
        input.setValue(request.authorization, forHTTPHeaderField: "Authorization")
        let task = urlSession.webSocketTask(with: input)
        task.maximumMessageSize = Int(request.maxMessageBytes)
        let accepted = lock.withLock { () -> Bool in
            guard !closed, socket == nil else { return false }
            socket = Socket(id: request.socketId, host: request.host, pin: request.spkiSha256, task: task)
            return true
        }
        guard accepted else { task.cancel(); return }
        task.resume()
    }

    func sendFrame(socketId: UInt64, frame: String) {
        guard let task = lock.withLock({ () -> URLSessionWebSocketTask? in
            guard !closed, let socket, socket.id == socketId, !socket.ended else { return nil }
            return socket.task
        }) else { return }
        // URLSession sends a task's messages in the order they were queued.
        task.send(.string(frame)) { [weak self] error in
            if error != nil { self?.ended(socketId, .network(detail: "Sending to the Mac failed")) }
        }
    }

    func close() {
        let task = lock.withLock { () -> URLSessionWebSocketTask? in
            closed = true
            requests.removeAll()
            return socket?.task
        }
        task?.cancel(with: .normalClosure, reason: nil)
        urlSession.invalidateAndCancel()
    }

    // MARK: URLSession delegate (serial delegate queue)

    func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        let expected = lock.withLock { () -> (host: String, pin: String)? in
            if let pending = requests[task.taskIdentifier] {
                return (pending.request.host, pending.request.spkiSha256)
            }
            if let socket, socket.task === task { return (socket.host, socket.pin) }
            return nil
        }
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust, let expected else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        let result = PinnedTrust.evaluate(trust: trust, challengeHost: challenge.protectionSpace.host,
                                          expectedHost: expected.host, pin: expected.pin)
        if result.accepted {
            completionHandler(.useCredential, URLCredential(trust: trust))
            return
        }
        lock.withLock {
            if requests[task.taskIdentifier] != nil {
                requests[task.taskIdentifier]?.trustRejected = true
            } else if socket?.task === task {
                socket?.trustRejected = true
            }
        }
        completionHandler(.cancelAuthenticationChallenge, nil)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        let status = UInt16(clamping: (response as? HTTPURLResponse)?.statusCode ?? 0)
        lock.withLock { requests[dataTask.taskIdentifier]?.status = status }
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
        let socketEnd = lock.withLock { () -> (id: UInt64, rejected: Bool)? in
            guard let socket, socket.task === task else { return nil }
            return (socket.id, socket.trustRejected)
        }
        if let socketEnd {
            let status = (task.response as? HTTPURLResponse)?.statusCode
            let failure: TransportFailure = socketEnd.rejected ? .trustRejected(detail: "Pin refused")
                : .network(detail: status.map { "The Mac refused the connection (HTTP \($0))" }
                    ?? "The connection to the Mac ended")
            ended(socketEnd.id, failure)
            return
        }
        guard let item = lock.withLock({ requests.removeValue(forKey: task.taskIdentifier) }),
              let core = attached else { return }
        let requestId = item.request.requestId
        if item.trustRejected {
            core.requestFailed(requestId: requestId, failure: .trustRejected(detail: "Pin refused"))
        } else if item.tooLarge {
            core.requestFailed(requestId: requestId, failure: .tooLarge)
        } else if error != nil {
            core.requestFailed(requestId: requestId, failure: .network(detail: "Route unavailable"))
        } else {
            let route = item.route ?? RouteObservation(remoteAddress: nil, localAddress: nil,
                interface: "unknown", proxied: false, tlsVersion: nil)
            core.requestCompleted(requestId: requestId, status: item.status, body: item.body, route: route)
        }
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask,
                    didOpenWithProtocol protocol: String?) {
        guard let id = lock.withLock({ () -> UInt64? in
            guard !closed, let socket, socket.task === webSocketTask, !socket.ended else { return nil }
            return socket.id
        }) else { return }
        attached?.socketOpened(socketId: id)
        receive(webSocketTask, id: id)
        keepAlive(webSocketTask, id: id)
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask,
                    didCloseWith closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        guard let id = lock.withLock({ socket?.task === webSocketTask ? socket?.id : nil }) else { return }
        ended(id, .network(detail: "The Mac closed the connection"))
    }

    // MARK: Socket lifetime

    /// Each text message reaches the core before the next is read, so a busy
    /// core slows the socket instead of queueing without bound.
    private func receive(_ task: URLSessionWebSocketTask, id: UInt64) {
        Task.detached { [weak self] in
            while true {
                let message: URLSessionWebSocketTask.Message
                do { message = try await task.receive() } catch {
                    self?.ended(id, .network(detail: "The connection to the Mac ended"))
                    return
                }
                guard let core = self?.attached else { return }
                switch message {
                case .string(let text): await core.receive(socketId: id, frame: text)
                case .data: continue // the gateway sends text frames only
                @unknown default: continue
                }
            }
        }
    }

    private func keepAlive(_ task: URLSessionWebSocketTask, id: UInt64) {
        Task.detached { [weak self] in
            while true {
                try? await Task.sleep(for: Self.pingInterval)
                guard let self, self.isOpen(id) else { return }
                task.sendPing { [weak self] error in
                    if error != nil { self?.ended(id, .network(detail: "The Mac stopped answering")) }
                }
            }
        }
    }

    private func isOpen(_ id: UInt64) -> Bool {
        lock.withLock { !closed && socket?.id == id && socket?.ended == false }
    }

    /// Reports the socket's end once; after `close()` nothing is reported.
    private func ended(_ id: UInt64, _ failure: TransportFailure) {
        let report = lock.withLock { () -> Bool in
            guard !closed, var current = socket, current.id == id, !current.ended else { return false }
            current.ended = true
            socket = current
            return true
        }
        guard report else { return }
        attached?.socketClosed(socketId: id, failure: failure)
    }
}
