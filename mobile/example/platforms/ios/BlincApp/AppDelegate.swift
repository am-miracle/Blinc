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
        // `registerDefaults()` populates the Swift-side namespace
        // handler table (haptics, clipboard, edit_menu, device, app,
        // …). MUST be called before `connectToRust()` so the table
        // is populated before any Rust call could arrive — otherwise
        // the first `native_call("haptics", "selection", ())` from
        // a touch handler hits an empty table and the namespace
        // resolution fails silently.
        //
        // `connectToRust()` then calls `blinc_set_native_call_fn(blinc_ios_native_call)`
        // which registers the Swift dispatch function with the Rust
        // bridge. After that, every Rust `native_call(...)` routes
        // through `BlincNativeBridge.shared.callNative(...)` → the
        // namespace table → the matching Swift closure.
        BlincNativeBridge.shared.registerDefaults()
        BlincNativeBridge.shared.connectToRust()

        window = UIWindow(frame: UIScreen.main.bounds)
        window?.rootViewController = BlincViewController()
        window?.makeKeyAndVisible()
        return true
    }
}
