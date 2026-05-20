//! Default CSS stylesheet for blinc_cn components.
//!
//! All visual properties use `var()` to reference theme tokens,
//! making everything overridable via CSS. Component-level variables
//! (e.g., `--cn-button-primary-bg`) provide targeted override points
//! that fall back to theme tokens.
//!
//! # Usage
//!
//! ```ignore
//! // Register default styles before user CSS
//! blinc_cn::register_cn_styles(ctx);
//!
//! // User CSS can then override:
//! ctx.add_css(r#"
//!     .cn-button--primary { background: #ff6600; }
//!     .cn-card { border-radius: 0; }
//! "#);
//! ```

/// Default CSS for all blinc_cn components.
///
/// Uses `var(--theme-token)` for all color references.
/// Each component defines `var(--cn-component-prop, var(--fallback))` for overridability.
pub const CN_STYLES: &str = r#"
/* ============================================================================
   Typography base — every cn widget inherits the theme's canonical
   sans, body line-height, and dense-HID letter-spacing. CN_STYLES
   was previously silent on these, leaving widgets to fall back to
   the platform default font and ignore the theme's deliberate
   "Noto Sans + 13px text_sm + tracking_tight" choice.
   ============================================================================ */

.cn-button, .cn-card, .cn-card-header, .cn-card-footer,
.cn-badge, .cn-alert, .cn-alert-box,
.cn-input, .cn-textarea, .cn-label,
.cn-checkbox, .cn-switch, .cn-radio,
.cn-tabs-list, .cn-tabs-trigger,
.cn-select-trigger, .cn-select-content, .cn-select-item,
.cn-combobox-trigger, .cn-combobox-content, .cn-combobox-item,
.cn-slider-track, .cn-progress,
.cn-avatar, .cn-tooltip,
.cn-dialog, .cn-drawer, .cn-sheet, .cn-toast,
.cn-accordion, .cn-accordion-trigger, .cn-accordion-content,
.cn-breadcrumb, .cn-breadcrumb-item,
.cn-pagination, .cn-pagination-btn,
.cn-nav-menu, .cn-nav-link,
.cn-sidebar, .cn-sidebar-item,
.cn-dropdown-menu, .cn-dropdown-item,
.cn-context-menu, .cn-context-menu-item,
.cn-menubar, .cn-menubar-trigger, .cn-menubar-item,
.cn-popover-content, .cn-hover-card-content,
.cn-tree-node, .cn-skeleton {
    font-family: var(--font-sans);
}

.cn-kbd {
    font-family: var(--font-mono);
}

/* Body-text widgets get the theme's line-height so multi-line
   content (alert descriptions, accordion content, tooltips) breathes
   correctly. */
.cn-alert, .cn-alert-box, .cn-accordion-content, .cn-tooltip {
    line-height: var(--leading-normal);
}

/* Dense-HID tracking on interactive labels. Picks up the variant's
   tracking_tight value (Universal HID = -0.025em). */
.cn-button, .cn-tabs-trigger,
.cn-input, .cn-textarea,
.cn-select-trigger, .cn-combobox-trigger {
    letter-spacing: var(--tracking-tight);
}

/* ============================================================================
   Button
   ============================================================================ */

/* Button: visual states (hover, active, disabled) handled by Stateful FSM.
   Geometry tokens flow from the active theme's RadiusTokens so each
   variant (Restrained / Hybrid / Expressive) gets its own corner reach.
   The Rust side already reads `RadiusToken` per size — these rules act
   as the cascade override surface users can hook into. */
.cn-button { border-radius: var(--radius-default); }
.cn-button--primary { }
.cn-button--secondary { }
.cn-button--destructive { }
.cn-button--outline { }
.cn-button--ghost { }
.cn-button--link { }
.cn-button--disabled { }
.cn-button--sm { border-radius: var(--radius-sm); }
.cn-button--md { border-radius: var(--radius-default); }
.cn-button--lg { border-radius: var(--radius-lg); }
.cn-button--icon { border-radius: var(--radius-default); }

/* ============================================================================
   Card
   ============================================================================ */

.cn-card {
    background: var(--cn-card-bg, var(--surface));
    border: 1px solid var(--cn-card-border, var(--border));
    border-radius: var(--cn-card-radius, var(--radius-xl));
    padding: var(--space-6);
    gap: var(--space-4);
}

.cn-card-header {
    gap: var(--space-1-5);
}

.cn-card-footer {
    gap: var(--space-2);
}

/* ============================================================================
   Badge
   ============================================================================ */

.cn-badge {
    border-radius: var(--radius-full);
    font-size: var(--text-xs);
    /* `2 10` doesn't have an exact spacing token (space-0-5 = 2,
       space-2-5 = 10) — use the closest matching pair. */
    padding: var(--space-0-5) var(--space-2-5);
}
.cn-badge--default {
    background: var(--primary);
    color: var(--text-inverse);
}
.cn-badge--secondary {
    background: var(--secondary);
    color: var(--text-inverse);
}
.cn-badge--success {
    background: var(--success);
    color: var(--text-inverse);
}
.cn-badge--warning {
    background: var(--warning);
    color: var(--text-inverse);
}
.cn-badge--destructive {
    background: var(--error);
    color: var(--text-inverse);
}
.cn-badge--outline {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-primary);
}

/* ============================================================================
   Alert
   ============================================================================ */

.cn-alert {
    background: var(--cn-alert-bg, var(--surface));
    border: 1px solid var(--cn-alert-border, var(--border));
    border-radius: var(--radius-default);
    color: var(--text-primary);
    font-size: var(--text-sm);
    padding: var(--space-4);
}
.cn-alert-box {
    background: var(--cn-alert-bg, var(--surface));
    border: 1px solid var(--cn-alert-border, var(--border));
    border-radius: var(--radius-default);
    color: var(--text-primary);
    font-size: var(--text-sm);
    padding: var(--space-4);
    gap: var(--space-3);
}
.cn-alert--success {
    background: var(--success-bg);
    border-color: var(--success);
    color: var(--success);
}
.cn-alert--warning {
    background: var(--warning-bg);
    border-color: var(--warning);
    color: var(--warning);
}
.cn-alert--error {
    background: var(--error-bg);
    border-color: var(--error);
    color: var(--error);
}
.cn-alert--info {
    background: var(--info-bg);
    border-color: var(--info);
    color: var(--info);
}

/* ============================================================================
   Separator
   ============================================================================ */

.cn-separator {
    background: var(--cn-separator-color, var(--border));
}

/* ============================================================================
   Skeleton
   ============================================================================ */

.cn-skeleton {
    background: var(--cn-skeleton-bg, var(--surface-elevated));
    border-radius: var(--radius-sm);
}

/* ============================================================================
   Input
   ============================================================================ */

.cn-input {
    background: var(--cn-input-bg, var(--input-bg));
    border: 1px solid var(--cn-input-border, var(--border));
    border-radius: var(--radius-default);
    color: var(--text-primary);
}
.cn-input:hover {
    border-color: var(--border-hover);
    background: var(--input-bg-hover);
}
.cn-input:focus {
    border-color: var(--border-focus);
    background: var(--input-bg-focus);
}
.cn-input--error {
    border-color: var(--border-error);
}

.cn-input--sm { font-size: var(--text-xs); }
.cn-input--md { font-size: var(--text-sm); }
.cn-input--lg { font-size: var(--text-lg); }

/* ============================================================================
   Textarea
   ============================================================================ */

.cn-textarea {
    background: var(--cn-textarea-bg, var(--input-bg));
    border: 1px solid var(--cn-textarea-border, var(--border));
    border-radius: var(--radius-default);
    color: var(--text-primary);
}
.cn-textarea:hover {
    border-color: var(--border-hover);
    background: var(--input-bg-hover);
}
.cn-textarea:focus {
    border-color: var(--border-focus);
    background: var(--input-bg-focus);
}

/* ============================================================================
   Label
   ============================================================================ */

.cn-label {
    color: var(--cn-label-color, var(--text-primary));
}
.cn-label--disabled {
    color: var(--text-tertiary);
}

/* ============================================================================
   Kbd
   ============================================================================ */

.cn-kbd {
    background: var(--cn-kbd-bg, var(--surface));
    border-color: var(--cn-kbd-border, var(--border));
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
}

/* ============================================================================
   Checkbox
   ============================================================================ */

.cn-checkbox {
    /* Border width is Rust-owned because it varies per size
       (1.5px / 2px / 2px). Border color + bg are state-driven by the
       Stateful builder, but we still set `transition:` here so any user
       cascade override animates rather than snaps. */
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: background var(--duration-fast), border-color var(--duration-fast);
}
/* Hover scale is Rust-driven via the Stateful FSM — duplicating it here
   compounded with the Rust transform. State colors handled in Rust too. */
.cn-checkbox--checked {
    background: var(--cn-checkbox-checked-bg, var(--primary));
    border-color: var(--cn-checkbox-checked-border, var(--primary));
}
.cn-checkbox--disabled {
    opacity: 0.5;
    cursor: not-allowed;
}

/* ============================================================================
   Switch
   ============================================================================ */

.cn-switch {
    border-radius: var(--radius-full);
    cursor: pointer;
    transition: background var(--duration-normal);
}
.cn-switch-track {
    background: var(--cn-switch-off-bg, var(--border));
    border-radius: var(--radius-full);
}
.cn-switch-track--on {
    background: var(--cn-switch-on-bg, var(--primary));
}
.cn-switch-thumb {
    background: var(--cn-switch-thumb, var(--text-inverse));
    border-radius: var(--radius-full);
}
.cn-switch--disabled {
    opacity: 0.5;
    cursor: not-allowed;
}

/* ============================================================================
   Radio
   ============================================================================ */

.cn-radio {
    border: 2px solid var(--cn-radio-border, var(--border-secondary));
    border-radius: var(--radius-full);
    cursor: pointer;
    transition: border-color var(--duration-fast), transform var(--duration-fastest);
}
.cn-radio:hover {
    border-color: var(--cn-radio-hover-border, var(--primary));
    transform: scale(1.05, 1.05);
}
.cn-radio--selected {
    border-color: var(--cn-radio-selected, var(--primary));
}
.cn-radio-dot {
    background: var(--cn-radio-dot, var(--primary));
    border-radius: var(--radius-full);
}
.cn-radio--disabled {
    opacity: 0.5;
    cursor: not-allowed;
}

/* ============================================================================
   Tabs
   ============================================================================ */

.cn-tabs-list {
    /* Tonal tray for the trigger row — uses surface-overlay so the
       active trigger (raised to --surface) reads as elevated. */
    background: var(--cn-tabs-list-bg, var(--surface-overlay));
    border-radius: var(--radius-md);
    padding: var(--space-1);
    gap: var(--space-1);
}
.cn-tabs-trigger {
    border-radius: var(--radius-default);
    cursor: pointer;
    color: var(--text-secondary);
    transition: color var(--duration-fast);
}
.cn-tabs-trigger:hover:not(.cn-tabs-trigger--active) {
    color: var(--text-primary);
}
.cn-tabs-trigger--active {
    /* Active trigger lifts to --surface so it stands out from the
       --surface-overlay tray underneath. Previously --background,
       which made the active trigger MERGE with the canvas instead
       of reading as raised. */
    background: var(--cn-tabs-active-bg, var(--surface));
    color: var(--text-primary);
    box-shadow: theme(shadow-sm);
}
.cn-tabs-trigger--disabled {
    opacity: 0.5;
    cursor: not-allowed;
}

/* Tab trigger sizes — content + padding determines height (no fixed
   heights). Vertical padding kept tight to keep the tray compact. */
.cn-tabs-trigger--sm { padding: var(--space-1) var(--space-3); font-size: var(--text-sm); }
.cn-tabs-trigger--md { padding: var(--space-1-5) var(--space-4); font-size: var(--text-sm); }
.cn-tabs-trigger--lg { padding: var(--space-2) var(--space-5); font-size: var(--text-lg); }

/* ============================================================================
   Select
   ============================================================================ */

.cn-select-trigger {
    /* `background`, `border`, and `color` are Rust-owned (state-aware:
       open / disabled / placeholder). Leaving CSS values here would
       overwrite the disabled `InputBgDisabled` fill with `--surface`
       (white) — the exact regression that hid the disabled trigger bg.
       Keep only the radius + cursor + transition; users still cascade
       via more specific rules if they need to override. */
    border-radius: var(--radius-default);
    cursor: pointer;
    transition: border-color var(--duration-fast);
}

.cn-select-content {
    /* Floating dropdown panel → elevation 2.
       Shared overlay-menu chrome — keep `.cn-dropdown-menu`,
       `.cn-context-menu`, and `.cn-combobox-content` in sync. */
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-default);
    padding: var(--space-1);
}

.cn-select-item {
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    color: var(--text-primary);
    border-radius: var(--radius-sm);
    transition: background var(--duration-fastest);
}
/* Item hover uses `--accent-subtle` because the parent panel is
   already at `--surface-elevated`; hovering to the same colour
   would be invisible. `--accent-subtle` is a low-alpha accent
   tint specifically designed for this use. */
.cn-select-item:hover {
    background: var(--accent-subtle);
}
.cn-select-item--selected {
    background: var(--accent-subtle);
}

/* ============================================================================
   Slider
   ============================================================================ */

.cn-slider-track {
    background: var(--cn-slider-track-bg, var(--surface-elevated));
    border-radius: var(--radius-full);
}
.cn-slider-fill {
    background: var(--cn-slider-fill-bg, var(--primary));
    border-radius: var(--radius-full);
}
.cn-slider-thumb {
    border: 2px solid var(--cn-slider-thumb-border, var(--border));
    border-radius: var(--radius-full);
    background: var(--cn-slider-thumb-bg, var(--surface));
    cursor: pointer;
}

/* ============================================================================
   Progress
   ============================================================================ */

.cn-progress {
    background: var(--cn-progress-track, var(--secondary));
    border-radius: var(--radius-full);
    overflow: hidden;
}
.cn-progress-bar {
    background: var(--cn-progress-bar, var(--primary));
    border-radius: var(--radius-full);
    transition: width var(--duration-slow);
}
.cn-progress--sm { height: var(--space-1); }
.cn-progress--md { height: var(--space-2); }
.cn-progress--lg { height: var(--space-3); }

/* ============================================================================
   Avatar
   ============================================================================ */

.cn-avatar {
    background: var(--cn-avatar-bg, var(--surface));
    border-radius: var(--radius-full);
    overflow: hidden;
}
.cn-avatar--square {
    border-radius: var(--radius-default);
}

/* ============================================================================
   Spinner
   ============================================================================ */

.cn-spinner {
    color: var(--cn-spinner-color, var(--primary));
}

/* ============================================================================
   Tooltip
   ============================================================================ */

.cn-tooltip {
    background: var(--cn-tooltip-bg, var(--tooltip-bg));
    color: var(--cn-tooltip-text, var(--tooltip-text));
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    padding: var(--space-1-5) var(--space-3);
    /* CSS-driven fade-in. Motion FSM via `motion_enter` on the new
       OverlayStack is currently not propagating opacity to the cached
       primitive batch correctly (Phase 3 known issue), so we delegate
       enter animation to the CSS animation system which the renderer
       already handles per-frame. Exit snaps for now; once the motion
       integration is fixed, switch back to motion_enter/_exit. */
    animation: cn-tooltip-enter var(--duration-fast) ease-out;
}

@keyframes cn-tooltip-enter {
    from { opacity: 0; }
    to   { opacity: 1; }
}

/* ============================================================================
   Dialog
   ============================================================================ */

.cn-dialog {
    /* Modal → elevation 2 (lifts above surrounding cards) */
    background: var(--cn-dialog-bg, var(--surface-elevated));
    border: 1px solid var(--cn-dialog-border, var(--border));
    border-radius: var(--radius-xl);
    padding: var(--space-6);
    gap: var(--space-4);
    /* CSS-driven enter — same approach as cn-tooltip / cn-popover while the
       motion FSM integration with the new OverlayStack is being fixed. */
    animation: cn-dialog-enter var(--duration-normal) ease-out;
    transform-origin: center;
}

@keyframes cn-dialog-enter {
    from { opacity: 0; transform: scale(0.96); }
    to   { opacity: 1; transform: scale(1); }
}

/* ============================================================================
   Drawer
   ============================================================================ */

.cn-drawer {
    /* Modal overlay → elevation 2 */
    background: var(--cn-drawer-bg, var(--surface-elevated));
    border: 1px solid var(--cn-drawer-border, var(--border));
    /* CSS-driven enter — slide in from edge + fade. The drawer is pinned to
       an edge by OverlayStack; sides apply via .cn-drawer--left/right. */
    animation: cn-drawer-enter-left var(--duration-normal) ease-out;
}
.cn-drawer--right {
    animation-name: cn-drawer-enter-right;
}
.cn-drawer--top {
    animation-name: cn-drawer-enter-top;
}
.cn-drawer--bottom {
    animation-name: cn-drawer-enter-bottom;
}
.cn-drawer-header {
    border-bottom: 1px solid var(--border);
    padding: var(--space-4);
}
.cn-drawer-footer {
    padding: var(--space-4);
}

@keyframes cn-drawer-enter-left {
    from { opacity: 0; transform: translateX(-100%); }
    to   { opacity: 1; transform: translateX(0); }
}
@keyframes cn-drawer-enter-right {
    from { opacity: 0; transform: translateX(100%); }
    to   { opacity: 1; transform: translateX(0); }
}
@keyframes cn-drawer-enter-top {
    from { opacity: 0; transform: translateY(-100%); }
    to   { opacity: 1; transform: translateY(0); }
}
@keyframes cn-drawer-enter-bottom {
    from { opacity: 0; transform: translateY(100%); }
    to   { opacity: 1; transform: translateY(0); }
}

/* ============================================================================
   Sheet
   ============================================================================ */

.cn-sheet {
    /* Modal overlay → elevation 2 */
    background: var(--cn-sheet-bg, var(--surface-elevated));
    border: 1px solid var(--cn-sheet-border, var(--border));
    animation: cn-sheet-enter-right var(--duration-normal) ease-out;
}
.cn-sheet--left   { animation-name: cn-sheet-enter-left; }
.cn-sheet--right  { animation-name: cn-sheet-enter-right; }
.cn-sheet--top    { animation-name: cn-sheet-enter-top; }
.cn-sheet--bottom { animation-name: cn-sheet-enter-bottom; }

@keyframes cn-sheet-enter-left {
    from { opacity: 0; transform: translateX(-100%); }
    to   { opacity: 1; transform: translateX(0); }
}
@keyframes cn-sheet-enter-right {
    from { opacity: 0; transform: translateX(100%); }
    to   { opacity: 1; transform: translateX(0); }
}
@keyframes cn-sheet-enter-top {
    from { opacity: 0; transform: translateY(-100%); }
    to   { opacity: 1; transform: translateY(0); }
}
@keyframes cn-sheet-enter-bottom {
    from { opacity: 0; transform: translateY(100%); }
    to   { opacity: 1; transform: translateY(0); }
}

/* ============================================================================
   Toast
   ============================================================================ */

.cn-toast {
    /* Floating notification → elevation 2 */
    background: var(--cn-toast-bg, var(--surface-elevated));
    border: 1px solid var(--cn-toast-border, var(--border));
    border-radius: var(--radius-xl);
    color: var(--text-primary);
    /* CSS-driven enter — slide in from the side + fade. The toast tray
       positions toasts at a corner, so a slide-in pairs naturally. */
    animation: cn-toast-enter var(--duration-normal) ease-out;
}

@keyframes cn-toast-enter {
    from { opacity: 0; transform: translateX(8%); }
    to   { opacity: 1; transform: translateX(0); }
}
.cn-toast--success {
    border-left: 4px solid var(--success);
}
.cn-toast--warning {
    border-left: 4px solid var(--warning);
}
.cn-toast--error {
    border-left: 4px solid var(--error);
}
.cn-toast--info {
    border-left: 4px solid var(--info);
}

/* ============================================================================
   Accordion
   ============================================================================ */

.cn-accordion {
    /* Accordion outer is a card-tier container, not a modal —
       elevation 1 (--surface). */
    background: var(--cn-accordion-bg, var(--surface));
    border: 1.5px solid var(--cn-accordion-border, var(--border));
    border-radius: var(--radius-xl);
}
.cn-accordion-trigger {
    padding: var(--space-4) var(--space-3);
    cursor: pointer;
    color: var(--text-primary);
    font-size: var(--text-sm);
}
.cn-accordion-trigger:hover {
    background: var(--surface-overlay);
}
.cn-accordion-content {
    /* Expanded content recedes to the canvas tone inside the
       elevated container. */
    background: var(--cn-accordion-content-bg, var(--background));
    border-top: 1px solid var(--border);
    color: var(--text-secondary);
}

/* ============================================================================
   Breadcrumb
   ============================================================================ */

.cn-breadcrumb {
    gap: var(--space-2);
    color: var(--text-secondary);
}
.cn-breadcrumb-item {
    color: var(--text-secondary);
    cursor: pointer;
}
.cn-breadcrumb-item:hover {
    color: var(--text-primary);
}
.cn-breadcrumb-item--active {
    color: var(--text-primary);
}

/* ============================================================================
   Pagination
   ============================================================================ */

.cn-pagination {
    gap: var(--space-1);
}
.cn-pagination-btn {
    border: 1px solid var(--border);
    border-radius: var(--radius-default);
    cursor: pointer;
    color: var(--text-primary);
}
.cn-pagination-btn:hover {
    background: var(--surface-elevated);
}
.cn-pagination-btn--active {
    background: var(--primary);
    color: var(--text-inverse);
    border-color: var(--primary);
}
.cn-pagination-btn--disabled {
    opacity: 0.5;
    cursor: not-allowed;
}

/* ============================================================================
   Navigation Menu
   ============================================================================ */

.cn-nav-menu {
    gap: var(--space-1);
}
.cn-nav-link {
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    color: var(--text-secondary);
}
.cn-nav-link:hover {
    background: var(--surface-elevated);
    color: var(--text-primary);
}
.cn-nav-link--active {
    background: var(--surface-elevated);
    color: var(--text-primary);
}

.cn-nav-menu-content {
    /* Floating overlay → elevation 2 */
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    /* CSS-driven enter — slide down + fade. Motion FSM workaround. */
    animation: cn-nav-menu-enter var(--duration-fast) ease-out;
    transform-origin: top center;
}

@keyframes cn-nav-menu-enter {
    from { opacity: 0; transform: scale(0.98) translateY(-4px); }
    to   { opacity: 1; transform: scale(1) translateY(0); }
}

/* ============================================================================
   Sidebar
   ============================================================================ */

.cn-sidebar {
    background: var(--cn-sidebar-bg, var(--surface));
    border-right: 1px solid var(--border);
}
.cn-sidebar-item {
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    background: transparent;
    color: var(--text-secondary);
}
.cn-sidebar-item:hover:not(.cn-sidebar-item--active) {
    /* Subtle accent feedback — matches the overlay-menu hover treatment
       (dropdown / select / context). Previously this used
       `--surface-elevated` (#FBFCFE in light), which is the same token
       as the active state, so hover and active looked identical. */
    background: var(--accent-subtle);
    color: var(--text-primary);
}
.cn-sidebar-item--active {
    background: var(--surface-elevated);
    color: var(--text-primary);
}

/* ============================================================================
   Scroll Area
   ============================================================================ */

.cn-scroll-area {
    overflow: hidden;
}

/* ============================================================================
   Dropdown Menu
   ============================================================================ */

.cn-dropdown-menu {
    /* Shared overlay-menu chrome with select / context / combobox. */
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-default);
    padding: var(--space-1);
    /* CSS-driven enter — slight scale + fade. Motion FSM workaround. */
    animation: cn-dropdown-menu-enter var(--duration-fast) ease-out;
    transform-origin: top center;
}

@keyframes cn-dropdown-menu-enter {
    from { opacity: 0; transform: scale(0.96) translateY(-4px); }
    to   { opacity: 1; transform: scale(1) translateY(0); }
}
.cn-dropdown-item {
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-primary);
    font-size: var(--text-sm);
    transition: background var(--duration-fastest);
}
/* `--accent-subtle` — parent panel sits at `--surface-elevated`,
   so hovering to the same tier would be invisible. */
.cn-dropdown-item:hover {
    background: var(--accent-subtle);
}
.cn-dropdown-item--disabled {
    opacity: 0.5;
    cursor: not-allowed;
}
.cn-dropdown-item--destructive {
    color: var(--error);
}

/* ============================================================================
   Context Menu
   ============================================================================ */

.cn-context-menu {
    /* Shared overlay-menu chrome with select / dropdown / combobox. */
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-default);
    padding: var(--space-1);
    /* CSS-driven enter — small scale + fade. Motion FSM workaround. */
    animation: cn-context-menu-enter var(--duration-fast) ease-out;
    transform-origin: top left;
}

@keyframes cn-context-menu-enter {
    from { opacity: 0; transform: scale(0.96) translateY(-2px); }
    to   { opacity: 1; transform: scale(1) translateY(0); }
}
.cn-context-menu-item {
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-primary);
    font-size: var(--text-sm);
    transition: background var(--duration-fastest);
}
.cn-context-menu-item:hover {
    background: var(--accent-subtle);
}

/* ============================================================================
   Menubar
   ============================================================================ */

.cn-menubar {
    /* Elevated container → elevation 2 */
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-1);
    gap: var(--space-1);
}
.cn-menubar-trigger {
    padding: var(--space-1-5) var(--space-3);
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-primary);
    font-size: var(--text-sm);
    background: transparent;
}
.cn-menubar-trigger:hover {
    background: var(--surface-elevated);
}
.cn-menubar-item {
    border-radius: var(--radius-sm);
    background: transparent;
}
.cn-menubar-item:hover {
    background: var(--surface-elevated);
}

/* ============================================================================
   Popover
   ============================================================================ */

.cn-popover-content {
    /* Floating overlay → elevation 2 */
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-4);
    /* CSS-driven enter — same approach as cn-tooltip while the motion FSM
       integration with the new OverlayStack is being fixed. */
    animation: cn-popover-enter var(--duration-normal) ease-out;
    transform-origin: top center;
}

@keyframes cn-popover-enter {
    from { opacity: 0; transform: scale(0.96) translateY(-4px); }
    to   { opacity: 1; transform: scale(1) translateY(0); }
}

/* ============================================================================
   Hover Card
   ============================================================================ */

.cn-hover-card-content {
    /* Floating overlay → elevation 2 */
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-4);
    /* CSS-driven enter — same approach as cn-tooltip / cn-popover. */
    animation: cn-hover-card-enter var(--duration-normal) ease-out;
    transform-origin: top center;
}

@keyframes cn-hover-card-enter {
    from { opacity: 0; transform: scale(0.96) translateY(-4px); }
    to   { opacity: 1; transform: scale(1) translateY(0); }
}

/* ============================================================================
   Tree View
   ============================================================================ */

.cn-tree-node {
    /* No `padding` here — Rust owns per-side padding so the left side
       can encode tree-depth indent. CSS overriding `padding:` would
       collapse all rows to the same x-offset. */
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: background var(--duration-fastest);
}
.cn-tree-node:hover {
    background: var(--surface-elevated);
}
.cn-tree-node--selected {
    background: var(--primary);
    color: var(--text-inverse);
}

/* ============================================================================
   Resizable
   ============================================================================ */

/* `.cn-resizable-handle` is the wide HIT AREA wrapper (thickness +
   hit padding on each side). The actual visible thin handle line
   is the Rust-side inner div whose background is theme-driven —
   `--border` at rest, `--primary` while dragging. Painting the
   wrapper background would fill the entire hit zone with that
   colour, making the handle read 2-3× wider than its actual
   visual stripe. Keep the wrapper transparent. */
.cn-resizable-handle {
    background: transparent;
}

/* ============================================================================
   Collapsible
   ============================================================================ */

.cn-collapsible-trigger {
    cursor: pointer;
    color: var(--text-primary);
}

/* ============================================================================
   Combobox
   ============================================================================ */

.cn-combobox-trigger {
    /* Match select-trigger: bg / border / color are Rust-owned so the
       state-aware disabled fill isn't clobbered by CSS. */
    border-radius: var(--radius-default);
    cursor: pointer;
    transition: border-color var(--duration-fast);
}
.cn-combobox-content {
    /* Shared overlay-menu chrome with select / dropdown / context. */
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-default);
    padding: var(--space-1);
}
.cn-combobox-item {
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-primary);
    font-size: var(--text-sm);
    transition: background var(--duration-fastest);
}
.cn-combobox-item:hover {
    background: var(--accent-subtle);
}
.cn-combobox-item--selected {
    background: var(--accent-subtle);
}
"#;
