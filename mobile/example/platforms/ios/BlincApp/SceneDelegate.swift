import UIKit

// Scene-based lifecycle adopter.
//
// Starting with iOS 13, Apple split app lifecycle between the
// UIApplicationDelegate (process-level events) and UISceneDelegate
// (per-window events). Apps that don't adopt scenes get a runtime
// warning and, in a future iOS release, a hard assertion at launch:
//
//     `UIScene` lifecycle will soon be required. Failure to adopt
//     will result in an assert in the future.
//
// The fix is to declare a scene configuration in Info.plist under
// `UIApplicationSceneManifest.UISceneConfigurations` and point it at
// a class conforming to `UIWindowSceneDelegate`. This class is that
// delegate — it owns the `UIWindow` and installs the Blinc root
// view controller.
//
// Process-level setup (native bridge wiring, font registration,
// anything that needs to run exactly once per launch) stays in
// `AppDelegate.application(_:didFinishLaunchingWithOptions:)`.
@available(iOS 13.0, *)
class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = scene as? UIWindowScene else { return }

        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = BlincViewController()
        window.makeKeyAndVisible()
        self.window = window
    }
}
