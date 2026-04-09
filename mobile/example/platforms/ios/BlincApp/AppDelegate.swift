import UIKit

@main
class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        // Wire the Swift `BlincNativeBridge` into Rust BEFORE creating
        // the render context so any startup code that calls
        // `native_call(...)` (haptics, edit menu, clipboard, etc.)
        // resolves to the registered Swift handlers instead of
        // falling through with `NotRegistered`. Without this:
        //
        //   * Haptic feedback during touch input is silently dropped
        //   * The native edit menu (Cut / Copy / Paste) never appears
        //   * The Android clipboard fix that routes through
        //     `clipboard.copy` / `clipboard.paste` has no effect on
        //     iOS either (iOS uses arboard directly so it works,
        //     but parity is still nice)
        //
        // `connectToRust()` calls `blinc_set_native_call_fn(blinc_ios_native_call)`
        // which registers the Swift dispatch function with the Rust
        // bridge. After that, every Rust `native_call(...)` routes
        // through `BlincNativeBridge.shared.callNative(...)` and the
        // matching Swift handlers fire.
        BlincNativeBridge.shared.connectToRust()

        window = UIWindow(frame: UIScreen.main.bounds)
        window?.rootViewController = BlincViewController()
        window?.makeKeyAndVisible()
        return true
    }
}
