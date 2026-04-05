/**
 * Blinc Native Bridge for iOS
 *
 * Swift implementation for handling native calls from Rust.
 * Register handlers for each namespace/function, then Rust can call
 * them via native_call("namespace", "function", args).
 *
 * Usage:
 * ```swift
 * // In AppDelegate.application(_:didFinishLaunchingWithOptions:)
 * BlincNativeBridge.shared.registerDefaults()
 * BlincNativeBridge.shared.connectToRust()
 *
 * // Or register custom handlers
 * BlincNativeBridge.shared.register(namespace: "myapi", name: "my_function") { args in
 *     // args is [Any]
 *     return "result"
 * }
 * ```
 */

import Foundation
import UIKit
import AudioToolbox
import AVFoundation

public final class BlincNativeBridge {

    public static let shared = BlincNativeBridge()

    // Handler type: (args: [Any]) throws -> Any?
    private var handlers: [String: [String: ([Any]) throws -> Any?]] = [:]

    private init() {}

    // MARK: - Registration

    /// Register a native function handler
    ///
    /// - Parameters:
    ///   - namespace: The namespace (e.g., "device", "haptics")
    ///   - name: The function name
    ///   - handler: Handler that receives args array and returns a result
    public func register(namespace: String, name: String, handler: @escaping ([Any]) throws -> Any?) {
        if handlers[namespace] == nil {
            handlers[namespace] = [:]
        }
        handlers[namespace]![name] = handler
    }

    /// Convenience: Register a no-arg function returning String
    public func registerString(namespace: String, name: String, handler: @escaping () -> String) {
        register(namespace: namespace, name: name) { _ in handler() }
    }

    /// Convenience: Register a no-arg void function
    public func registerVoid(namespace: String, name: String, handler: @escaping () -> Void) {
        register(namespace: namespace, name: name) { _ in handler(); return nil }
    }

    // MARK: - Native Call Handler

    /// Called from Rust via C FFI to execute a registered function
    ///
    /// - Parameters:
    ///   - namespace: The namespace
    ///   - name: The function name
    ///   - argsJson: JSON-encoded arguments array
    /// - Returns: JSON-encoded result or error
    func callNative(namespace: String, name: String, argsJson: String) -> String {
        do {
            guard let nsHandlers = handlers[namespace] else {
                return errorJson(type: "NotRegistered", message: "Namespace '\(namespace)' not found")
            }

            guard let handler = nsHandlers[name] else {
                return errorJson(type: "NotRegistered", message: "Function '\(namespace).\(name)' not found")
            }

            // Parse args from JSON
            let args = parseArgs(argsJson)

            // Call handler
            let result = try handler(args)

            return successJson(value: result)
        } catch {
            return errorJson(type: "PlatformError", message: error.localizedDescription)
        }
    }

    /// Connect to Rust by registering our native call function
    public func connectToRust() {
        blinc_set_native_call_fn(blinc_ios_native_call)
    }

    // MARK: - Default Handlers

    /// Register default handlers for common functionality
    public func registerDefaults() {

        // =====================================================================
        // Device namespace
        // =====================================================================

        registerString(namespace: "device", name: "get_battery_level") {
            UIDevice.current.isBatteryMonitoringEnabled = true
            let level = UIDevice.current.batteryLevel
            UIDevice.current.isBatteryMonitoringEnabled = false
            return level >= 0 ? String(Int(level * 100)) : "0"
        }

        registerString(namespace: "device", name: "get_model") {
            UIDevice.current.model
        }

        registerString(namespace: "device", name: "get_os_version") {
            UIDevice.current.systemVersion
        }

        register(namespace: "device", name: "is_low_power_mode") { _ in
            ProcessInfo.processInfo.isLowPowerModeEnabled
        }

        register(namespace: "device", name: "has_notch") { _ in
            if #available(iOS 11.0, *) {
                let window = UIApplication.shared.windows.first
                return (window?.safeAreaInsets.top ?? 0) > 20
            }
            return false
        }

        registerString(namespace: "device", name: "get_locale") {
            Locale.current.identifier
        }

        registerString(namespace: "device", name: "get_timezone") {
            TimeZone.current.identifier
        }

        // =====================================================================
        // Haptics namespace
        // =====================================================================

        register(namespace: "haptics", name: "vibrate") { args in
            // iOS doesn't support custom duration vibration via public API
            AudioServicesPlaySystemSound(kSystemSoundID_Vibrate)
            return nil
        }

        register(namespace: "haptics", name: "impact") { args in
            if #available(iOS 10.0, *) {
                let style: Int = args.first as? Int ?? 1
                let feedbackStyle: UIImpactFeedbackGenerator.FeedbackStyle
                switch style {
                case 0: feedbackStyle = .light
                case 2: feedbackStyle = .heavy
                default: feedbackStyle = .medium
                }
                let generator = UIImpactFeedbackGenerator(style: feedbackStyle)
                generator.prepare()
                generator.impactOccurred()
            }
            return nil
        }

        registerVoid(namespace: "haptics", name: "selection") {
            if #available(iOS 10.0, *) {
                let generator = UISelectionFeedbackGenerator()
                generator.prepare()
                generator.selectionChanged()
            }
        }

        registerVoid(namespace: "haptics", name: "success") {
            if #available(iOS 10.0, *) {
                let generator = UINotificationFeedbackGenerator()
                generator.prepare()
                generator.notificationOccurred(.success)
            }
        }

        registerVoid(namespace: "haptics", name: "warning") {
            if #available(iOS 10.0, *) {
                let generator = UINotificationFeedbackGenerator()
                generator.prepare()
                generator.notificationOccurred(.warning)
            }
        }

        registerVoid(namespace: "haptics", name: "error") {
            if #available(iOS 10.0, *) {
                let generator = UINotificationFeedbackGenerator()
                generator.prepare()
                generator.notificationOccurred(.error)
            }
        }

        // =====================================================================
        // Clipboard namespace
        // =====================================================================

        register(namespace: "clipboard", name: "copy") { args in
            let text = args.first as? String ?? ""
            UIPasteboard.general.string = text
            return nil
        }

        registerString(namespace: "clipboard", name: "paste") {
            UIPasteboard.general.string ?? ""
        }

        register(namespace: "clipboard", name: "has_content") { _ in
            UIPasteboard.general.hasStrings
        }

        registerVoid(namespace: "clipboard", name: "clear") {
            UIPasteboard.general.items = []
        }

        // =====================================================================
        // Camera namespace
        // =====================================================================

        register(namespace: "camera", name: "preview_start") { args in
            let width = args[safe: 0] as? Int ?? 640
            let height = args[safe: 1] as? Int ?? 480
            let fps = args[safe: 2] as? Int ?? 30
            let facing = args[safe: 3] as? Int ?? 0  // 0=front, 1=back
            let streamId = args[safe: 4] as? Int64 ?? 0

            BlincCameraHelper.shared.startPreview(
                width: width, height: height, fps: fps,
                facing: facing == 0 ? .front : .back,
                streamId: UInt64(streamId)
            )
            return nil
        }

        registerVoid(namespace: "camera", name: "preview_stop") {
            BlincCameraHelper.shared.stopPreview()
        }

        // =====================================================================
        // Audio recording namespace
        // =====================================================================

        register(namespace: "audio", name: "record_start") { args in
            let sampleRate = args[safe: 0] as? Int ?? 44100
            let channels = args[safe: 1] as? Int ?? 1
            let streamId = args[safe: 2] as? Int64 ?? 0

            BlincAudioRecorderHelper.shared.startRecording(
                sampleRate: sampleRate, channels: channels,
                streamId: UInt64(streamId)
            )
            return nil
        }

        registerVoid(namespace: "audio", name: "record_stop") {
            BlincAudioRecorderHelper.shared.stopRecording()
        }

        // =====================================================================
        // Keyboard namespace
        // =====================================================================

        register(namespace: "keyboard", name: "show") { _ in
            DispatchQueue.main.async {
                BlincKeyboardHelper.shared.showKeyboard()
            }
            return nil
        }

        register(namespace: "keyboard", name: "hide") { _ in
            DispatchQueue.main.async {
                BlincKeyboardHelper.shared.hideKeyboard()
            }
            return nil
        }

        // =====================================================================
        // App namespace
        // =====================================================================

        registerString(namespace: "app", name: "get_version") {
            Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0"
        }

        registerString(namespace: "app", name: "get_build_number") {
            Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "1"
        }

        registerString(namespace: "app", name: "get_bundle_id") {
            Bundle.main.bundleIdentifier ?? ""
        }

        register(namespace: "app", name: "open_url") { args in
            guard let urlString = args.first as? String,
                  let url = URL(string: urlString) else {
                return false
            }

            if #available(iOS 10.0, *) {
                UIApplication.shared.open(url, options: [:], completionHandler: nil)
                return true
            } else {
                return UIApplication.shared.openURL(url)
            }
        }

        register(namespace: "app", name: "share_text") { args in
            let text = args.first as? String ?? ""
            DispatchQueue.main.async {
                let activityVC = UIActivityViewController(activityItems: [text], applicationActivities: nil)
                if let windowScene = UIApplication.shared.connectedScenes.first as? UIWindowScene,
                   let rootVC = windowScene.windows.first?.rootViewController {
                    rootVC.present(activityVC, animated: true)
                }
            }
            return nil
        }
    }

    // MARK: - Helper Functions

    private func parseArgs(_ json: String) -> [Any] {
        guard let data = json.data(using: .utf8),
              let array = try? JSONSerialization.jsonObject(with: data) as? [Any] else {
            return []
        }
        return array
    }

    private func successJson(value: Any?) -> String {
        var result: [String: Any] = ["success": true]

        switch value {
        case nil:
            result["value"] = NSNull()
        case let bool as Bool:
            result["value"] = bool
        case let int as Int:
            result["value"] = int
        case let int64 as Int64:
            result["value"] = int64
        case let float as Float:
            result["value"] = float
        case let double as Double:
            result["value"] = double
        case let string as String:
            result["value"] = string
        case let data as Data:
            result["value"] = data.base64EncodedString()
        default:
            result["value"] = String(describing: value)
        }

        if let data = try? JSONSerialization.data(withJSONObject: result),
           let json = String(data: data, encoding: .utf8) {
            return json
        }
        return "{\"success\":true,\"value\":null}"
    }

    private func errorJson(type: String, message: String) -> String {
        let result: [String: Any] = [
            "success": false,
            "errorType": type,
            "errorMessage": message
        ]

        if let data = try? JSONSerialization.data(withJSONObject: result),
           let json = String(data: data, encoding: .utf8) {
            return json
        }
        return "{\"success\":false,\"errorType\":\"\(type)\",\"errorMessage\":\"\(message)\"}"
    }
}

// MARK: - Safe Array Access

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}

// MARK: - Camera Helper

/// Captures camera frames and sends RGBA data to Rust.
///
/// Uses AVCaptureSession + AVCaptureVideoDataOutput.
/// Each frame is converted to RGBA and sent via blinc_dispatch_stream_data.
class BlincCameraHelper: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
    static let shared = BlincCameraHelper()

    private var session: AVCaptureSession?
    private var streamId: UInt64 = 0
    private let queue = DispatchQueue(label: "blinc.camera")

    func startPreview(width: Int, height: Int, fps: Int, facing: AVCaptureDevice.Position, streamId: UInt64) {
        self.streamId = streamId

        let session = AVCaptureSession()
        session.sessionPreset = .medium

        guard let device = AVCaptureDevice.default(
            .builtInWideAngleCamera, for: .video, position: facing
        ) else { return }

        guard let input = try? AVCaptureDeviceInput(device: device) else { return }
        if session.canAddInput(input) { session.addInput(input) }

        let output = AVCaptureVideoDataOutput()
        output.videoSettings = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ]
        output.setSampleBufferDelegate(self, queue: queue)
        if session.canAddOutput(output) { session.addOutput(output) }

        session.startRunning()
        self.session = session
    }

    func stopPreview() {
        session?.stopRunning()
        session = nil
    }

    func captureOutput(_ output: AVCaptureOutput,
                       didOutput sampleBuffer: CMSampleBuffer,
                       from connection: AVCaptureConnection) {
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }

        CVPixelBufferLockBaseAddress(pixelBuffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, .readOnly) }

        let width = CVPixelBufferGetWidth(pixelBuffer)
        let height = CVPixelBufferGetHeight(pixelBuffer)
        let bytesPerRow = CVPixelBufferGetBytesPerRow(pixelBuffer)

        guard let baseAddress = CVPixelBufferGetBaseAddress(pixelBuffer) else { return }
        let ptr = baseAddress.assumingMemoryBound(to: UInt8.self)

        // Convert BGRA → RGBA
        var rgba = [UInt8](repeating: 0, count: width * height * 4)
        for y in 0..<height {
            for x in 0..<width {
                let srcIdx = y * bytesPerRow + x * 4
                let dstIdx = (y * width + x) * 4
                rgba[dstIdx + 0] = ptr[srcIdx + 2]  // R ← B
                rgba[dstIdx + 1] = ptr[srcIdx + 1]  // G
                rgba[dstIdx + 2] = ptr[srcIdx + 0]  // B ← R
                rgba[dstIdx + 3] = ptr[srcIdx + 3]  // A
            }
        }

        // Send to Rust
        rgba.withUnsafeBufferPointer { buf in
            blinc_dispatch_stream_data(streamId, buf.baseAddress!, UInt64(rgba.count))
        }
    }
}

// MARK: - Audio Recording Helper

/// Records audio from the microphone and sends PCM float samples to Rust.
class BlincAudioRecorderHelper {
    static let shared = BlincAudioRecorderHelper()

    private var audioEngine: AVAudioEngine?
    private var streamId: UInt64 = 0

    func startRecording(sampleRate: Int, channels: Int, streamId: UInt64) {
        self.streamId = streamId

        let engine = AVAudioEngine()
        let inputNode = engine.inputNode
        let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: Double(sampleRate),
            channels: AVAudioChannelCount(channels),
            interleaved: true
        )!

        inputNode.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, _ in
            guard let self = self else { return }
            guard let floatData = buffer.floatChannelData else { return }

            let frameCount = Int(buffer.frameLength)
            let channelCount = Int(buffer.format.channelCount)

            // Convert float samples to bytes (little-endian)
            var bytes = [UInt8](repeating: 0, count: frameCount * channelCount * 4)
            for i in 0..<(frameCount * channelCount) {
                let ch = i % channelCount
                let frame = i / channelCount
                let value = floatData[ch][frame]
                let valueBytes = withUnsafeBytes(of: value.bitPattern.littleEndian) { Array($0) }
                bytes[i * 4 + 0] = valueBytes[0]
                bytes[i * 4 + 1] = valueBytes[1]
                bytes[i * 4 + 2] = valueBytes[2]
                bytes[i * 4 + 3] = valueBytes[3]
            }

            bytes.withUnsafeBufferPointer { buf in
                blinc_dispatch_stream_data(self.streamId, buf.baseAddress!, UInt64(bytes.count))
            }
        }

        do {
            try engine.start()
            self.audioEngine = engine
        } catch {
            print("BlincAudioRecorder: failed to start: \(error)")
        }
    }

    func stopRecording() {
        audioEngine?.inputNode.removeTap(onBus: 0)
        audioEngine?.stop()
        audioEngine = nil
    }
}

// MARK: - Keyboard Helper

/// Helper class that uses a hidden UITextField to trigger the iOS soft keyboard.
/// iOS requires a UITextInput responder to show the keyboard — there's no
/// standalone API like Android's InputMethodManager.
class BlincKeyboardHelper: NSObject, UITextFieldDelegate {
    static let shared = BlincKeyboardHelper()

    private var hiddenTextField: UITextField?

    private override init() {
        super.init()
    }

    func showKeyboard() {
        if hiddenTextField == nil {
            let tf = UITextField(frame: CGRect(x: -1000, y: -1000, width: 1, height: 1))
            tf.autocorrectionType = .no
            tf.autocapitalizationType = .none
            tf.spellCheckingType = .no
            tf.delegate = self
            if let windowScene = UIApplication.shared.connectedScenes.first as? UIWindowScene,
               let window = windowScene.windows.first {
                window.addSubview(tf)
            }
            hiddenTextField = tf
        }
        hiddenTextField?.becomeFirstResponder()
    }

    func hideKeyboard() {
        hiddenTextField?.resignFirstResponder()
    }

    // Forward text input events to Blinc's event system
    func textField(_ textField: UITextField, shouldChangeCharactersIn range: NSRange, replacementString string: String) -> Bool {
        // The actual text input is handled by Blinc's event system,
        // not by UITextField. We just need the keyboard visible.
        // Clear the hidden field to prevent accumulation.
        textField.text = ""
        return false
    }
}

// MARK: - C FFI Entry Point

/// C function called by Rust to execute native handlers
/// Returns a malloc'd string that Rust must free with blinc_free_string
@_cdecl("blinc_ios_native_call")
public func blinc_ios_native_call(
    ns: UnsafePointer<CChar>,
    name: UnsafePointer<CChar>,
    argsJson: UnsafePointer<CChar>
) -> UnsafeMutablePointer<CChar>? {
    let namespace = String(cString: ns)
    let funcName = String(cString: name)
    let args = String(cString: argsJson)

    let result = BlincNativeBridge.shared.callNative(
        namespace: namespace,
        name: funcName,
        argsJson: args
    )

    return strdup(result)
}

/// Free a string allocated by blinc_ios_native_call
@_cdecl("blinc_free_string")
public func blinc_free_string(ptr: UnsafeMutablePointer<CChar>?) {
    if let ptr = ptr {
        free(ptr)
    }
}

/// Show the soft keyboard (called from Rust frame loop)
@_cdecl("blinc_ios_show_keyboard")
public func blinc_ios_show_keyboard() {
    DispatchQueue.main.async {
        BlincKeyboardHelper.shared.showKeyboard()
    }
}

/// Hide the soft keyboard (called from Rust frame loop)
@_cdecl("blinc_ios_hide_keyboard")
public func blinc_ios_hide_keyboard() {
    DispatchQueue.main.async {
        BlincKeyboardHelper.shared.hideKeyboard()
    }
}
