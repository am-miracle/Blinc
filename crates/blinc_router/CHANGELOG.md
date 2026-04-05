# Changelog

All notable changes to `blinc_router` will be documented in this file.

## [0.4.0] - 2026-04-05

### Added
- Route definition with trie matching: `/users/:id`, `*wildcard`, nested routes
- Scoped `use_router()` hook via thread-local router stack
- `RouterHistory` with push/replace/back/forward
- `PageTransition` using `AnimationPreset` + `SpringConfig`
- Navigation guards with redirect/reject
- Deep linking: auto-registered URI parsing + platform dispatch (desktop, iOS, Android)
- Named routing with reverse lookup: `push_named("user", &[("id", "42")])`
- Route outlet: `router.outlet()` builds current view with scoped context
- Stack/tab/bottom-sheet navigator patterns (documented integration)
- Animation suspension: old views auto-pause via `enter_scope`/`exit_scope`
- Nested route stacks via sub-routers
- System back button auto-registered by `RouterBuilder::build()`
