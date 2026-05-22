# Overlay Stack — Design

Status: **DRAFT** · Author: refactor agent · Awaits review before any code changes

## Goal

Replace the current `OverlayManagerInner` (insertion-ordered `IndexMap` with per-kind ad-hoc behavior, ~2920 LOC in `crates/blinc_layout/src/widgets/overlay.rs`) with an `OverlayStack` that enforces **LIFO stack discipline**:

- Every overlay — modal, dialog, dropdown, popover, context menu, menubar, tooltip, toast, hover card — lives in the same stack.
- ESC pops the topmost dismissable entry. (Non-dismissable kinds like toasts are walked past.)
- Click outside the top entry's hit region dismisses it.
- Programmatic close-by-handle also removes everything stacked above (so a nested chain unwinds atomically).
- Z-order is the stack order — no manual `z_index` config.

## Non-goals

- No change to the cn-component public API (`cn::dialog()`, `cn::popover()` etc.) until widgets are migrated one-by-one in Phase 3. The first cut keeps the builder shapes identical and rewires only the internals.
- No change to overlay rendering pipeline (`build_overlay_layer()` still returns a `Div`; how primitives reach the GPU is unchanged).

## Explicit non-feature: no overlay-owned animation system

The current `OverlayManagerInner` ships its own animation stack:
- `OverlayState` FSM (`Closed → Opening → Open → PendingClose → Closing → Closed`).
- `OverlayAnimation` config (per-overlay enter / exit easing, duration, transform).
- `update(current_time_ms)` that ticks animation progress and gates eviction on completion.

**None of this is moving forward.** Blinc already has three animation primitives that do the same job correctly, and the overlay system should compose with them rather than duplicate:

1. **`motion()` container** ([`blinc_layout::motion`](crates/blinc_layout/src/motion.rs)) — wraps content with an enter / exit FSM. `query_motion(key).enter()` / `.exit()` drive transitions; `is_exited()` reports completion. Used everywhere from tabs to checkboxes.
2. **FLIP / `animate_bounds()`** ([`VisualAnimationConfig`](crates/blinc_layout/src/visual_animation.rs)) — for bounds-driven animations (drawer slide-in is just `motion().translate_x(from_offscreen_to_zero)` or a FLIP delta of natural position).
3. **CSS `@keyframes`** — declarative; widget authors can write `.cn-popover-content { animation: scaleIn 150ms ease-out; }` and have it run on insertion without touching Rust.

`OverlayStack` therefore knows about **two states only**: an entry is either *live* (in the stack, accepting input) or *exiting* (still rendered so its motion container can play the exit animation, but excluded from input dispatch). Eviction happens when the motion container reports its exit FSM is done — the stack polls via `query_motion(entry.motion_key).is_exited()` in `update()`.

Concretely:
- **Push** = insert entry. Content is wrapped in a `motion()` keyed by the entry handle. Whatever enter animation the widget configured (motion enter config, CSS animation, FLIP) plays automatically when the wrapper is first rendered.
- **Pop** = mark `exiting = true`, call `query_motion(entry.motion_key).exit()`. Render still happens, motion plays exit, input ignores the entry.
- **Update** = walk entries; remove any whose `exiting` is true AND `query_motion().is_exited()` is true.
- **Auto-dismiss** (toasts) = same as pop, but triggered by elapsed time instead of user action.

What goes away from today's code:
- `OverlayState` enum and the 200-line `StateTransitions` impl.
- `OverlayAnimation` struct and its companion easing math.
- The `is_overlay_closing()` / `set_overlay_closing()` thread-local flag (already deprecated, finally deletable).
- Per-`update()` animation interpolation; motion's scheduler already runs.

## Why the existing manager is the wrong shape

Concrete pain points observed while polishing cn:
1. **Z-order leaks.** `IndexMap` order is insertion order, but `close()` removes by handle, leaving "holes" mid-iteration. Code that walks overlays for input dispatch needs ad-hoc filters to find "the topmost open one".
2. **No single source of truth for top.** `close_top()`, `handle_escape()`, `handle_backdrop_click()` each re-implement "find topmost dismissable" with subtly different rules.
3. **Cross-kind interactions are silently broken.** When a popover opens *over* a dialog, ESC closes the dialog (because the popover doesn't register as the "top dismissable"). The current code papers over this with kind-specific guards in `handle_escape`.
4. **Per-kind builders duplicate state machinery.** `ModalBuilder`, `DialogBuilder`, `DropdownBuilder`, `ContextMenuBuilder`, `ToastBuilder` re-implement `.dismissable(bool)`, `.with_backdrop()`, `.on_close()`, `.content()` ~6 times each.
5. **Two-thirds of the file is plumbing.** Of the 2920 LOC, ~1500 are builders + `OverlayManagerExt` glue. With a single push API + kind tag, this collapses.

## Architecture

### Types

```rust
// crates/blinc_layout/src/widgets/overlay_stack.rs (new)

pub struct OverlayStack {
    /// Strict LIFO. `entries.last()` is the top.
    entries: Vec<OverlayEntry>,
    next_id: AtomicU64,
    viewport: (f32, f32),
    scale_factor: f32,
    current_time_ms: u64,
    /// Set when content / structure changes; consumed by the windowed runner.
    dirty: AtomicBool,
    /// Set when only animation state advanced (no structural delta).
    animation_dirty: AtomicBool,
}

// Sibling type — independent singleton, independent lock.
pub struct ToastTray {
    toasts: Vec<ToastEntry>,
    next_id: AtomicU64,
    corner: Corner,             // TopRight, BottomRight, etc.
    gap_px: f32,
    max_visible: usize,
    current_time_ms: u64,
    dirty: AtomicBool,
}

pub struct OverlayEntry {
    pub handle: OverlayHandle,
    pub kind: OverlayKind,
    /// Anchor / position / backdrop config — NO animation field on this struct
    /// (the wrapping motion() / CSS / FLIP owns animation).
    pub config: OverlayConfig,
    pub content_fn: Arc<dyn Fn() -> Div + Send + Sync>,
    /// Dismiss policy for THIS entry (derived from kind + builder overrides).
    pub dismiss: DismissRules,
    /// Motion container key. The push wrapper renders the entry as
    /// `motion().with_key(motion_key).child(content_fn())` so callers can use
    /// the regular `query_motion()` API to trigger enter / exit. The default
    /// motion config per kind sets a reasonable enter / exit (e.g. dialog fades
    /// in + scales 0.95 → 1.0); widgets override via builder.
    pub motion_key: String,
    /// Spawned wall-clock time, used only for auto-dismiss timers (toasts).
    /// Animation timing is NOT tracked here.
    pub spawned_at_ms: u64,
    /// True after `pop()` (or auto-dismiss): rendered but excluded from input.
    /// Cleared by physical removal in `update()` once motion reports exit done.
    pub exiting: bool,
}

pub struct DismissRules {
    /// ESC pops this entry. False for toasts (and any custom non-dismissable modal).
    pub on_escape: bool,
    /// Click outside this entry's hit region pops it.
    pub on_click_outside: bool,
    /// Mouse leaving this entry / its trigger pops it. (Hover card, tooltip.)
    pub on_mouse_leave: bool,
    /// Auto-dismiss after duration_ms. None = sticky.
    pub auto_after_ms: Option<u64>,
    /// Blocks input from reaching layers below (modal, dialog, drawer, sheet).
    pub blocks_below: bool,
    /// Renders a backdrop element behind this entry.
    pub backdrop: Option<BackdropConfig>,
}
```

`DismissRules` is the **single behavioural contract per entry**. Kind-specific branches that used to live across 4 different methods (`handle_escape`, `handle_click_at`, `close_top`, `update`) become uniform passes that read this struct.

### Default dismiss rules per kind

| Kind          | on_escape | on_click_outside | on_mouse_leave | auto_after_ms | blocks_below | backdrop |
| ------------- | --------- | ---------------- | -------------- | ------------- | ------------ | -------- |
| Modal         | ✓         | ✓ (if dismissable) | —              | —             | ✓            | ✓        |
| Dialog        | ✓         | ✓                | —              | —             | ✓            | ✓        |
| Drawer/Sheet  | ✓         | ✓                | —              | —             | ✓            | ✓ (side) |
| Popover       | ✓         | ✓                | —              | —             | —            | —        |
| Dropdown      | ✓         | ✓                | —              | —             | —            | —        |
| ContextMenu   | ✓         | ✓                | —              | —             | —            | —        |
| Menubar       | ✓         | ✓                | —              | —             | —            | —        |
| NavMenu       | ✓         | ✓                | —              | —             | —            | —        |
| HoverCard     | ✓         | ✓                | ✓ (delay)      | —             | —            | —        |
| Tooltip       | —         | —                | ✓              | —             | —            | —        |
| Toast         | —         | —                | —              | ✓ (e.g. 4s)   | —            | —        |

Toast and Tooltip have `on_escape = false`, so `handle_escape()` walks past them when looking for the top dismissable.

### Core API

```rust
impl OverlayStack {
    // --- Building ---

    pub fn push(&mut self, entry: OverlayEntry) -> OverlayHandle;

    // Replace an existing entry (same handle) — used when a stateful overlay
    // re-renders its config (e.g. anchor moved, content changed).
    pub fn replace(&mut self, handle: OverlayHandle, entry: OverlayEntry);

    // --- Inspecting ---

    pub fn top(&self) -> Option<&OverlayEntry>;
    pub fn top_handle(&self) -> Option<OverlayHandle>;
    pub fn topmost_dismissable_by_escape(&self) -> Option<&OverlayEntry>;
    pub fn topmost_blocking(&self) -> Option<&OverlayEntry>;
    pub fn iter_top_down(&self) -> impl Iterator<Item = &OverlayEntry>;
    pub fn iter_bottom_up(&self) -> impl Iterator<Item = &OverlayEntry>;
    pub fn contains(&self, handle: OverlayHandle) -> bool;
    pub fn is_empty(&self) -> bool;
    pub fn len(&self) -> usize;

    // --- Closing ---

    /// Pop top + everything above it. If `handle` is the top, just pop top.
    /// If `handle` is below other entries, those entries are closed too
    /// (a nested chain unwinds atomically — you can't have a dropdown left
    /// dangling after its parent dialog closes).
    pub fn close(&mut self, handle: OverlayHandle);

    /// Pop top entry. Returns the closed handle.
    pub fn pop(&mut self) -> Option<OverlayHandle>;

    /// Close everything. Used on route transitions, escape-all etc.
    pub fn close_all(&mut self);

    // --- Input dispatch ---

    /// Returns true if the key was consumed. ESC walks top-down through entries,
    /// finds the first with `dismiss.on_escape == true`, closes it + everything above.
    pub fn handle_escape(&mut self) -> bool;

    /// Returns true if click was consumed by the overlay layer (and should not
    /// propagate to underlying widgets). Walks top-down: if click is inside an
    /// entry's hit region, that entry "captures" the click and the search stops.
    /// If click falls outside the top entry AND its dismiss rules say
    /// `on_click_outside`, pops it and continues with the new top (so a click
    /// in empty space cascades through stacked menus until something is
    /// inside-hit or the stack is empty).
    pub fn handle_click_at(&mut self, x: f32, y: f32) -> bool;

    /// Mouse-leave for kinds that want hover-driven close (HoverCard, Tooltip).
    pub fn handle_mouse_leave(&mut self, handle: OverlayHandle);
    pub fn handle_mouse_enter(&mut self, handle: OverlayHandle);

    /// Per-frame tick. Does NOT advance animations (motion / FLIP / CSS run on
    /// their own schedulers). Responsibilities:
    /// - run auto-dismiss timers (toasts).
    /// - reap entries whose `exiting` is true AND `query_motion(motion_key).is_exited()`.
    pub fn update(&mut self, current_time_ms: u64);

    // --- Render ---

    /// Build the overlay layer (single `Div` to be composited above the main UI).
    /// Renders entries in stack order with backdrops slotted in immediately below
    /// each backdrop-bearing entry. Does NOT include toasts — the toast tray
    /// builds its own layer, composited above this one by `blinc_app`.
    pub fn build_overlay_layer(&self) -> Div;

    // --- Queries used by windowed runner ---

    pub fn has_blocking_overlay(&self) -> bool;
    pub fn has_animating_overlays(&self) -> bool;
    pub fn has_visible_overlays(&self) -> bool;
    pub fn take_dirty(&self) -> bool;
    pub fn take_animation_dirty(&self) -> bool;
}
```

### Push API: one builder shape

Today there are six builders (`ModalBuilder`, `DialogBuilder`, `ContextMenuBuilder`, `ToastBuilder`, `DropdownBuilder`, plus `hover_card` reusing `DropdownBuilder`). Each is ~80 LOC of repeated `.dismissable(b)`, `.with_backdrop(...)`, `.on_close(f)`, `.content(f)` setters.

Collapse to a single `OverlayBuilder`:

```rust
pub struct OverlayBuilder {
    kind: OverlayKind,
    config: OverlayConfig,
    dismiss: DismissRules,
    content_fn: Option<Arc<dyn Fn() -> Div + Send + Sync>>,
}

impl OverlayBuilder {
    pub fn new(kind: OverlayKind) -> Self {
        let dismiss = DismissRules::default_for(kind);
        let config = OverlayConfig::default_for(kind);
        Self { kind, config, dismiss, content_fn: None }
    }
    pub fn dismissable(mut self, b: bool) -> Self { … }
    pub fn anchor(mut self, x: f32, y: f32) -> Self { … }
    pub fn anchored_to(mut self, bounds: ElementBounds, dir: AnchorDirection) -> Self { … }
    pub fn backdrop(mut self, cfg: BackdropConfig) -> Self { … }
    pub fn auto_dismiss_after_ms(mut self, ms: u64) -> Self { … }
    pub fn on_close(mut self, f: impl Fn() + 'static) -> Self { … }
    pub fn content<F: Fn() -> Div + Send + Sync + 'static>(mut self, f: F) -> Self { … }
    pub fn show(self) -> OverlayHandle { stack().push(self.into_entry()) }
}
```

Per-kind helpers (`stack().popover()`, `stack().dialog()`, …) become 1-line constructors that return `OverlayBuilder::new(kind)` with kind-appropriate defaults pre-applied. The current cn widget call sites don't have to change shape — they just route through a thinner API.

### Render order

`build_overlay_layer()` emits children in this order (bottom-to-top in z):

1. For each entry, in stack-order (bottom to top):
   - If `entry.dismiss.backdrop` is `Some(_)`, emit a `motion()`-wrapped backdrop div (keyed `<entry.motion_key>:backdrop`) so the backdrop fades in/out via the regular motion API.
   - Emit a `motion()` wrapper keyed `entry.motion_key`, child = `entry.content_fn()`, positioned per `entry.config.position`. The motion config (enter / exit transforms, easing, duration) is whatever the widget set up — `cn::dialog()` typically uses a scale-in motion, `cn::popover()` an opacity fade, `cn::drawer()` a translate, etc. **The stack itself sets nothing on the motion config**; it just provides the key and the child.
2. Toasts come from `ToastTray::build_tray_layer()` and `blinc_app` composites them above the stack's layer. Each toast is wrapped in its own motion() (typically slide-from-edge enter + fade exit). FLIP via `animate_bounds(::position().snappy())` on the tray container handles the reorder when toasts auto-dismiss from the middle of the stack.

Backdrops are SIBLINGS of entries (not parents), so toggling a backdrop's exit animation doesn't unmount the next-down entry. The motion's child being a closure result (rebuilt each frame) means CSS keyframe animations on the content also work naturally — the `@keyframes` rule runs on every "first-paint" of the element, which is exactly the insert moment.

Widget authors choose their animation primitive freely:
- **Scaling dialog enter**: `motion()` with a scale enter config (current cn::dialog convention).
- **Drawer slide-in from edge**: `motion().translate_x(SharedAnimatedValue)` driven by a spring.
- **Popover fade**: a CSS class with `animation: cn-fade-in 120ms ease-out;` — no Rust animation code needed.
- **Toast stack reorder**: FLIP via `animate_bounds(::position().snappy())` on the toast tray (existing pattern in cn::toast).

The overlay stack stays out of all four — it only owns ordering, dismissal, and lifetime.

### Input dispatch

`windowed.rs` already calls:

- `handle_escape()` on ESC keydown.
- `handle_click_at(x, y)` on mouse-down outside any "captured" widget.
- `has_blocking_overlay()` to gate keyboard / mouse routing.

These map 1:1 to the new API. The change is *semantics*:

- **ESC** now closes only the topmost dismissable, not the topmost blocking. Pressing ESC with `dialog → popover → tooltip` stacked closes the popover first.
- **Click outside** walks top-down, popping entries whose `on_click_outside` is true, until a click lands inside an entry OR a non-click-dismissable entry is reached. Today this is per-handler ad hoc.
- **`has_blocking_overlay()`** is the OR of `blocks_below` across all entries (modal/dialog/drawer/sheet anywhere in the stack blocks lower-tier input).

### Migration path (per widget)

For each cn widget the migration is mechanical:

| Widget          | Current calls                                | New equivalent                       |
| --------------- | --------------------------------------------- | ------------------------------------ |
| popover         | `mgr.dropdown().at().content().show()`        | `stack().popover().at().content().show()` |
| dropdown_menu   | `mgr.dropdown()…show()`                       | `stack().dropdown()…show()`          |
| context_menu    | `mgr.context_menu()…show()`                   | `stack().context_menu()…show()`      |
| dialog          | `mgr.modal()…show()` / `mgr.dialog()…show()`  | `stack().dialog()…show()`            |
| drawer          | custom side-positioned modal                  | `stack().drawer().side(Side::Right)…show()` |
| sheet           | custom side-positioned modal                  | `stack().sheet().side(Side::Bottom)…show()` |
| toast           | `mgr.toast().corner().auto_dismiss().show()`  | `stack().toast()…show()`             |
| tooltip         | `mgr.dropdown()…show()` with hover wiring     | `stack().tooltip().anchored_to()…show()` |
| hover_card      | `mgr.hover_card()…show()`                     | `stack().hover_card()…show()`        |
| navigation_menu | `mgr.dropdown()…show()` per submenu           | `stack().popover()…show()` per submenu |
| menubar         | `mgr.dropdown()…show()` per menu              | `stack().popover()…show()` per menu  |

Counts (current `OverlayManager` refs per file): popover 7, dropdown_menu 22, context_menu 20, dialog 6, drawer 5, sheet 5, toast 6, tooltip 7, hover_card 9, navigation_menu 10, menubar 30. Total **127** call sites; mechanical 1-to-1 search/replace plus per-widget adjustment of dismiss-rule defaults.

### `blinc_app` touchpoints

`grep -r "overlay_manager\|build_overlay_layer\|has_blocking_overlay\|has_visible_overlays\|handle_escape\|handle_click_at" crates/blinc_app/src/` returns **57 sites**. Pattern is the same — replace `overlay_manager().lock().unwrap().X(…)` with `stack().X(…)`. All methods listed in "Core API" above already have current-API equivalents, so this is straight rename + signature adjustment.

## Migration phases

### Phase 1 — design lock-in (THIS DOCUMENT)
- Review + sign-off.
- Decisions confirmed (see "Resolved decisions" below): separate kinds for drawer / sheet / dialog; menubar uses N entries with `close(top_handle)` for hover-switch unwinding; nested submenus are independent stack entries unwound via the same `close()` semantics; toast is a sibling type, not a field on the stack.

### Phase 2 — new module, old still active
- Create `crates/blinc_layout/src/widgets/overlay_stack.rs`.
- Port `OverlayKind`, `OverlayConfig`, `BackdropConfig` over unchanged from `overlay.rs`.
- **Do NOT port** `OverlayState` or `OverlayAnimation` — replaced by motion / FLIP / CSS as described above.
- Implement `OverlayStack` + `OverlayEntry` + `DismissRules` + `OverlayBuilder`.
- Wire `motion()` into `build_overlay_layer()`: each entry wrapped in `motion().with_key(entry.motion_key).child(entry.content_fn())`. Backdrops similarly wrapped.
- Add unit tests:
  - push / pop ordering.
  - ESC walks past non-dismissable.
  - close(handle) closes everything above.
  - click-outside cascades.
  - has_blocking_overlay across stack.
  - eviction-after-exit-animation: pop() sets `exiting`, update() removes once motion exits.
- `overlay_state.rs` exposes BOTH `overlay_manager()` (old) and `overlay_stack()` (new) — they don't share state yet.
- No widget migration. Old behaviour fully preserved.
- **Commit gate:** `cargo build --workspace` + `cargo test -p blinc_layout` green. cn_demo unchanged visually.

### Phase 3 — migrate widgets one at a time
Order: **smallest first** so failure modes are caught early.

1. tooltip (7 refs) — fewest dependencies; mouse-leave is the only nontrivial dismiss.
2. popover (7 refs).
3. hover_card (9 refs).
4. toast (6 refs) — validates the corner-stacked render branch.
5. dialog (6 refs), drawer (5 refs), sheet (5 refs) — modal triad together.
6. context_menu (20 refs).
7. navigation_menu (10 refs), menubar (30 refs).
8. dropdown_menu (22 refs) — last because it has the most state-machine glue.

After each widget: `cargo build -p blinc_cn` + run the corresponding cn_demo section by hand. Commit per widget.

### Phase 4 — migrate `blinc_app` touchpoints
- Replace `overlay_manager()` calls in `windowed.rs`, `android.rs`, `ios.rs`, `web.rs`, `fuchsia.rs`.
- Update `BlincContextState` to expose `overlay_stack()` everywhere it currently exposes `overlay_manager()`.
- One commit covering all platforms.

### Phase 5 — delete old code
- Remove `widgets/overlay.rs` (the old `OverlayManagerInner` and friends).
- Remove `overlay_manager()` from `overlay_state.rs`.
- Remove deprecated `is_overlay_closing()` / `set_overlay_closing()` flag.
- Rename `widgets/overlay_stack.rs` → `widgets/overlay.rs` (so call sites don't have to change a second time).
- Final commit.

## Verification

- `cargo build --workspace` green at every phase.
- `cargo test -p blinc_layout --lib` — new stack-discipline tests pass.
- `cargo run -p blinc_app_examples --example cn_demo --features cn` — manual walkthrough of each overlay-using section:
  - Open dropdown, then context-menu within → ESC closes context-menu first, dropdown still open.
  - Open dialog, open popover inside → ESC closes popover, dialog still open.
  - Open menubar menu, hover to another menu → only one menu open at a time.
  - Toast auto-dismisses without disturbing other overlays.
  - Tooltip mouse-leave dismisses tooltip without closing anything else.
- `cargo check --target wasm32-unknown-unknown -p blinc_web_hello` — web build still compiles.

## Resolved decisions

### 1. Drawer / sheet / dialog are distinct `OverlayKind`s

User-side API stays semantic:

```rust
cn::dialog().content(|| …).show();
cn::drawer().right().content(|| …).show();
cn::sheet().bottom().content(|| …).show();
```

Each kind ships its own defaults (motion preset, position rule, padding, CSS classes). The implementation shares the same `DismissRules` shape (all three are blocking modals with backdrop + ESC + click-outside dismiss), but the kind tag drives the defaults, anchoring, and class hooks.

Rejected: `cn::modal().side(Side::Right)` style. The terseness savings (~20 LOC across builders) aren't worth losing the semantic affordance — users would have to remember "drawer-from-the-right = `modal().side(Right).full_height()`" every time.

### 2. Menubar — N entries, one per open submenu level

Matches shadcn / Radix Menubar behaviour. The menubar trigger row itself is a regular widget (always-visible, no stack entry). Each open menu / submenu becomes its own entry:

- Click "File" → `stack.push(popover_entry { anchor: file_trigger_bounds, ... })`. Top of stack = File menu.
- Click "Recent" inside File menu (it has a submenu) → push another entry, anchored to the Recent item. Top = Recent submenu, File menu still below.
- Hover top-level "Edit" while File chain is open:
  ```rust
  stack.close(file_entry_handle); // close() removes target + everything above
  stack.push(edit_entry);
  ```
  `close()` already promises atomic close-everything-above, so the File submenu chain unwinds in one call.
- ESC pops top (one level) without touching the chain below. Existing dismiss rule.
- Click outside the entire menubar row → `stack.handle_click_at()` walks top-down through every open entry, popping each one whose hit region doesn't contain the click. Cascades to empty.

No special menubar bookkeeping in the stack. The menubar widget just owns: which top-level trigger is "active" + the per-trigger entry handle, and routes hover-switch to the close/push above.

### 3. Nested submenus are independent stack entries

Same model as the menubar case above. A submenu opened from inside another submenu is just `stack.push(...)` of a popover entry. Closing the parent via `stack.close(parent_handle)` unwinds the chain.

This means deeply-nested menus stack visibly (each submenu is z-ordered above its parent). Closing the top level cascades everything below it — atomic. No "owned children" relationship to track on `OverlayEntry`.

### 4. Toast is a sibling type, not a field on `OverlayStack`

```rust
// crates/blinc_layout/src/widgets/overlay_stack.rs
pub struct OverlayStack { /* LIFO modals + anchored */ }

// crates/blinc_layout/src/widgets/toast_tray.rs
pub struct ToastTray { /* corner-stacked queue */ }
```

Independent singletons (`overlay_stack()` / `toast_tray()`), independent locks. The two surfaces share zero state. `blinc_app`'s `build_overlay_layer()` composites the two into one render layer at the end:

```rust
let mut overlay = stack.build_overlay_layer();
overlay = overlay.child(tray.build_tray_layer());
```

Rationale: toast behaviour (corner-stacked notification queue, no input dispatch, auto-timeout, tap-to-dismiss, no ESC interaction, no z-ordering with modals) shares nothing with stack behaviour. Co-locating them in one struct invites accidental cross-coupling — e.g. someone writing `close_all` that drops toasts, or a lock contention bug where toast.push() blocks dialog.show(). Sibling types make the boundary explicit and let each surface evolve independently.

### 5. Granular dismiss configuration on every builder

Every `DismissRules` field is exposed as a builder override. Kind sets the defaults; the user overrides any of them per-instance.

```rust
impl OverlayBuilder {
    // Defaults from kind (overridable):
    pub fn dismissable_by_escape(mut self, b: bool) -> Self { … }
    pub fn dismissable_by_click_outside(mut self, b: bool) -> Self { … }
    pub fn dismissable_by_mouse_leave(mut self, b: bool, delay_ms: u32) -> Self { … }
    pub fn auto_dismiss_after_ms(mut self, ms: u64) -> Self { … }
    pub fn blocks_below(mut self, b: bool) -> Self { … }
    pub fn with_backdrop(mut self, cfg: BackdropConfig) -> Self { … }
    pub fn without_backdrop(mut self) -> Self { … }

    // Convenience aliases:
    pub fn dismissable(self, b: bool) -> Self {
        self.dismissable_by_escape(b).dismissable_by_click_outside(b)
    }

    // Lifecycle callbacks:
    pub fn on_open(mut self, f: impl Fn() + Send + Sync + 'static) -> Self { … }
    pub fn on_close(mut self, f: impl Fn(CloseReason) + Send + Sync + 'static) -> Self { … }

    // Animation (delegated to motion / FLIP / CSS — see "non-feature" section):
    pub fn motion_config(mut self, cfg: MotionConfig) -> Self { … }   // optional override
}

pub enum CloseReason {
    /// User code called `handle.close()` or `stack.close(handle)`.
    Programmatic,
    /// ESC key.
    Escape,
    /// Click landed outside this entry's hit region.
    ClickOutside,
    /// Mouse left this entry + its trigger and the close-delay elapsed.
    MouseLeave,
    /// Auto-dismiss timer (toast) elapsed.
    AutoTimeout,
    /// A lower entry was closed; this entry was stacked above and was closed atomically.
    UnwindFromBelow,
}
```

`OverlayHandle` exposes state queries so user code can react reactively:

```rust
impl OverlayHandle {
    pub fn is_live(&self) -> bool;       // present in stack, not exiting
    pub fn is_exiting(&self) -> bool;    // pop() called, motion playing exit
    pub fn close(&self);                 // sugar for stack().close(self)
}
```

`on_open` / `on_close` are the primary integration points for cn widgets that keep a `State<bool>` for "is this dropdown open" — they call `dropdown_state.set(true)` in `on_open` and `dropdown_state.set(false)` in `on_close`. Stale state after click-outside dismiss (a current pain point) is fixed by this contract.

## Risks / open questions

1. **Per-overlay state outside the stack.** Resolved via the `on_close(CloseReason)` callback specified in the "Granular dismiss configuration" section. cn widgets that keep a `State<bool>` for "is this dropdown open" call `state.set(false)` from `on_close`, regardless of how the close happened (escape / click-outside / programmatic / unwind). The current manager has fragmented callback wiring per dismiss-path; the new contract has one entry point.
2. **Eviction-after-exit-animation.** When `pop()` is called, the entry stays in the Vec with `exiting = true` until `query_motion(entry.motion_key).is_exited()` returns true. The widget author's chosen exit animation (motion config, CSS keyframe, FLIP delta) plays to completion before the entry is physically removed. If the widget configured no exit animation, the motion FSM transitions to exited immediately and the entry is reaped on the next `update()` tick. **Failure mode to watch for**: a widget that doesn't wrap its content in `motion()` (or whose motion has no exit phase) would still be evicted next frame — same as today's "no animation configured" path. We should document this in the migration guide so widget authors don't forget the motion wrapper.
3. **Nested submenus inside a single trigger.** Today, navigation_menu opens one big dropdown that internally manages its own submenu state. Migrating each submenu into its own stack entry (Option A in Phase 1 question 3) is cleaner but changes the focus / click-outside boundary visibly. The "one entry, internal sub-state" approach (Option B) is closer to current behaviour and lower risk.
4. **Tooltip re-entry races.** The current `PendingClose → cancel_close → Open` path exists because hover cards have a "grace period" between trigger and content. Porting that to the new state machine needs the same `on_mouse_leave` delay + `on_mouse_enter` cancel logic. Existing FSM carries it through — just need to wire it.
5. **Toast positioning.** Toasts currently use a `toast_corner` + `toast_gap` config on the manager. The new stack keeps them in a separate `toasts: Vec<ToastEntry>` field with the same config — this is the simplest way to avoid mixing LIFO and queue semantics in one Vec.

## Estimated effort

- Phase 1: ~30 min (design review with user).
- Phase 2: ~half-day (new module + tests).
- Phase 3: ~1 day (13 widgets, mechanical).
- Phase 4: ~2 hours (platform runners).
- Phase 5: ~30 min (deletes + rename).
- **Total: ~2 days of focused work, with checkpoints every ~2 hours.**
