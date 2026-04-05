//! Cross-platform routing for Blinc
//!
//! Declarative routing with path matching, navigation history,
//! guards, and page stack management.
//!
//! # Example
//!
//! ```ignore
//! use blinc_router::{Router, Route};
//!
//! let router = Router::new()
//!     .route(Route::new("/").name("home").view(home_page))
//!     .route(Route::new("/users").name("users").view(users_page)
//!         .child(Route::new("/:id").view(user_detail)))
//!     .not_found(not_found_page)
//!     .build();
//!
//! // In your UI:
//! let router = use_router();
//! router.push("/users/42");
//! router.back();
//! ```

pub mod history;
pub mod route;

use std::sync::{Arc, Mutex, OnceLock};

use blinc_core::context_state::BlincContextState;
use blinc_core::reactive::State;
use history::{HistoryEntry, RouterHistory};
use route::{MatchedRoute, QueryParams, Route, RouteContext, RouteParams, RouteTrie};

/// Route view function: receives context, returns a Div
pub type RouteView = fn(RouteContext) -> blinc_layout::div::Div;

/// Navigation guard: receives (from, to) and returns whether to allow
pub type NavigationGuard = Arc<dyn Fn(&HistoryEntry, &MatchedRoute) -> GuardResult + Send + Sync>;

/// Result of a navigation guard check
pub enum GuardResult {
    /// Allow the navigation
    Allow,
    /// Redirect to a different path
    Redirect(String),
    /// Block the navigation with a reason
    Reject(String),
}

/// Shared router state
struct RouterState {
    trie: RouteTrie,
    views: Vec<RouteView>,
    guards: Vec<NavigationGuard>,
    history: RouterHistory,
    /// Current matched route (drives UI updates via signal)
    current_match: Option<MatchedRoute>,
}

/// Global router singleton
static ROUTER: OnceLock<Arc<Mutex<RouterState>>> = OnceLock::new();

/// Signal that fires when the route changes
static ROUTE_SIGNAL: OnceLock<State<String>> = OnceLock::new();

/// Router builder
pub struct Router {
    routes: Vec<Route>,
    not_found: Option<RouteView>,
    guards: Vec<NavigationGuard>,
    initial_path: String,
}

impl Router {
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            not_found: None,
            guards: Vec::new(),
            initial_path: "/".to_string(),
        }
    }

    /// Add a route
    pub fn route(mut self, route: Route) -> Self {
        self.routes.push(route);
        self
    }

    /// Set the not-found page
    pub fn not_found(mut self, view: RouteView) -> Self {
        self.not_found = Some(view);
        self
    }

    /// Add a global navigation guard
    pub fn guard(mut self, guard: NavigationGuard) -> Self {
        self.guards.push(guard);
        self
    }

    /// Set initial path (default: "/")
    pub fn initial(mut self, path: impl Into<String>) -> Self {
        self.initial_path = path.into();
        self
    }

    /// Build the router and install it globally
    pub fn build(self) -> RouterHandle {
        let mut trie = RouteTrie::new();
        let mut views: Vec<RouteView> = Vec::new();

        // Register routes recursively
        fn register_routes(
            trie: &mut RouteTrie,
            views: &mut Vec<RouteView>,
            routes: &[Route],
            prefix: &str,
        ) {
            for route in routes {
                let full_path = if prefix == "/" {
                    route.path.clone()
                } else {
                    format!("{}{}", prefix, route.path)
                };

                if let Some(view) = route.view {
                    let idx = views.len();
                    views.push(view);
                    trie.add(&full_path, idx, route.name.as_deref());
                }

                if !route.children.is_empty() {
                    register_routes(trie, views, &route.children, &full_path);
                }
            }
        }

        register_routes(&mut trie, &mut views, &self.routes, "/");

        if let Some(nf_view) = self.not_found {
            let idx = views.len();
            views.push(nf_view);
            trie.set_not_found(idx);
        }

        // Match initial path
        let initial_match = trie.match_path(&self.initial_path);

        let state = Arc::new(Mutex::new(RouterState {
            trie,
            views,
            guards: self.guards,
            history: RouterHistory::new(&self.initial_path),
            current_match: initial_match,
        }));

        let _ = ROUTER.set(Arc::clone(&state));

        // Create route signal for reactive updates
        let ctx = BlincContextState::get();
        let signal = ctx.use_state_keyed("__blinc_router_path", || self.initial_path.clone());
        let _ = ROUTE_SIGNAL.set(signal);

        RouterHandle { state }
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle for programmatic navigation
#[derive(Clone)]
pub struct RouterHandle {
    state: Arc<Mutex<RouterState>>,
}

impl RouterHandle {
    /// Navigate to a path
    pub fn push(&self, path: impl Into<String>) {
        let path = path.into();
        let mut state = self.state.lock().unwrap();

        // Match the route
        let matched = state.trie.match_path(&path);

        // Run guards
        if let Some(ref m) = matched {
            for guard in &state.guards {
                match guard(&state.history.current, m) {
                    GuardResult::Allow => {}
                    GuardResult::Redirect(redirect_path) => {
                        drop(state);
                        self.push(redirect_path);
                        return;
                    }
                    GuardResult::Reject(reason) => {
                        tracing::warn!("Navigation to '{}' rejected: {}", path, reason);
                        return;
                    }
                }
            }
        }

        // Update history
        let entry = HistoryEntry {
            path: path.clone(),
            params: matched
                .as_ref()
                .map(|m| m.params.clone())
                .unwrap_or_default(),
            query: matched
                .as_ref()
                .map(|m| m.query.clone())
                .unwrap_or_default(),
            title: None,
        };
        state.history.push(entry);
        state.current_match = matched;

        // Fire route signal
        if let Some(signal) = ROUTE_SIGNAL.get() {
            signal.set(path);
        }
    }

    /// Replace current route (no history entry)
    pub fn replace(&self, path: impl Into<String>) {
        let path = path.into();
        let mut state = self.state.lock().unwrap();
        let matched = state.trie.match_path(&path);

        let entry = HistoryEntry {
            path: path.clone(),
            params: matched
                .as_ref()
                .map(|m| m.params.clone())
                .unwrap_or_default(),
            query: matched
                .as_ref()
                .map(|m| m.query.clone())
                .unwrap_or_default(),
            title: None,
        };
        state.history.replace(entry);
        state.current_match = matched;

        if let Some(signal) = ROUTE_SIGNAL.get() {
            signal.set(path);
        }
    }

    /// Go back
    pub fn back(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state.history.back() {
            let path = entry.path.clone();
            state.current_match = state.trie.match_path(&path);
            if let Some(signal) = ROUTE_SIGNAL.get() {
                signal.set(path);
            }
        }
    }

    /// Go forward
    pub fn forward(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state.history.forward() {
            let path = entry.path.clone();
            state.current_match = state.trie.match_path(&path);
            if let Some(signal) = ROUTE_SIGNAL.get() {
                signal.set(path);
            }
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.state.lock().unwrap().history.can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.state.lock().unwrap().history.can_go_forward()
    }

    /// Get the current path
    pub fn current_path(&self) -> String {
        self.state.lock().unwrap().history.current.path.clone()
    }

    /// Get the current matched route
    pub fn current_route(&self) -> Option<MatchedRoute> {
        self.state.lock().unwrap().current_match.clone()
    }

    /// Build the current route's view
    pub fn build_current_view(&self) -> Option<blinc_layout::div::Div> {
        let state = self.state.lock().unwrap();
        if let Some(ref matched) = state.current_match {
            let view = state.views.get(matched.view_index)?;
            let ctx = RouteContext {
                params: matched.params.clone(),
                query: matched.query.clone(),
                path: matched.path.clone(),
                router: self.clone(),
            };
            Some(view(ctx))
        } else {
            None
        }
    }
}

/// Get the global router handle (panics if router not initialized)
pub fn use_router() -> RouterHandle {
    RouterHandle {
        state: Arc::clone(
            ROUTER
                .get()
                .expect("Router not initialized. Call Router::new().build() first."),
        ),
    }
}

/// Get the current route's parameters
pub fn use_params() -> RouteParams {
    use_router()
        .current_route()
        .map(|r| r.params)
        .unwrap_or_default()
}

/// Get the current route's query parameters
pub fn use_query() -> QueryParams {
    use_router()
        .current_route()
        .map(|r| r.query)
        .unwrap_or_default()
}

/// Get the route signal for reactive dependency tracking.
/// Use with `Stateful::deps()` to rebuild when route changes.
pub fn route_signal_id() -> Option<blinc_core::reactive::SignalId> {
    ROUTE_SIGNAL.get().map(|s| s.signal_id())
}

/// Build a route outlet — renders the current route's view.
///
/// Place this in your layout where route content should appear.
/// It rebuilds when the route signal changes.
pub fn route_outlet() -> blinc_layout::div::Div {
    let router = use_router();
    router
        .build_current_view()
        .unwrap_or_else(blinc_layout::div::div)
}
