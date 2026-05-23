# Changelog

All notable changes to `blinc_cn` will be documented in this file.

## [Unreleased]

### Added
- **`cn::toggle`** — shadcn-style binary toggle button. Themed wrapper around `blinc_layout::widgets::toggle`, contributing `.cn-toggle` + `.cn-toggle--default` / `.cn-toggle--outline` variant classes and `.cn-toggle--sm` / `--md` / `--lg` size classes. All parse / state / token-default work lives in the layout widget — cn only supplies the surface CSS and the variant / size selection. `ToggleVariant::Default` (no border off) is the toolbar-friendly default; `ToggleVariant::Outline` borders the off state (pairs with future `cn::toggle_group`). `ToggleSize::{Small, Medium, Large}` heights line up with `cn::button` and `cn::input` so toggle rows sit flush.
- **HID focus ring on `.cn-input` / `.cn-textarea`**. On focus the border brightens to `--border-focus` and a 2 px outer ring at 2 px offset draws around the input edge using a 35 %-alpha tint (`--focus-ring`) so the ring reads as a soft halo distinct from the crisp border. Error and success variants get their own `--focus-ring-error` / `--focus-ring-success` colours. Outline scales from a transparent 1 px-offset baseline to the focus 2 px offset with a 160 ms ease for the first focus interaction (see Known Issues).
- **Semantic easings wired through dialog / sheet / drawer / toast and the CSS surface**. Enter / exit animations now use `--ease-default` (or the variant-specific role) instead of the previous fixed `EaseInOut`. The cn stylesheet picks up the same vars so user-written CSS transitions match the framework's built-in motion.
- **`ButtonSize::Custom(width, height)`**. Escape hatch for cases where the Small / Medium / Large / Icon ladder doesn't fit — wide auth-flow CTAs, tight inline actions, settings rows aligned to a specific column.

### Changed
- Dropdown menu / context menu / select / combobox / menubar / nav-menu hover highlights now clip to the panel's outer radius, so the highlight follows the panel's rounded edge instead of leaving a visible strip of the panel's surface bg at the corners.
- Combobox search input bumps `radius_sm` → `radius_md` to match the dropdown's corner reach, plus added horizontal padding so the input doesn't butt against the panel edge.
- Breadcrumb item labels + separators (slash / text) now `.no_wrap()` so long path segments don't fold across two lines mid-trail.
- `cn::label` collapses to `w_fit()` and the inner text is `.no_wrap()` so a long label doesn't take the full row width.
- `.cn-input` / `.cn-textarea` no longer redeclare `border:` at the base level — the layout `TextInput` setters supply idle / hover / focused border colours; the redundant base rule was being rewritten by `apply_complex_selector_styles` every frame and clobbering the setter-chosen focused colour.
- `apply_css_overrides` on text input / text area now applies `:focus` AFTER `:hover` for the `FocusedHovered` state so focus colour wins while the user is typing in a hovered input. Same ordering applied to the outline-extraction path.
- All component `.class()` builders take `impl AsRef<str>` (was `impl Into<String>`), and per-component `classes` / `css_classes` storage is now `Vec<Arc<str>>` interned through `blinc_core::intern`. A class repeated across hundreds of nodes now allocates exactly once.
- `element_classes()` overrides return `&[Arc<str>]` to match the trait change in `blinc_layout`.

### Fixed
- Popover / context-menu / dropdown / select / combobox / menubar / dialog / sheet / drawer / toast animations now play on the first interaction (class-only `@keyframes` rules were previously skipped on the very first build).
- Dropdown menu's top/bottom Rust-side `.py(1.0)` padding removed (was double-padding with the CSS `padding:` declaration).

### Known Issues
- `.cn-input` / `.cn-textarea` focus ring transition plays smoothly on the first focus but snaps in on every subsequent focus. Border-color transition is unaffected; only the outline transition is affected. See `gotcha_focus_ring_transition_no_replay.md` in dev memory. Deferred.

## [0.4.0] - 2026-04-05

### Changed
- Version bump to align with workspace 0.4.0 release

## [0.1.15] - 2026-03-22

### Fixed

- Removed CSS transition declarations from nav-link, sidebar-item, menubar-trigger, and menubar-item that caused hover-leave visual artifacts
- Sidebar item background set to transparent to prevent stale background on hover-leave
- Clippy warnings in menubar overlay functions (let-binding return)
- Toast slide distance adjusted to 200px for clear right-edge entry animation
- Toast enter/exit animations now use proper off-screen distance for all corner positions
