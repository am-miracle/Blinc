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

Deep linking is **automatic** — just build a router and it works on all platforms.
`RouterBuilder::build()` auto-registers the deep link handler and back button.

```rust
// That's it — no platform-specific setup needed in Rust
let router = RouterBuilder::new()
    .route(Route::new("/users/:id").view(user_page))
    .build();
// Deep links to myapp://host/users/42 automatically navigate
```

### Platform Configuration

**Android** — add intent filters in `AndroidManifest.xml`:
```xml
<intent-filter>
    <action android:name="android.intent.action.VIEW" />
    <data android:scheme="myapp" />
</intent-filter>
```

**iOS** — add URL types in `Info.plist`:
```xml
<key>CFBundleURLTypes</key>
<array>
    <dict>
        <key>CFBundleURLSchemes</key>
        <array><string>myapp</string></array>
    </dict>
</array>
```

**Desktop** — register a custom URL scheme with the OS:

*macOS* (`Info.plist`):
```xml
<key>CFBundleURLTypes</key>
<array>
    <dict>
        <key>CFBundleURLSchemes</key>
        <array><string>myapp</string></array>
    </dict>
</array>
```

*Windows* (registry, set up by installer):
```
HKEY_CLASSES_ROOT\myapp\shell\open\command = "C:\path\to\myapp.exe" "--deep-link=%1"
```

*Linux* (`.desktop` file):
```ini
MimeType=x-scheme-handler/myapp
Exec=myapp --deep-link=%u
```

CLI fallback:
```bash
myapp --deep-link=myapp://host/users/42
```

### How It Works

1. `RouterBuilder::build()` registers a global deep link handler
2. Platform runners auto-dispatch incoming URIs to the handler
3. The router parses the URI and calls `push(path)`
4. No user code needed beyond building the router

## System Back Button

Also automatic — `RouterBuilder::build()` registers a back button handler.

- **Android**: system back button navigates back if the router has history
- **Desktop**: `Key::Back` dispatches through the back handler stack
- If at the root route, the event propagates (app exits normally)

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
