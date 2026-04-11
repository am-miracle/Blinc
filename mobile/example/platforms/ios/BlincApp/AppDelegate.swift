import UIKit

@main
class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        // Wire the Swift `BlincNativeBridge` into Rust BEFORE the
        // scene connects and creates the render context. Any startup
        // code that calls `native_call(...)` (haptics, edit menu,
        // clipboard, etc.) resolves to the registered Swift handlers
        // instead of falling through with `NotRegistered`. Without
        // this:
        //
        //   * Haptic feedback during touch input is silently dropped
        //   * The native edit menu (Cut / Copy / Paste) never appears
        //
        // `registerDefaults()` populates the Swift-side namespace
        // handler table (haptics, clipboard, edit_menu, device, app,
        // …). MUST be called before `connectToRust()` so the table
        // is populated before any Rust call could arrive — otherwise
        // the first `native_call("haptics", "selection", ())` from
        // a touch handler hits an empty table and the namespace
        // resolution fails silently.
        //
        // `connectToRust()` then calls
        // `blinc_set_native_call_fn(blinc_ios_native_call)`, which
        // registers Swift's dispatch function with the Rust bridge.
        // After that, every Rust `native_call(...)` routes through
        // `BlincNativeBridge.shared.callNative(...)` → the namespace
        // table → the matching Swift closure.
        BlincNativeBridge.shared.registerDefaults()
        BlincNativeBridge.shared.connectToRust()

        return true
    }

    // MARK: - UISceneSession lifecycle

    // Point the system at `SceneDelegate` for newly-connecting
    // scenes. Without this, iOS falls back to the legacy single-
    // window path (AppDelegate-owned `UIWindow`) and emits the
    // `UIScene lifecycle will soon be required` warning.
    @available(iOS 13.0, *)
    func application(
        _ application: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        let config = UISceneConfiguration(
            name: "Default Configuration",
            sessionRole: connectingSceneSession.role
        )
        config.delegateClass = SceneDelegate.self
        return config
    }
}
