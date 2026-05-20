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
    border: 2px solid var(--cn-checkbox-border, var(--border));
    border-radius: var(--radius-sm);
    background: var(--cn-checkbox-bg, var(--input-bg));
    cursor: pointer;
    transition: background var(--duration-fast), border-color var(--duration-fast), transform var(--duration-fastest);
}
.cn-checkbox:hover {
    border-color: var(--border-hover);
    transform: scale(1.05, 1.05);
}
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
    background: var(--cn-tabs-list-bg, var(--surface-elevated));
    border-radius: var(--radius-md);
    padding: var(--space-1-5);
    gap: var(--space-1);
}
.cn-tabs-trigger {
    border-radius: var(--radius-default);
    cursor: pointer;
    color: var(--text-secondary);
    transition: background var(--duration-fast), color var(--duration-fast);
}
.cn-tabs-trigger:hover {
    color: var(--text-primary);
    background: var(--surface-overlay);
}
.cn-tabs-trigger--active {
    background: var(--cn-tabs-active-bg, var(--background));
    color: var(--text-primary);
    box-shadow: theme(shadow-sm);
}
.cn-tabs-trigger--disabled {
    opacity: 0.5;
    cursor: not-allowed;
}

/* Tab trigger sizes — height values stay raw (no height tokens exist
   yet; tracked as a future spacing scale addition). */
.cn-tabs-trigger--sm { height: 32px; padding: var(--space-1) var(--space-3); font-size: var(--text-sm); }
.cn-tabs-trigger--md { height: 40px; padding: var(--space-2) var(--space-4); font-size: var(--text-sm); }
.cn-tabs-trigger--lg { height: 48px; padding: var(--space-3) var(--space-5); font-size: var(--text-lg); }

/* ============================================================================
   Select
   ============================================================================ */

.cn-select-trigger {
    background: var(--cn-select-bg, var(--surface));
    border: 1px solid var(--cn-select-border, var(--border));
    border-radius: var(--radius-default);
    cursor: pointer;
    color: var(--text-primary);
    transition: border-color var(--duration-fast);
}
.cn-select-trigger:hover {
    border-color: var(--border-hover);
}

.cn-select-content {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
}

.cn-select-item {
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    color: var(--text-primary);
    border-radius: var(--radius-sm);
    transition: background var(--duration-fastest);
}
.cn-select-item:hover {
    background: var(--surface-elevated);
}
.cn-select-item--selected {
    background: var(--surface-elevated);
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
}

/* ============================================================================
   Dialog
   ============================================================================ */

.cn-dialog {
    background: var(--cn-dialog-bg, var(--surface));
    border: 1px solid var(--cn-dialog-border, var(--border));
    border-radius: var(--radius-xl);
    padding: var(--space-6);
    gap: var(--space-4);
}

/* ============================================================================
   Drawer
   ============================================================================ */

.cn-drawer {
    background: var(--cn-drawer-bg, var(--surface));
    border: 1px solid var(--cn-drawer-border, var(--border));
}
.cn-drawer-header {
    border-bottom: 1px solid var(--border);
    padding: var(--space-4);
}
.cn-drawer-footer {
    padding: var(--space-4);
}

/* ============================================================================
   Sheet
   ============================================================================ */

.cn-sheet {
    background: var(--cn-sheet-bg, var(--surface));
    border: 1px solid var(--cn-sheet-border, var(--border));
}

/* ============================================================================
   Toast
   ============================================================================ */

.cn-toast {
    background: var(--cn-toast-bg, var(--surface));
    border: 1px solid var(--cn-toast-border, var(--border));
    border-radius: var(--radius-xl);
    color: var(--text-primary);
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
    background: var(--cn-accordion-bg, var(--surface-elevated));
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
    background: var(--cn-accordion-content-bg, var(--surface));
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
.cn-sidebar-item:hover {
    background: var(--surface-elevated);
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
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-1);
}
.cn-dropdown-item {
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-primary);
    font-size: var(--text-sm);
    transition: background var(--duration-fastest);
}
.cn-dropdown-item:hover {
    background: var(--surface-elevated);
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
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-1);
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
    background: var(--surface-elevated);
}

/* ============================================================================
   Menubar
   ============================================================================ */

.cn-menubar {
    background: var(--surface);
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
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-4);
}

/* ============================================================================
   Hover Card
   ============================================================================ */

.cn-hover-card-content {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-4);
}

/* ============================================================================
   Tree View
   ============================================================================ */

.cn-tree-node {
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-primary);
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

.cn-resizable-handle {
    background: var(--border);
    transition: background var(--duration-fast);
}
.cn-resizable-handle:hover {
    background: var(--primary);
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
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-default);
    cursor: pointer;
    color: var(--text-primary);
    transition: border-color var(--duration-fast);
}
.cn-combobox-trigger:hover {
    border-color: var(--border-hover);
}
.cn-combobox-content {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
}
.cn-combobox-item {
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    color: var(--text-primary);
    transition: background var(--duration-fastest);
}
.cn-combobox-item:hover {
    background: var(--surface-elevated);
}
"#;
