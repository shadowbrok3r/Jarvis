import AVFoundation
import SwiftUI
import UIKit

// MARK: - Backend (session queue; no MainActor)

/// Owns `AVCaptureSession` and mutates it only on `sessionQueue`.
private final class JarvisCameraSessionBackend: @unchecked Sendable {
    let captureSession = AVCaptureSession()
    private let sessionQueue = DispatchQueue(label: "ai.jarvis.JarvisCameraSession.session", qos: .userInitiated)
    private var isConfigured = false

    func startPreview(completion: @escaping @Sendable (Result<String, Error>) -> Void) {
        sessionQueue.async { [weak self] in
            guard let self else { return }
            do {
                try self.configureSessionIfNeededLocked()
                guard !self.captureSession.isRunning else {
                    completion(.success(self.activeCameraSummaryLocked()))
                    return
                }
                self.captureSession.startRunning()
                let summary = self.activeCameraSummaryLocked()
                completion(.success(summary))
            } catch {
                completion(.failure(error))
            }
        }
    }

    func stopPreview(completion: @escaping @Sendable () -> Void) {
        sessionQueue.async { [weak self] in
            guard let self else { return }
            if self.captureSession.isRunning {
                self.captureSession.stopRunning()
            }
            completion()
        }
    }

    func teardown(completion: @escaping @Sendable () -> Void) {
        sessionQueue.async { [weak self] in
            guard let self else { return }
            if self.captureSession.isRunning {
                self.captureSession.stopRunning()
            }
            self.captureSession.beginConfiguration()
            for input in self.captureSession.inputs {
                self.captureSession.removeInput(input)
            }
            self.captureSession.commitConfiguration()
            self.isConfigured = false
            completion()
        }
    }

    private func configureSessionIfNeededLocked() throws {
        guard !isConfigured else { return }
        captureSession.beginConfiguration()
        captureSession.sessionPreset = .medium

        let device = Self.preferredVideoDevice()
        guard let device else {
            captureSession.commitConfiguration()
            throw JarvisCameraError.noVideoDevice
        }

        let input = try AVCaptureDeviceInput(device: device)
        guard captureSession.canAddInput(input) else {
            captureSession.commitConfiguration()
            throw JarvisCameraError.cannotAddInput
        }
        captureSession.addInput(input)
        captureSession.commitConfiguration()
        isConfigured = true

        let position = device.position
        let posLabel = position == .front ? "Front" : (position == .back ? "Back" : "Unknown")
        JarvisIOSLog.recordCamera("configureSession: device=\(device.localizedName), position=\(posLabel)")
    }

    private func activeCameraSummaryLocked() -> String {
        guard let device = (captureSession.inputs.compactMap { $0 as? AVCaptureDeviceInput }.first?.device) else {
            return ""
        }
        let position = device.position
        let posLabel = position == .front ? "Front" : (position == .back ? "Back" : "Unknown")
        return "\(posLabel) (wide)"
    }

    private static func preferredVideoDevice() -> AVCaptureDevice? {
        let front = AVCaptureDevice.DiscoverySession(
            deviceTypes: [.builtInWideAngleCamera],
            mediaType: .video,
            position: .front
        )
        if let d = front.devices.first { return d }
        let back = AVCaptureDevice.DiscoverySession(
            deviceTypes: [.builtInWideAngleCamera],
            mediaType: .video,
            position: .back
        )
        return back.devices.first
    }
}

private enum JarvisCameraError: LocalizedError {
    case noVideoDevice
    case cannotAddInput

    var errorDescription: String? {
        switch self {
        case .noVideoDevice: "No video capture device available."
        case .cannotAddInput: "Cannot add video input to capture session."
        }
    }
}

// MARK: - Observable façade (MainActor)

/// Front camera preferred, wide-angle back fallback. Debug preview only; no frames sent to the hub.
@MainActor
@Observable
final class JarvisCameraSession {
    enum AuthorizationState: Equatable {
        case notDetermined
        case authorized
        case denied
        case restricted
    }

    private(set) var authorization: AuthorizationState = .notDetermined
    private(set) var isRunning = false
    private(set) var lastError: String?
    private(set) var activeCameraSummary: String = ""

    var captureSession: AVCaptureSession { backend.captureSession }

    /// True when `startPreview` may proceed (authorized only).
    var canStartPreview: Bool { authorization == .authorized }

    private let backend = JarvisCameraSessionBackend()

    init() {
        refreshAuthorizationFromSystem()
    }

    func refreshAuthorizationFromSystem() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .notDetermined: authorization = .notDetermined
        case .authorized: authorization = .authorized
        case .denied: authorization = .denied
        case .restricted: authorization = .restricted
        @unknown default: authorization = .notDetermined
        }
    }

    func requestVideoAccessIfNeeded() {
        refreshAuthorizationFromSystem()
        guard authorization == .notDetermined else { return }
        JarvisIOSLog.recordCamera("requestVideoAccess: prompting user")
        AVCaptureDevice.requestAccess(for: .video) { granted in
            Task { @MainActor in
                self.refreshAuthorizationFromSystem()
                JarvisIOSLog.recordCamera("requestVideoAccess: granted=\(granted)")
            }
        }
    }

    func startPreview() {
        lastError = nil
        refreshAuthorizationFromSystem()
        guard authorization == .authorized else {
            let msg = "Camera not authorized (status: \(authorization))."
            lastError = msg
            JarvisIOSLog.recordCameraError("startPreview: \(msg)")
            return
        }

        backend.startPreview { [weak self] result in
            Task { @MainActor in
                guard let self else { return }
                switch result {
                case .success(let summary):
                    self.activeCameraSummary = summary
                    self.isRunning = true
                    self.lastError = nil
                    JarvisIOSLog.recordCamera("startPreview ok (\(summary))")
                case .failure(let error):
                    self.isRunning = false
                    let msg = error.localizedDescription
                    self.lastError = msg
                    JarvisIOSLog.recordCameraError("startPreview: \(msg)")
                }
            }
        }
    }

    func stopPreview() {
        backend.stopPreview { [weak self] in
            Task { @MainActor in
                guard let self else { return }
                self.isRunning = false
                JarvisIOSLog.recordCamera("stopPreview")
            }
        }
    }

    func teardown() {
        backend.teardown { [weak self] in
            Task { @MainActor in
                guard let self else { return }
                self.isRunning = false
                self.activeCameraSummary = ""
                JarvisIOSLog.recordCamera("teardown: inputs removed")
            }
        }
    }
}

// MARK: - Preview layer (UIKit)

final class PreviewContainerView: UIView {
    override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }

    var previewLayer: AVCaptureVideoPreviewLayer {
        guard let pl = layer as? AVCaptureVideoPreviewLayer else {
            fatalError("JarvisCameraPreview: expected AVCaptureVideoPreviewLayer")
        }
        return pl
    }
}

/// SwiftUI host for `AVCaptureVideoPreviewLayer`; binds `session` from `JarvisCameraSession.captureSession`.
struct JarvisCameraPreviewRepresentable: UIViewRepresentable {
    let session: AVCaptureSession

    func makeUIView(context: Context) -> PreviewContainerView {
        let v = PreviewContainerView()
        v.previewLayer.session = session
        v.previewLayer.videoGravity = .resizeAspectFill
        v.backgroundColor = .black
        return v
    }

    func updateUIView(_ uiView: PreviewContainerView, context: Context) {
        if uiView.previewLayer.session !== session {
            uiView.previewLayer.session = session
        }
        uiView.previewLayer.videoGravity = .resizeAspectFill
    }
}
