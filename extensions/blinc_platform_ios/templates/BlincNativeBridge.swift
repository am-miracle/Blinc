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

/// `UITextField` subclass that overrides `deleteBackward()` so
/// the delegate is informed of backspace presses *even when the
/// field is empty*.
///
/// ## Why this is necessary
///
/// `BlincKeyboardHelper` clears the hidden text field on every
/// change (`textField.text = ""`) so that the field's own buffer
/// never accumulates — the source of truth is the Rust
/// `text_input` widget. But that means the field is *always*
/// empty, and iOS does NOT call
/// `shouldChangeCharactersIn:replacementString:` when the user
/// presses backspace on an empty field. Backspace presses get
/// silently dropped.
///
/// The standard iOS workaround: subclass `UITextField` and
/// override `deleteBackward()`. This method is called for every
/// backspace press regardless of buffer state, and we forward
/// the event to the delegate via a custom protocol so
/// `BlincKeyboardHelper` can dispatch `blinc_ios_handle_key_down(ctx, 8)`.
class BlincHiddenTextField: UITextField {
    weak var blincDelegate: BlincKeyboardHelper?

    override func deleteBackward() {
        // Forward to the Blinc helper *first*, then call super
        // so the field's own (empty) buffer behavior is
        // preserved. Calling super on an empty field is a no-op,
        // so the order is mostly cosmetic.
        blincDelegate?.didPressBackspace()
        super.deleteBackward()
    }
}

/// Helper class that uses a hidden `BlincHiddenTextField` to
/// trigger the iOS soft keyboard and forward keystrokes back
/// into the Rust runtime.
///
/// iOS requires a `UITextInput` responder to show the keyboard —
/// there's no standalone API like Android's `InputMethodManager`.
///
/// ## Wiring text input back to Rust
///
/// `BlincViewController` (or any code that owns the
/// `IOSRenderContext`) MUST set `BlincKeyboardHelper.blincContext`
/// after creating the context, e.g.:
///
/// ```swift
/// // After: let ctx = blinc_create_context(...)
/// BlincKeyboardHelper.blincContext = ctx
/// ```
///
/// Without that, the keyboard pops up but every typed character
/// is silently dropped — the delegate has no context pointer to
/// forward into.
class BlincKeyboardHelper: NSObject, UITextFieldDelegate {
    static let shared = BlincKeyboardHelper()

    /// Active Blinc render context. Set by `BlincViewController`
    /// (or whatever owns the context) after `blinc_create_context`
    /// returns. Read by the delegate methods to forward typed
    /// characters into the Rust runtime.
    static var blincContext: OpaquePointer? = nil

    private var hiddenTextField: BlincHiddenTextField?

    private override init() {
        super.init()

        // Subscribe to keyboard frame notifications. We use
        // `WillChangeFrame` rather than `WillShow` / `WillHide`
        // because the former fires for every transition, including
        // hardware-keyboard attach (which collapses the soft
        // keyboard to a small inline accessory bar — height drops
        // but isn't zero), interactive dismissal (the user dragging
        // the keyboard down), and split-keyboard / floating-keyboard
        // mode changes on iPad. `WillShow`/`WillHide` miss those.
        //
        // The notification's `userInfo` contains:
        //   * `UIKeyboardFrameEndUserInfoKey`   — final frame in
        //     SCREEN coordinates as `NSValue<CGRect>`. We compute the
        //     intersection with the current key window's bounds to
        //     get the actually-obscured area (the keyboard frame can
        //     extend below the bottom of the screen during the
        //     animation), then take the height in POINTS (which is
        //     UIKit's logical-pixel unit — same coordinate space the
        //     Rust runner stores in `WindowedContext.width/height`).
        //   * `UIKeyboardAnimationDurationUserInfoKey` /
        //     `UIKeyboardAnimationCurveUserInfoKey` — duration and
        //     timing curve of the system animation. We don't push
        //     these into Rust right now (the runner just snaps to
        //     the new inset on the next frame), but they're worth
        //     a future hook for matching the system curve when
        //     we animate the scroll-into-view.
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleKeyboardFrameChange(_:)),
            name: UIResponder.keyboardWillChangeFrameNotification,
            object: nil
        )
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleKeyboardFrameChange(_:)),
            name: UIResponder.keyboardWillHideNotification,
            object: nil
        )
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    /// Compute the inset (height of the screen the keyboard is
    /// covering) and forward to Rust via the new FFI export.
    /// `keyboardWillHide` is wired to the same handler — UIKit
    /// posts a final frame at the bottom of the screen for the
    /// hide path, so the intersection-with-window-bounds math
    /// naturally produces zero in that case.
    @objc private func handleKeyboardFrameChange(_ notification: Notification) {
        guard let userInfo = notification.userInfo else { return }
        guard let endFrameValue = userInfo[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue else { return }
        let keyboardFrameInScreen = endFrameValue.cgRectValue

        // Find the active key window so we can convert from screen
        // coordinates and compute the intersection with the visible
        // area. On iOS 13+ we go through `connectedScenes`; the
        // top-most foreground active scene's first key window is the
        // one our `BlincViewController` lives in.
        let keyWindow: UIWindow? = UIApplication.shared
            .connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .filter { $0.activationState == .foregroundActive }
            .flatMap { $0.windows }
            .first { $0.isKeyWindow }

        guard let window = keyWindow else { return }

        let keyboardFrameInWindow = window.convert(keyboardFrameInScreen, from: nil)
        let intersection = keyboardFrameInWindow.intersection(window.bounds)

        // `intersection.height` is in points (UIKit logical units).
        // The Rust runner already stores logical pixels in
        // `WindowedContext.width/height`, so this maps 1:1 with no
        // DPI conversion needed.
        let insetPoints = intersection.isNull ? 0.0 : Double(intersection.height)

        if let ctx = BlincKeyboardHelper.blincContext {
            blinc_ios_set_keyboard_inset(ctx, Float(insetPoints))
        }
    }

    func showKeyboard() {
        if hiddenTextField == nil {
            let tf = BlincHiddenTextField(frame: CGRect(x: -1000, y: -1000, width: 1, height: 1))
            tf.autocorrectionType = .no
            tf.autocapitalizationType = .none
            tf.spellCheckingType = .no
            tf.delegate = self
            tf.blincDelegate = self
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

    /// Called by `BlincHiddenTextField.deleteBackward` when the
    /// user presses backspace. Forwards as virtual key code 8 so
    /// the Rust `text_input` widget's `on_key_down` handler runs
    /// `delete_backward()` (matches the desktop runner's table).
    func didPressBackspace() {
        if let ctx = BlincKeyboardHelper.blincContext {
            blinc_ios_handle_key_down(ctx, 8)
        }
    }

    /// Forward typed characters to the Rust text-input widget.
    ///
    /// The hidden `UITextField` is purely a keyboard host — its
    /// own buffer is irrelevant and we clear it on every change
    /// to prevent accumulation. The actual character dispatch
    /// happens via `blinc_ios_handle_text_input`, which
    /// broadcasts the event through the render tree to whichever
    /// Blinc text-input widget is currently focused.
    ///
    /// Backspace on an EMPTY field is NOT delivered through this
    /// delegate — see `BlincHiddenTextField.deleteBackward`.
    /// Backspace WHILE the field has content (rare, since we
    /// keep it empty) lands here with `range.length > 0,
    /// string.isEmpty` and we forward it via the same
    /// `didPressBackspace` path.
    ///
    /// Returning `false` tells UITextField NOT to apply the
    /// replacement to its buffer (we don't want it accumulating
    /// state). The `textField.text = ""` clear is belt-and-
    /// suspenders for autocorrect / dictation paths that might
    /// stuff text in despite the `false` return.
    func textField(_ textField: UITextField, shouldChangeCharactersIn range: NSRange, replacementString string: String) -> Bool {
        if let ctx = BlincKeyboardHelper.blincContext {
            if range.length > 0 && string.isEmpty {
                // Backspace path with non-empty field. Same
                // forwarding as `deleteBackward`.
                blinc_ios_handle_key_down(ctx, 8)
            } else if !string.isEmpty {
                // Normal character insert (or autocorrect /
                // dictation multi-char insert).
                //
                // The bridging header declares both FFI functions
                // as `IOSRenderContext* _Nonnull ctx`, which
                // Swift bridges as `OpaquePointer` (the opaque-
                // struct C type maps to `OpaquePointer`, NOT
                // `UnsafeMutablePointer<T>`). Pass `ctx` directly
                // without wrapping.
                string.withCString { ptr in
                    blinc_ios_handle_text_input(ctx, ptr)
                }
            }
        }

        // Always clear the hidden field — Blinc owns the text
        // buffer, the UITextField is just the keyboard host.
        textField.text = ""
        return false
    }

    /// Return-key handler. Forwards as virtual key code 13
    /// (matches the desktop runner's table for Enter).
    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        if let ctx = BlincKeyboardHelper.blincContext {
            blinc_ios_handle_key_down(ctx, 13)
        }
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
