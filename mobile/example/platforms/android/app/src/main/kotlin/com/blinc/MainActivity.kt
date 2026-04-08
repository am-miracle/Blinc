package com.blinc.example

import android.app.NativeActivity
import android.graphics.PixelFormat
import android.os.Bundle
import com.blinc.BlincNativeBridge

class MainActivity : NativeActivity() {
    companion object {
        init {
            System.loadLibrary("example")
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        // Register Blinc native bridge handlers BEFORE NativeActivity spawns
        // the android_main thread, so JNI calls from Rust can resolve
        // device/haptics/clipboard/app namespace handlers.
        BlincNativeBridge.registerDefaults(this)
        super.onCreate(savedInstanceState)

        // Force the window's pixel format to OPAQUE.
        //
        // NativeActivity surfaces default to TRANSLUCENT on modern
        // Android, which makes SurfaceFlinger composite the framebuffer
        // using its alpha channel. On the Pixel 10 Pro / Tensor G5
        // PowerVR Vulkan driver this combines with wgpu's `Inherit`
        // composite alpha mode (the only mode the driver exposes) to
        // produce an entirely invisible window. The authoritative fix
        // is the Rust-side `ANativeWindow_setBuffersGeometry` call in
        // `blinc_app::android::AndroidApp::init_gpu`, but the Java-
        // side `setFormat(OPAQUE)` here is a defensive belt-and-braces
        // measure: on devices where the NDK call doesn't fully take
        // hold (e.g. older Android versions, custom OEM compositors),
        // the JVM-side hint still gets the window pixel format set to
        // a non-translucent layout before the NDK code starts.
        window.setFormat(PixelFormat.OPAQUE)
    }
}
