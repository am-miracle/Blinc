# Routing & Navigation

The `blinc_router` crate provides cross-platform routing with path matching, navigation history, guards, page transitions, and deep linking.

## Setup

```rust
use blinc_router::{RouterBuilder, Route, PageTransition};

let router = RouterBuilder::new()
    .route(Route::new("/").name("home").view(home_page))
    .route(Route::new("/users").name("users").view(users_page)
        .child(Route::new("/:id").name("user").view(user_detail)))
    .route(Route::new("/settings")
        .view(settings_page)
        .transition(PageTransition::modal()))
    .not_found(not_found_page)
    .build();
```

## Navigation

```rust
// Push (adds to history)
router.push("/users/42");

// Named route with params
router.push_named("user", &[("id", "42")]);

// Replace (no history entry)
router.replace("/login");

// Back / Forward
router.back();
router.forward();

// Check state
router.can_go_back();
router.current_path();
router.params().get("id");
router.query().get("page");
```

## Route Outlet

Place `router.outlet()` where the page content should render:

```rust
fn build_ui(ctx: &WindowedContext) -> impl ElementBuilder {
    div().flex_col()
        .child(nav_bar(&router))
        .child(router.outlet()) // Current route renders here
}
```

## use_router() Hook

Inside route views, `use_router()` returns the active router:

```rust
fn user_detail(ctx: RouteContext) -> Div {
    let router = use_router(); // Same as ctx.router
    let id = ctx.params.get("id").unwrap_or("?");

    div()
        .child(text(&format!("User #{}", id)))
        .child(
            div().on_click(move |_| router.back())
                .child(text("Back"))
        )
}
```

Nested routers work automatically — `use_router()` returns whichever router's `outlet()` is currently building.

## Page Transitions

Per-route transitions using Blinc's animation system:

```rust
Route::new("/settings")
    .view(settings_page)
    .transition(PageTransition::slide())      // iOS push style
    .transition(PageTransition::fade())       // Crossfade
    .transition(PageTransition::modal())      // Slide up/down
    .transition(PageTransition::scale())      // Scale in/out
    .transition(PageTransition::none())       // Instant

// Custom with spring physics
    .transition(PageTransition::slide().with_spring(SpringConfig::bouncy()))
```

## Navigation Guards

Protect routes with guards that allow, redirect, or reject:

```rust
use blinc_router::{NavigationGuard, GuardResult};
use std::sync::Arc;

let auth_guard: NavigationGuard = Arc::new(|_from, _to| {
    if is_authenticated() {
        GuardResult::Allow
    } else {
        GuardResult::Redirect("/login".into())
    }
});

RouterBuilder::new()
    .route(Route::new("/dashboard").view(dashboard).guard(auth_guard))
    .build();
```

## Deep Linking

### Desktop (CLI)

```rust
// Check for --deep-link=URI argument
if let Some(uri) = blinc_router::cli_deep_link() {
    router.handle_deep_link(&uri);
}
```

### Android

```rust
// In your app setup
blinc_app::android::set_deep_link_handler(move |uri| {
    router.handle_deep_link(uri);
});
```

Wire in Kotlin (BlincNativeBridge.kt):
```kotlin
// In onNewIntent or onCreate
intent?.data?.toString()?.let { uri ->
    // Call Rust via JNI
    dispatch_deep_link(uri)
}
```

### iOS

```rust
// In your app setup
blinc_app::ios::set_deep_link_handler(move |uri| {
    router.handle_deep_link(uri);
});
```

Wire in Swift (AppDelegate):
```swift
func application(_ app: UIApplication, open url: URL, options: ...) -> Bool {
    blinc_ios_handle_deep_link(url.absoluteString)
    return true
}
```

## System Back Button

Register the router as the back button handler (Android):

```rust
let _back_handle = router.register_back_handler();
// When back is pressed:
// - If router can go back → navigates back, event consumed
// - If at root → event propagates (app exits)
```

## Route Matching

Express-style path patterns:

| Pattern | Example | Matches |
|---------|---------|---------|
| Static | `/about` | Exact match |
| Parameter | `/users/:id` | `/users/42` → `{id: "42"}` |
| Wildcard | `/files/*path` | `/files/a/b/c` → `{path: "a/b/c"}` |
| Nested | parent + child | `/users` + `/:id` → `/users/42` |
| Query | any path | `/search?q=hello` → `{q: "hello"}` |

## Named Routes

Look up routes by name for type-safe navigation:

```rust
// Check if a named route exists
router.path_for("user"); // Some("/users/:id")

// Navigate with params
router.push_named("user", &[("id", "42")]); // → /users/42

// Check if a path matches
router.has_route("/users/42"); // true
```
