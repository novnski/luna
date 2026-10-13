import AVFoundation
import SwiftUI

struct PairedMacsView: View {
    @Bindable var model: PairedMacsModel
    @Environment(\.dismiss) private var dismiss
    @State private var entering = false
    @State private var scanning = false
    @State private var code = ""
    @State private var address = ""
    @State private var forget: PairedMacMetadata?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    Text("Connect directly to a Mac you've paired with. The Mac runs the work and must stay awake and reachable.")
                        .font(Theme.sans(14)).foregroundStyle(Theme.textMuted)
                    if let preview = model.preview {
                        confirmation(preview)
                    } else if entering {
                        entry
                    } else {
                        Button { entering = true } label: {
                            Label("Add Mac", systemImage: "plus")
                        }.buttonStyle(.borderedProminent).accessibilityIdentifier("remote.add-mac")
                    }
                    if let message = model.message {
                        Text(message).font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
                            .accessibilityIdentifier("remote.status")
                    }
                    if model.hosts.isEmpty && !entering && model.preview == nil {
                        ContentUnavailableView("No Macs paired", systemImage: "laptopcomputer",
                            description: Text("On your Mac, open Settings → Connections → Control this Mac and select Add."))
                    }
                    ForEach(model.hosts) { host in
                        VStack(alignment: .leading, spacing: 10) {
                            HStack {
                                Image(systemName: "laptopcomputer")
                                Text(host.name).font(Theme.sans(17, weight: .semibold))
                                Spacer()
                                Text(host.needsPairing ? "Pair again" : model.selectedId == host.id && model.phase == "connected" ? "Connected" : "Saved")
                                    .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                            }
                            if let date = host.lastAuthenticated {
                                Text("Last verified \(date.formatted(date: .abbreviated, time: .shortened))")
                                    .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                            }
                            if let route = host.route { Text(route).font(Theme.mono(11)).foregroundStyle(Theme.textMuted) }
                            if model.selectedId == host.id, let live = model.liveStatus {
                                Text(live).font(Theme.mono(11)).foregroundStyle(Theme.textMuted)
                                    .accessibilityIdentifier("remote.live")
                            }
                            Text("Access expires \(Date(timeIntervalSince1970: Double(host.expiresAtMs) / 1000).formatted(date: .abbreviated, time: .omitted))")
                                .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                            HStack {
                                Button(host.needsPairing ? "Pair again" : "Connect") {
                                    if host.needsPairing { entering = true } else { model.connect(host) }
                                }.buttonStyle(.bordered).accessibilityIdentifier("remote.connect")
                                Button("Forget", role: .destructive) { forget = host }
                                    .buttonStyle(.bordered).accessibilityIdentifier("remote.forget")
                            }
                        }.padding(16).background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: 16))
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier("remote.saved-mac")
                    }
                    Text(Self.coreLine).font(Theme.mono(11)).foregroundStyle(Theme.textMuted)
                        .accessibilityIdentifier("remote.core-identity")
                }.padding(20)
            }.background(Theme.bg).navigationTitle("Paired Macs")
                .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { model.cancelEnrollment(); dismiss() } } }
                .sheet(isPresented: $scanning) {
                    NavigationStack {
                        PairingScanner { scanned in
                            scanning = false; code = scanned; model.prepare(code: scanned)
                        }.ignoresSafeArea(edges: .bottom)
                            .navigationTitle("Scan pairing code")
                            .navigationBarTitleDisplayMode(.inline)
                            .toolbar { ToolbarItem(placement: .topBarTrailing) {
                                Button("Close") { scanning = false }
                            } }
                    }
                }
                .confirmationDialog("Forget this Mac?", isPresented: Binding(
                    get: { forget != nil }, set: { if !$0 { forget = nil } })) {
                    Button("Forget from this phone", role: .destructive) {
                        if let forget { model.forget(forget) }; forget = nil
                    }
                } message: { Text("This removes the saved connection on this phone. Revoke access on the Mac to end its grant.") }
                .onChange(of: model.phase) { _, phase in
                    if phase == "connected" || (phase == "authenticating" && model.preview == nil) {
                        entering = false; code = ""; address = ""
                    }
                }
                .onAppear {
                    if let incoming = model.incomingCode { model.prepare(code: incoming) }
                }
        }
    }
    private var entry: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("On the Mac, select Add. Scan its QR code or enter the pairing code.")
                .font(Theme.sans(14)).foregroundStyle(Theme.textMuted)
            Button { scanning = true } label: { Label("Scan QR code", systemImage: "qrcode.viewfinder") }
                .buttonStyle(.bordered).accessibilityIdentifier("remote.scan")
            SecureField("Pairing code", text: $code)
                .textContentType(.oneTimeCode).textInputAutocapitalization(.never).autocorrectionDisabled()
                .textFieldStyle(.roundedBorder).accessibilityIdentifier("remote.pairing-code")
            TextField("Mac address (optional)", text: $address)
                .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                .textFieldStyle(.roundedBorder).accessibilityIdentifier("remote.address")
            Button("Review pairing") { model.prepare(code: code, address: address) }
                .buttonStyle(.borderedProminent).disabled(code.isEmpty).accessibilityIdentifier("remote.review")
            Button("Cancel") { code = ""; address = ""; entering = false; model.cancelEnrollment() }
        }.padding(16).background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: 16))
    }
    private func confirmation(_ preview: PairingPreview) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Pair with \(preview.hostName)?").font(Theme.sans(20, weight: .semibold))
            Text(preview.endpoints.joined(separator: "\n")).font(Theme.mono(12)).foregroundStyle(Theme.textMuted)
            Text("Host \(preview.deviceId.prefix(8)) · profile \(preview.environmentId.prefix(8))")
                .font(Theme.mono(11)).foregroundStyle(Theme.textMuted)
            Text(preview.scopes.map(scopeName).joined(separator: " · "))
                .font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
            Text("Confirm this is the code shown on your Mac. Access will be remembered on this phone.")
                .font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
            Button(model.phase == "confirm" ? "Pair Mac" :
                ["verifying", "redeeming", "saving", "authenticating"].contains(model.phase)
                    ? "Connecting…" : "Create a fresh code") { model.confirm() }
                .buttonStyle(.borderedProminent).disabled(model.phase != "confirm")
                .accessibilityIdentifier("remote.confirm")
            Button("Cancel") { model.cancelEnrollment(); code = ""; entering = true }
        }.padding(16).background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: 16))
    }
    /// Which owned core this build linked (IOS-00): support and build evidence.
    private static let coreLine: String = {
        let core = coreIdentity()
        return "LUNA core \(core.coreVersion) · SDK \(core.sdkVersion) · API \(core.apiVersion)"
    }()
    private func scopeName(_ scope: String) -> String {
        switch scope { case "environment-read": "Host information"; case "tasks-read": "Read tasks"
        case "files-read": "Read files"; case "terminal-operate": "Terminal access"; default: "Unsupported access" }
    }
}

struct PairingScanner: UIViewControllerRepresentable {
    let scanned: (String) -> Void
    func makeUIViewController(context: Context) -> CameraScanner {
        CameraScanner(scanned: scanned)
    }
    func updateUIViewController(_ uiViewController: CameraScanner, context: Context) {}
}
final class CameraScanner: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    private let capture = AVCaptureSession()
    private let scanned: (String) -> Void
    private var didScan = false
    private var disappeared = false
    private let captureQueue = DispatchQueue(label: "luna.pairing-camera")
    init(scanned: @escaping (String) -> Void) { self.scanned = scanned; super.init(nibName: nil, bundle: nil) }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        AVCaptureDevice.requestAccess(for: .video) { [weak self] allowed in
            DispatchQueue.main.async {
                guard let self, !self.disappeared else { return }
                if allowed { self.start() } else { self.unavailable("Camera access is off. Close this sheet and enter the pairing code manually.") }
            }
        }
    }
    private func start() {
        guard let camera = AVCaptureDevice.default(for: .video), let input = try? AVCaptureDeviceInput(device: camera),
              capture.canAddInput(input) else { unavailable("Camera is unavailable. Enter the pairing code manually."); return }
        capture.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard capture.canAddOutput(output) else { unavailable("Scanner is unavailable. Enter the pairing code manually."); return }
        capture.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        let preview = AVCaptureVideoPreviewLayer(session: capture)
        preview.videoGravity = .resizeAspectFill; preview.frame = view.bounds
        view.layer.addSublayer(preview)
        captureQueue.async { [capture] in capture.startRunning() }
    }
    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        disappeared = true
        captureQueue.async { [capture] in capture.stopRunning() }
    }
    private func unavailable(_ text: String) {
        let label = UILabel(); label.text = text; label.textColor = .white; label.numberOfLines = 0
        label.textAlignment = .center; label.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(label)
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            label.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            label.centerYAnchor.constraint(equalTo: view.centerYAnchor),
        ])
    }
    func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput metadataObjects: [AVMetadataObject], from connection: AVCaptureConnection) {
        guard !didScan, let code = (metadataObjects.first as? AVMetadataMachineReadableCodeObject)?.stringValue,
              code.utf8.count <= 8192 else { return }
        didScan = true; scanned(code)
    }
}
