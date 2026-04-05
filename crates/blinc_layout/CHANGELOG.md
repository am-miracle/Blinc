# Changelog

All notable changes to `blinc_layout` will be documented in this file.

## [0.4.0] - 2026-04-05

### Added

#### Flow Shader 3D Targets
- CSS parser: `target: vertex` and `target: material` in `@flow` blocks
- Output target parsing for 3D: `position`, `world_normal`, `world_position`, `albedo`, `metallic`, `roughness`, `emissive`, `surface_normal`, `alpha_out`
- `parse_output_target()` helper for unified output name resolution

#### Code Editor Enhancements

- VS Code-style search bar via overlay (Cmd+F)
  - Find with case sensitive, whole word, regex toggles
  - Replace with replace current / replace all
  - Match highlighting with active match indicator
  - Signal-driven Stateful overlay (no deadlocks, no infinite rebuilds)
- Code folding with SVG chevron icons in gutter
- Bracket matching highlight (cursor adjacent to brace)
- Indentation guides (vertical lines at indent levels)
- Current line highlight
- Minimap (scaled-down code overview sidebar)
- Gutter: separate fold column, line numbers, separator with proper alignment

### Fixed

- Gutter line numbers vertically centered instead of top-aligned
- Fold toggle click not reaching correct line when lines folded
- Cursor X offset due to gutter width not subtracted from click coordinates
- Editor expanding vertically instead of scrolling on new lines
- Hover artifacts on sidebar, nav, and menubar components
- Checkbox background not updating on check/uncheck state change

## [0.1.15] - 2026-03-22

### Added

#### Code Editor Widget (`code_editor`)

- `code_editor(&state)` — full-featured editable code editor using Stateful incremental updates
- `code_editor_state("content")` — shared state constructor that persists across rebuilds
- Editing: type, Enter (auto-indent), Backspace, Delete, Tab/Shift+Tab (indent/dedent selected lines)
- Cmd+Backspace/Delete: delete word backward/forward
- Cmd+Z / Cmd+Shift+Z: undo/redo (200-entry history stack)
- Cmd+C/X/V: clipboard copy/cut/paste via platform commands
- Cmd+A: select all
- Cmd+Left/Right: word jump navigation
- Smart Home: toggle between first non-whitespace and column 0
- Page Up/Page Down navigation
- Mouse click cursor positioning, mouse drag selection, double-click word select
- Visual selection highlighting with absolutely-positioned divs
- Current line highlight (subtle overlay when focused)
- Cursor with blink animation
- Vertical scrolling via `overflow_y_scroll` on Stateful
- Monospace text measurement cache (`measure_mono()`)
- Per-line syntax highlight cache with dirty tracking
- Escape to blur/unfocus

#### Shared Text Editing Utilities (`text_edit` module)

- `word_boundary_left/right()` for word-jump navigation
- `word_at_position()` for double-click word selection
- `clipboard_read/write()` for OS clipboard integration (macOS pbpaste/pbcopy, Linux xclip)

#### Text Area Improvements

- Visual selection rendering (absolutely-positioned highlight divs)
- Word navigation: Cmd/Ctrl+Left/Right via shared `text_edit` utilities
- Clipboard: Cmd+C/X/V (copy/cut/paste)
- Select all: Cmd+A
- Control character filtering in TEXT_INPUT handler

#### Stateful Enhancements

- `overflow_y_scroll()` builder method on Stateful
- `inner_scroll_physics()` accessor for programmatic scroll control

#### Platform

- Letter key codes (A-Z) mapped to ASCII in windowed.rs KEY_DOWN events for Cmd+key shortcuts

### Other

- `class_name_with_state()` on `ComplexSelector` for extracting class names from state-bearing selectors (e.g. `.cn-sidebar-item:hover`)
- `get_classes()` on `ElementRegistry` for retrieving registered CSS classes by node ID

### Fixed

- Hover transition artifacts on sidebar, nav-link, and menubar components caused by incorrect base_styles caching and transition restart loops on sustained hover
- Checkbox background not updating on check/uncheck: parent node CSS class registrations now updated during full structural rebuilds so `.cn-checkbox--checked` is matched correctly
- Per-side borders with different colors now use SDF-based rendering with proper corner radius instead of rectangular fill strips
- Combobox dropdown scroll by using registered physics directly
- Smooth corner radius artifact on thin borders
- Stale element data (SVG tint, text content, images) during visual-only rebuilds: `element_type` is now updated in `update_subtree_props_from_builder`
- Base styles cache invalidated when CSS classes change during visual-only rebuilds
- Transition detection skipped for nodes already sustaining a hover state to prevent restart loops
- Toast stack container uses absolute positioning so it covers the full viewport independently of other overlay siblings

## [0.1.14] - 2026-02-24

### Added

- `set_auto_id()` and `children_builders_mut()` methods on `ElementBuilder` trait for stable child ID assignment within stateful containers
- `assign_inner_ids_recursive()` in stateful callback wrapper: auto-assigns deterministic inner IDs to children via `derive_child_key()` (e.g. `"button_0:div:0"`) so event handlers and CSS selectors persist across rebuilds

### Fixed

- CSS `color` property now inherits into text elements inside stateful container rebuilds (added text_color propagation to `apply_stylesheet_base_styles_for_subtree()` post-pass)
- Event handlers on inner children are now re-registered during visual-only subtree rebuilds (`update_subtree_props_from_builder`), ensuring closures capturing new state are properly propagated
- CSS text property inheritance (text_color, text_decoration, white_space, text_overflow) added to the full-tree post-pass at initial render time

## [0.1.13] - 2026-02-18

### Added

#### Pointer Query Pressure & Touch Count

- `env(pointer-pressure)` — normalized touch/click pressure (0.0-1.0), smoothed via `pointer-smoothing`
- `env(pointer-touch-count)` — number of active touch points (0 for mouse input)
- `set_pressure()` and `set_touch_count()` on `PointerQueryState` for per-event platform input
- Pressure smoothing using same exponential decay as position smoothing
- Registered `pointer-pressure` and `pointer-touch-count` env var names in renderer

#### Flow Shader Macro & Builders

- `flow!` macro for defining `@flow` shaders using Rust identifiers and primitives (no raw strings)
- `parse_flow_string()` public API for parsing `@flow` block strings into `FlowGraph`
- `FlowRef` enum (`Name`/`Graph`) with `From` impls for `&str`, `String`, and `FlowGraph`
- `.flow()` builder on `Div` accepting both `FlowRef::Name` (CSS reference) and `FlowRef::Graph` (direct)
- `.flow()` builder on `ElementStyle` for stylesheet-based flow references
- `flow:` property in `css!` and `style!` macros
- `flow_graph: Option<Arc<FlowGraph>>` field on `RenderProps` for direct graph attachment
- Flow parser: swizzle tolerance for `stringify!()` spaces (`uv . x` → `uv.x`)
- Flow parser: hex color tolerance for space after `#` (`# ff0000` → `#ff0000`)
- Flow parser: newline normalization in `parse_flow_string()` for `stringify!()` multi-line output

#### SVG Animation Properties

- `fill`, `stroke`, `stroke-width` as animatable CSS properties for SVG elements
- `stroke-dasharray`, `stroke-dashoffset` CSS properties for SVG line-drawing effects
- `d: path("...")` CSS property for SVG path morphing animation
- `svg_path_data` field on ElementStyle and RenderProps for path data propagation
- Decomposed transform fields (`rotate`, `scale_x`, `scale_y`) on ElementStyle to avoid lossy atan2 decomposition
- `parse_scale_values()` helper for extracting original scale factors from CSS

### Fixed

- Border morph/distortion on rounded clipping containers: overflow clip now deferred to after border rendering, preventing double-AA between border SDF and clip SDF at the same boundary
- Borders on elements with `overflow: clip` now render in the foreground layer (after images), matching CSS painting order (content → border → outline)
- `style_to_keyframe_properties()` preserves original rotation angle from CSS (avoids 359deg → -1deg via atan2)
- Clippy: use `Ok(v)` instead of `Some(v) = .ok()` pattern in glass parser

#### CSS Parser & Stylesheet Engine

- Full CSS parser with `#id` selector support and `:hover`, `:focus`, `:active`, `:disabled` pseudo-states
- Global stylesheet injection via `ctx.add_css()` with runtime theme variable support
- Stylesheet runtime integration: base styles, state overrides, and animation overrides (layered application)
- `css!` and `style!` macros for scoped inline styling that returns `ElementStyle`

#### Layout Properties

- `width`, `height` (px, %, auto)
- `min-width`, `max-width`, `min-height`, `max-height`
- `padding`, `margin` (shorthand and per-side)
- `gap` between flex children
- `display: flex | block | none`
- `flex-direction: row | column | row-reverse | column-reverse`
- `flex-wrap: wrap | nowrap`
- `flex-grow`, `flex-shrink`
- `align-items`, `align-self`, `justify-content` (start, center, end, stretch, space-between, space-around, space-evenly)
- `overflow: visible | clip | scroll`
- `border-width`, `border-color`
- `visibility: visible | hidden | collapse | normal` — hides rendering and collapses layout (Display::None in taffy)

#### Visual Properties

- `background` / `background-color` with solid colors, `linear-gradient()`, `radial-gradient()`, `conic-gradient()`
- Color formats: `#hex`, `rgb()`, `rgba()`, named colors, `theme()` tokens
- `border-radius` (uniform and per-corner)
- `opacity`
- `box-shadow` with offset, blur, spread, and color
- `transform: scale() rotate() translate()` (2D transforms)
- `backdrop-filter: glass | blur(Npx) | chrome | gold | metallic | wood`
- `backdrop-filter: liquid-glass(blur() saturate() brightness() border() tint())` variant with configurable border thickness and tint

#### SVG CSS Transform Inheritance

- SVGs now inherit CSS transforms from ancestor elements via `css_affine` propagation
- Affine decomposition into uniform scale (applied to bounds) + rotation angle (sent to shader)

#### Visibility

- `StyleVisibility` enum (`Visible`, `Hidden`) on `ElementStyle`
- CSS parser recognizes `visibility: hidden | visible | collapse | normal`
- `visibility: hidden` both skips rendering and collapses layout (sets `Display::None` in taffy)
- `visibility: visible` restores `Display::Flex` when reversing hidden state
- Visibility applied across all render paths: `render_layer_with_motion`, `render_text_recursive`, `collect_elements_recursive`
- Complex selector state changes (hover/leave) properly reset taffy styles via `base_taffy_styles`
- Layout recomputed after state style changes that affect layout properties

#### 3D CSS Transforms

- `perspective: <px>` for 3D perspective distance
- `rotate-x: <deg>`, `rotate-y: <deg>` for 3D axis rotation
- `translate-z: <px>` for Z-axis translation
- Correct inverse homography unprojection for flat elements with perspective

#### 3D SDF Shapes (Raymarched)

- `shape-3d: box | sphere | cylinder | torus | capsule | group`
- `depth: <px>` for 3D extrusion
- 32-step raymarching with analytical ray-AABB intersection for accurate hit detection
- Edge anti-aliasing via closest-approach distance tracking
- Blinn-Phong lighting with configurable `ambient`, `specular`, `light-direction`, `light-intensity`
- UV mapping: screen-space gradient evaluation for smooth gradients across all 3D faces

#### 3D Boolean Operations

- `3d-op: union | subtract | intersect | smooth-union | smooth-subtract | smooth-intersect`
- `3d-blend: <px>` for smooth blend radius
- `shape-3d: group` for collecting children into compound SDF

#### CSS Animations

- `@keyframes` with named animation definitions
- `animation` shorthand: name, duration, timing-function, delay, iteration-count, direction, fill-mode
- Timing functions: `linear`, `ease`, `ease-in`, `ease-out`, `ease-in-out`
- `animation-direction: normal | reverse | alternate | alternate-reverse`
- `animation-fill-mode: none | forwards | backwards | both`
- `animation-iteration-count: <number> | infinite`
- Animatable properties: `opacity`, `scale`, `scale-x`, `scale-y`, `translate-x`, `translate-y`, `rotate`, `rotate-x`, `rotate-y`, `perspective`, `depth`, `translate-z`, `blend-3d`, `background`, `border-color`, `border-radius`, `border-width`, `box-shadow`, `clip-path`, `filter` (including `blur`), `width`, `height`, `padding`, `margin`, `gap`

#### CSS Transitions

- `transition` shorthand: property, duration, timing-function, delay
- `transition: all 300ms ease` wildcard for all animatable properties
- Comma-separated multi-property transitions
- Smooth reverse transitions on hover-leave with mid-flight reversal support
- Filter identity defaults for proper interpolation (brightness/contrast/saturate default to 1.0)

#### CSS Filters

- `filter` property: `grayscale()`, `invert()`, `sepia()`, `hue-rotate()`, `brightness()`, `contrast()`, `saturate()`
- `filter: blur(Npx)` with GPU Kawase multi-pass blur via LayerEffect pipeline
- `filter: drop-shadow(x y blur color)` with GPU drop-shadow via LayerEffect pipeline
- Space-separated multi-function syntax: `filter: blur(4px) grayscale(1) brightness(1.5)`
- Supports `N`, `N%`, `Ndeg`, `Npx` argument formats
- Nested parenthesis handling in filter parser (e.g. `drop-shadow(4px 4px 8px rgba(0,0,0,0.5))`)

#### Backdrop-Filter Animation

- `backdrop-filter: blur(Npx)` now extracts actual blur radius (was ignoring value)
- `backdrop-filter: blur(Npx) saturate(N) brightness(N)` multi-function parsing
- Animatable `backdrop_blur`, `backdrop_saturation`, `backdrop_brightness` in `KeyframeProperties`
- Transition support: `transition: backdrop-filter 400ms ease` with smooth interpolation
- `@keyframes` support for backdrop-filter properties

#### Selector Hierarchy

- `.class` selectors via `Div::class("name")`
- Descendant combinator (space): `#parent .child`
- Child combinator (`>`): `#parent > .child`
- Compound selectors: `#id.class:hover`
- Structural pseudo-classes: `:first-child`, `:last-child`, `:nth-child(N)`, `:only-child`
- Complex selector matching engine with ancestor chain walking

#### Advanced Selectors

- Adjacent sibling combinator (`+`): `.a + .b`
- General sibling combinator (`~`): `.a ~ .b`
- `:not()` negation pseudo-class
- `:is()` / `:where()` functional pseudo-classes (matches any of listed selectors)
- `:first-of-type`, `:last-of-type`, `:nth-of-type(N)`, `:nth-last-of-type(N)`, `:only-of-type`
- `:empty`, `:root` pseudo-classes
- `*` universal selector

#### Layout Property Animation

- Animatable layout properties: `width`, `height`, `padding`, `margin`, `gap`
- Per-frame taffy style override with automatic `compute_layout()` when layout properties change
- `base_taffy_styles` snapshot for state reset

#### Theme System

- `theme()` function for accessing design tokens in CSS values
- Color tokens: `primary`, `secondary`, `background`, `surface`, `success`, `warning`, `error`, `info`, `text-primary`, `text-secondary`, etc.
- Radius tokens: `radius-none`, `radius-sm`, `radius-default`, `radius-md`, `radius-lg`, `radius-xl`, `radius-full`
- Shadow tokens: `shadow-none`, `shadow-sm`, `shadow-md`, `shadow-lg`, `shadow-xl`

#### Events

- `PINCH` event support in `EventContext` (center and scale fields)

#### CSS Form Widget Styling

- `caret-color` CSS property for text input cursor color
- `selection-color` CSS property for text selection highlight
- `::placeholder` pseudo-element for placeholder text styling (`color` property)
- `Stateful<S>` now forwards `element_id()` and `element_classes()` to ElementBuilder, enabling CSS matching for all Stateful-based widgets
- `.id()` and `.class()` builder methods on TextInput and TextArea
- CSS-aware `state_callback` in TextInput and TextArea: queries active stylesheet for base, `:hover`, `:focus`, `:disabled`, and `::placeholder` overrides
- `set_active_stylesheet()` / `active_stylesheet()` global for widget access to the current stylesheet
- `get_placeholder_style()` on Stylesheet for `::placeholder` pseudo-element lookup
- `Stateful::inner_layout_style()` method for capturing final taffy Style after all builder methods

### Fixed

- Stateful `base_style` capture timing: `on_state()` captured layout style before `.w()`/`.h()` were applied, causing widgets to revert to constructor defaults (e.g., `w_full()`) on state transitions. Now updated in `build()` with the final layout style
- CSS-parsed `backdrop-filter: blur()` glass now uses subtle white tint (`rgba(1,1,1,0.1)`) and zero border-thickness for clean frosted glass appearance (was fully transparent tint, making glass indistinguishable from backdrop)

- CSS-parsed `backdrop-filter: blur()` glass now uses subtle white tint (`rgba(1,1,1,0.1)`) and zero border-thickness for clean frosted glass appearance (was fully transparent tint, making glass indistinguishable from backdrop)
- CSS timing functions now map to spec-correct cubic-bezier values (`ease` was incorrectly using `ease-in-out` polynomial, causing 6.5x slower initial progress than CSS spec)
- Transform-origin mid-flight reversal jitter: `snapshot_before_keyframe_properties` now overlays `transform_origin` from active transition, preventing snap-back on hover-leave
- Cubic-bezier solver rewritten with f64 internal precision and binary-search fallback for jitter-free interpolation at 120fps
- Hover-leave reverse transitions now properly detected and animated (previously snapped to base state instantly)
- Transition repeat regression: pre-reset snapshots prevent spurious re-transitions when hover state persists after transition completion

## [0.1.12] - 2025-01-19

### Added

- `apply_touch_scroll_delta()` method for touch velocity tracking
- `scroll_time` field in `EventContext` for momentum scrolling
- `dispatch_scroll_chain_with_time()` in RenderTree for mobile scroll dispatch
- Momentum deceleration in scroll physics tick for touch devices

### Changed

- Scroll physics now supports velocity-based momentum scrolling
- `on_scroll_end()` starts momentum if velocity exceeds threshold

## [0.1.1] - Initial Release

- Initial public release with layout engine and scroll widgets
