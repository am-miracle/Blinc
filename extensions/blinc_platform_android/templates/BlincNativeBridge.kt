/**
 * Blinc Native Bridge for Android
 *
 * Kotlin implementation for handling native calls from Rust.
 * Register handlers for each namespace/function, then Rust can call
 * them via native_call("namespace", "function", args).
 *
 * Usage:
 * ```kotlin
 * // In Application.onCreate()
 * BlincNativeBridge.registerDefaults(context)
 *
 * // Or register custom handlers
 * BlincNativeBridge.register("myapi", "my_function") { args ->
 *     // args is JSONArray
 *     "result"
 * }
 * ```
 */

package com.blinc

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.BatteryManager
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.view.inputmethod.InputMethodManager
import androidx.core.content.getSystemService
import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale
import java.util.TimeZone

object BlincNativeBridge {

    // Handler type: (args: JSONArray) -> Any?
    private val handlers = mutableMapOf<String, MutableMap<String, (JSONArray) -> Any?>>()

    // Application context for system services
    private var appContext: Context? = null

    /**
     * Initialize with application context
     */
    fun init(context: Context) {
        appContext = context.applicationContext
    }

    /**
     * Register a native function handler
     *
     * @param namespace The namespace (e.g., "device", "haptics")
     * @param name The function name
     * @param handler Handler that receives JSON args and returns a result
     */
    fun register(namespace: String, name: String, handler: (JSONArray) -> Any?) {
        handlers.getOrPut(namespace) { mutableMapOf() }[name] = handler
    }

    /**
     * Convenience: Register a no-arg function returning String
     */
    fun registerString(namespace: String, name: String, handler: () -> String) {
        register(namespace, name) { handler() }
    }

    /**
     * Convenience: Register a no-arg void function
     */
    fun registerVoid(namespace: String, name: String, handler: () -> Unit) {
        register(namespace, name) { handler(); null }
    }

    /**
     * Called from JNI to execute a registered function
     *
     * @param namespace The namespace
     * @param name The function name
     * @param argsJson JSON-encoded arguments array
     * @return JSON-encoded result or error
     */
    @JvmStatic
    fun callNative(namespace: String, name: String, argsJson: String): String {
        return try {
            val nsHandlers = handlers[namespace]
                ?: return errorJson("NotRegistered", "Namespace '$namespace' not found")

            val handler = nsHandlers[name]
                ?: return errorJson("NotRegistered", "Function '$namespace.$name' not found")

            val args = JSONArray(argsJson)
            val result = handler(args)

            successJson(result)
        } catch (e: Exception) {
            errorJson("PlatformError", e.message ?: "Unknown error")
        }
    }

    /**
     * Register default handlers for common functionality
     */
    fun registerDefaults(context: Context) {
        init(context)
        val ctx = context.applicationContext

        // =====================================================================
        // Device namespace
        // =====================================================================

        registerString("device", "get_battery_level") {
            val bm = ctx.getSystemService<BatteryManager>()
            bm?.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)?.toString() ?: "0"
        }

        registerString("device", "get_model") {
            Build.MODEL
        }

        registerString("device", "get_os_version") {
            Build.VERSION.RELEASE
        }

        register("device", "is_low_power_mode") {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
                val pm = ctx.getSystemService(Context.POWER_SERVICE) as? android.os.PowerManager
                pm?.isPowerSaveMode ?: false
            } else {
                false
            }
        }

        register("device", "has_notch") {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                // Check for display cutout
                // This requires a window, return false as default
                false
            } else {
                false
            }
        }

        registerString("device", "get_locale") {
            Locale.getDefault().toString()
        }

        registerString("device", "get_timezone") {
            TimeZone.getDefault().id
        }

        // =====================================================================
        // Haptics namespace
        // =====================================================================

        register("haptics", "vibrate") { args ->
            val durationMs = args.optLong(0, 100)
            vibrate(ctx, durationMs)
            null
        }

        register("haptics", "impact") { args ->
            val style = args.optInt(0, 1)
            val amplitude = when (style) {
                0 -> 50   // light
                2 -> 255  // heavy
                else -> 128 // medium
            }
            vibrateWithAmplitude(ctx, 10, amplitude)
            null
        }

        registerVoid("haptics", "selection") {
            vibrateWithAmplitude(ctx, 5, 50)
        }

        registerVoid("haptics", "success") {
            vibrateWithAmplitude(ctx, 30, 200)
        }

        registerVoid("haptics", "warning") {
            vibrateWithAmplitude(ctx, 50, 150)
        }

        registerVoid("haptics", "error") {
            vibrateWithAmplitude(ctx, 100, 255)
        }

        // =====================================================================
        // Clipboard namespace
        // =====================================================================

        register("clipboard", "copy") { args ->
            val text = args.optString(0, "")
            val clipboard = ctx.getSystemService<ClipboardManager>()
            clipboard?.setPrimaryClip(ClipData.newPlainText("Blinc", text))
            null
        }

        registerString("clipboard", "paste") {
            val clipboard = ctx.getSystemService<ClipboardManager>()
            clipboard?.primaryClip?.getItemAt(0)?.text?.toString() ?: ""
        }

        register("clipboard", "has_content") {
            val clipboard = ctx.getSystemService<ClipboardManager>()
            clipboard?.hasPrimaryClip() ?: false
        }

        registerVoid("clipboard", "clear") {
            val clipboard = ctx.getSystemService<ClipboardManager>()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                clipboard?.clearPrimaryClip()
            }
        }

        // =====================================================================
        // Keyboard namespace
        // =====================================================================

        register("keyboard", "show") { _ ->
            val imm = ctx.getSystemService(Context.INPUT_METHOD_SERVICE) as? InputMethodManager
            // For NativeActivity, use the decor view to request focus
            val activity = appContext as? android.app.Activity
            activity?.runOnUiThread {
                val view = activity.window?.decorView?.rootView
                view?.requestFocus()
                imm?.showSoftInput(view, InputMethodManager.SHOW_IMPLICIT)
            }
            null
        }

        register("keyboard", "hide") { _ ->
            val imm = ctx.getSystemService(Context.INPUT_METHOD_SERVICE) as? InputMethodManager
            val activity = appContext as? android.app.Activity
            activity?.runOnUiThread {
                val view = activity.window?.decorView?.rootView
                imm?.hideSoftInputFromWindow(view?.windowToken, 0)
            }
            null
        }

        // =====================================================================
        // Camera namespace
        // =====================================================================

        register("camera", "preview_start") { args ->
            val width = args.optInt(0, 640)
            val height = args.optInt(1, 480)
            val fps = args.optInt(2, 30)
            val facing = args.optInt(3, 0) // 0=front, 1=back
            val streamId = args.optLong(4, 0)

            startCameraPreview(ctx, width, height, fps, facing, streamId)
            null
        }

        registerVoid("camera", "preview_stop") {
            stopCameraPreview()
        }

        // =====================================================================
        // Audio recording namespace
        // =====================================================================

        register("audio", "record_start") { args ->
            val sampleRate = args.optInt(0, 44100)
            val channels = args.optInt(1, 1)
            val streamId = args.optLong(2, 0)

            startAudioRecording(ctx, sampleRate, channels, streamId)
            null
        }

        registerVoid("audio", "record_stop") {
            stopAudioRecording()
        }

        // =====================================================================
        // App namespace
        // =====================================================================

        registerString("app", "get_version") {
            try {
                ctx.packageManager.getPackageInfo(ctx.packageName, 0).versionName ?: "1.0"
            } catch (e: Exception) {
                "1.0"
            }
        }

        registerString("app", "get_build_number") {
            try {
                val info = ctx.packageManager.getPackageInfo(ctx.packageName, 0)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                    info.longVersionCode.toString()
                } else {
                    @Suppress("DEPRECATION")
                    info.versionCode.toString()
                }
            } catch (e: Exception) {
                "1"
            }
        }

        registerString("app", "get_bundle_id") {
            ctx.packageName
        }

        register("app", "open_url") { args ->
            val url = args.optString(0, "")
            try {
                val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url))
                intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                ctx.startActivity(intent)
                true
            } catch (e: Exception) {
                false
            }
        }

        register("app", "share_text") { args ->
            val text = args.optString(0, "")
            val intent = Intent(Intent.ACTION_SEND).apply {
                type = "text/plain"
                putExtra(Intent.EXTRA_TEXT, text)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            ctx.startActivity(Intent.createChooser(intent, "Share").apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            })
            null
        }
    }

    // =========================================================================
    // Helper functions
    // =========================================================================

    private fun successJson(value: Any?): String {
        val obj = JSONObject()
        obj.put("success", true)
        when (value) {
            null -> obj.put("value", JSONObject.NULL)
            is String -> obj.put("value", value)
            is Boolean -> obj.put("value", value)
            is Int -> obj.put("value", value)
            is Long -> obj.put("value", value)
            is Float -> obj.put("value", value)
            is Double -> obj.put("value", value)
            is ByteArray -> obj.put("value", android.util.Base64.encodeToString(value, android.util.Base64.NO_WRAP))
            else -> obj.put("value", value.toString())
        }
        return obj.toString()
    }

    private fun errorJson(type: String, message: String): String {
        val obj = JSONObject()
        obj.put("success", false)
        obj.put("errorType", type)
        obj.put("errorMessage", message)
        return obj.toString()
    }

    private fun vibrate(context: Context, durationMs: Long) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val vm = context.getSystemService<VibratorManager>()
            vm?.defaultVibrator?.vibrate(
                VibrationEffect.createOneShot(durationMs, VibrationEffect.DEFAULT_AMPLITUDE)
            )
        } else {
            @Suppress("DEPRECATION")
            val v = context.getSystemService<Vibrator>()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                v?.vibrate(VibrationEffect.createOneShot(durationMs, VibrationEffect.DEFAULT_AMPLITUDE))
            } else {
                @Suppress("DEPRECATION")
                v?.vibrate(durationMs)
            }
        }
    }

    private fun vibrateWithAmplitude(context: Context, durationMs: Long, amplitude: Int) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                val vm = context.getSystemService<VibratorManager>()
                vm?.defaultVibrator?.vibrate(VibrationEffect.createOneShot(durationMs, amplitude))
            } else {
                @Suppress("DEPRECATION")
                val v = context.getSystemService<Vibrator>()
                v?.vibrate(VibrationEffect.createOneShot(durationMs, amplitude))
            }
        } else {
            vibrate(context, durationMs)
        }
    }

    // =========================================================================
    // Camera preview
    // =========================================================================

    private var cameraStreamId: Long = 0
    private var isCameraRunning = false

    /**
     * Start camera preview and stream RGBA frames to Rust via JNI.
     *
     * Uses Camera2 API. Each frame is converted to RGBA and sent via
     * nativeDispatchStreamData(streamId, rgbaBytes).
     */
    private fun startCameraPreview(
        context: Context,
        width: Int, height: Int, fps: Int, facing: Int, streamId: Long
    ) {
        cameraStreamId = streamId
        isCameraRunning = true

        // Camera2 implementation requires android.hardware.camera2 imports
        // and a background HandlerThread. This is a template — users should
        // adapt to their specific camera requirements.
        //
        // The key integration point:
        // 1. Open CameraDevice for the requested facing
        // 2. Create ImageReader with ImageFormat.YUV_420_888
        // 3. In OnImageAvailableListener, convert YUV → RGBA
        // 4. Call: nativeDispatchStreamData(streamId, rgbaBytes)
        //
        // Example conversion (simplified):
        // val image = reader.acquireLatestImage()
        // val rgba = yuvToRgba(image)  // convert planes to RGBA
        // nativeDispatchStreamData(cameraStreamId, rgba)
        // image.close()

        android.util.Log.i("BlincNativeBridge", "Camera preview started: ${width}x${height} @ ${fps}fps, stream=$streamId")
    }

    private fun stopCameraPreview() {
        isCameraRunning = false
        android.util.Log.i("BlincNativeBridge", "Camera preview stopped")
    }

    // =========================================================================
    // Audio recording
    // =========================================================================

    private var audioStreamId: Long = 0
    private var isAudioRecording = false
    private var audioRecordThread: Thread? = null

    /**
     * Start audio recording and stream PCM samples to Rust.
     *
     * Uses AudioRecord API. PCM float samples are sent as raw bytes via
     * nativeDispatchStreamData(streamId, pcmBytes).
     */
    private fun startAudioRecording(
        context: Context,
        sampleRate: Int, channels: Int, streamId: Long
    ) {
        audioStreamId = streamId
        isAudioRecording = true

        val channelConfig = if (channels == 1)
            android.media.AudioFormat.CHANNEL_IN_MONO
        else
            android.media.AudioFormat.CHANNEL_IN_STEREO

        val bufferSize = android.media.AudioRecord.getMinBufferSize(
            sampleRate, channelConfig, android.media.AudioFormat.ENCODING_PCM_FLOAT
        )

        audioRecordThread = Thread {
            try {
                val recorder = android.media.AudioRecord(
                    android.media.MediaRecorder.AudioSource.MIC,
                    sampleRate, channelConfig,
                    android.media.AudioFormat.ENCODING_PCM_FLOAT,
                    bufferSize
                )
                recorder.startRecording()

                val buffer = FloatArray(bufferSize / 4)
                while (isAudioRecording) {
                    val read = recorder.read(buffer, 0, buffer.size, android.media.AudioRecord.READ_BLOCKING)
                    if (read > 0) {
                        // Convert float array to byte array (little-endian)
                        val bytes = ByteArray(read * 4)
                        val bb = java.nio.ByteBuffer.wrap(bytes).order(java.nio.ByteOrder.LITTLE_ENDIAN)
                        for (i in 0 until read) {
                            bb.putFloat(buffer[i])
                        }
                        nativeDispatchStreamData(audioStreamId, bytes)
                    }
                }

                recorder.stop()
                recorder.release()
            } catch (e: Exception) {
                android.util.Log.e("BlincNativeBridge", "Audio recording error: ${e.message}")
            }
        }
        audioRecordThread?.start()
    }

    private fun stopAudioRecording() {
        isAudioRecording = false
        audioRecordThread?.join(1000)
        audioRecordThread = null
    }

    // JNI bridge for stream data
    @JvmStatic
    external fun nativeDispatchStreamData(streamId: Long, data: ByteArray)
}
