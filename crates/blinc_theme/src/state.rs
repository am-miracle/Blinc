//! Global theme state singleton
//!
//! ThemeState is designed to avoid triggering full layout rebuilds on theme changes.
//! - Visual tokens (colors, shadows) can be animated and only trigger repaints
//! - Layout tokens (spacing, typography, radii) trigger partial layout recomputation

use crate::theme::{ColorScheme, ThemeBundle};
use crate::tokens::*;
use blinc_animation::{AnimatedValue, AnimationScheduler, SchedulerHandle, SpringConfig};
use blinc_core::Color;
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc, Mutex, OnceLock, RwLock};

/// Global theme state instance
static THEME_STATE: OnceLock<ThemeState> = OnceLock::new();

/// Global redraw callback - set by the app layer to trigger UI updates
static REDRAW_CALLBACK: Mutex<Option<fn()>> = Mutex::new(None);

/// Set the redraw callback function
///
/// This should be called by the app layer (e.g., blinc_app) to register
/// a function that triggers UI redraws when theme changes.
pub fn set_redraw_callback(callback: fn()) {
    *REDRAW_CALLBACK.lock().unwrap() = Some(callback);
}

/// Trigger a redraw via the registered callback
fn trigger_redraw() {
    if let Some(callback) = *REDRAW_CALLBACK.lock().unwrap() {
        callback();
    }
}

/// Theme transition animation state
#[derive(Default)]
struct ThemeTransition {
    /// Animated progress value (0.0 = old theme, 1.0 = new theme)
    /// Uses AnimatedValue which is automatically ticked by the animation scheduler
    progress: Option<AnimatedValue>,
    /// Colors from the old theme (for interpolation)
    from_colors: Option<ColorTokens>,
    /// Colors from the new theme (target)
    to_colors: Option<ColorTokens>,
}

/// Global theme state - accessed directly by widgets during render
pub struct ThemeState {
    /// The current theme bundle (light/dark pair).
    ///
    /// Wrapped in `RwLock` so [`Self::init`] can swap it when the user
    /// calls `init` after the framework's automatic platform-theme
    /// init has already populated `THEME_STATE`. Read by
    /// [`Self::set_scheme`] to derive light/dark token sets.
    bundle: RwLock<ThemeBundle>,

    /// Current color scheme
    scheme: RwLock<ColorScheme>,

    /// Current color tokens (can be animated)
    colors: RwLock<ColorTokens>,

    /// Current shadow tokens (can be animated)
    shadows: RwLock<ShadowTokens>,

    /// Current spacing tokens
    spacing: RwLock<SpacingTokens>,

    /// Current typography tokens
    typography: RwLock<TypographyTokens>,

    /// Current radius tokens
    radii: RwLock<RadiusTokens>,

    /// Current animation tokens
    animations: RwLock<AnimationTokens>,

    /// Dynamic color overrides
    color_overrides: RwLock<FxHashMap<ColorToken, Color>>,

    /// Dynamic spacing overrides
    spacing_overrides: RwLock<FxHashMap<SpacingToken, f32>>,

    /// Dynamic radius overrides
    radius_overrides: RwLock<FxHashMap<RadiusToken, f32>>,

    /// Flag indicating theme needs repaint (colors changed)
    needs_repaint: AtomicBool,

    /// Flag indicating theme needs layout (spacing/typography changed)
    needs_layout: AtomicBool,

    /// Animation scheduler handle (set after window creation)
    scheduler_handle: RwLock<Option<SchedulerHandle>>,

    /// Theme transition animation state
    transition: Mutex<ThemeTransition>,
}

impl ThemeState {
    /// Initialize (or replace) the global theme state.
    ///
    /// Safe to call after the framework's auto-initialization. The
    /// windowed / mobile runtimes call [`Self::init_default`] before
    /// the user's UI builder runs, which means a user calling `init`
    /// from inside their builder would previously hit `OnceLock::set`
    /// returning `Err` and have their theme silently dropped. This
    /// path now swaps the bundle and re-derives every token set on
    /// the existing state, then triggers the registered redraw
    /// callback so CSS re-parses against the new theme variables.
    pub fn init(bundle: ThemeBundle, scheme: ColorScheme) {
        let theme = bundle.for_scheme(scheme);

        // Hand any CSS the bundle carries off to the module-level
        // pending queue so the windowed runner registers it via
        // `ctx.add_css` on the next frame — after this init has
        // updated the theme variables. The queue is a
        // `Mutex<Vec<String>>` static in `blinc_core::context_state`,
        // so init order doesn't matter: `run_with_theme` calls
        // `ThemeState::init` before the runner constructs
        // `BlincContextState`, but the runner's
        // `drain_stylesheets()` reads from the same place once it
        // exists.
        for css in &bundle.css_sources {
            blinc_core::context_state::queue_pending_stylesheet(css.clone());
        }

        // Track whether this call is creating the state or mutating
        // an existing one, so we can skip the redraw trigger on the
        // very first install (no UI exists to redraw yet — firing
        // the callback then is harmless, but the trace log it
        // produces would be misleading).
        let mut first_time = false;
        let state = THEME_STATE.get_or_init(|| {
            first_time = true;
            ThemeState {
                bundle: RwLock::new(bundle.clone()),
                scheme: RwLock::new(scheme),
                colors: RwLock::new(theme.colors().clone()),
                shadows: RwLock::new(theme.shadows().clone()),
                spacing: RwLock::new(theme.spacing().clone()),
                typography: RwLock::new(theme.typography().clone()),
                radii: RwLock::new(theme.radii().clone()),
                animations: RwLock::new(theme.animations().clone()),
                color_overrides: RwLock::new(FxHashMap::default()),
                spacing_overrides: RwLock::new(FxHashMap::default()),
                radius_overrides: RwLock::new(FxHashMap::default()),
                needs_repaint: AtomicBool::new(false),
                needs_layout: AtomicBool::new(false),
                scheduler_handle: RwLock::new(None),
                transition: Mutex::new(ThemeTransition::default()),
            }
        });

        if first_time {
            // `get_or_init`'s closure already populated every field
            // from the bundle we own; nothing left to do.
            return;
        }

        // Replace path — `get_or_init` returned a previously-built
        // state (the runner's platform default, or an earlier user
        // bundle). Swap the bundle and re-derive each token set so
        // existing readers see the new values, then trigger the
        // redraw callback so cached stylesheets reparse against the
        // new CSS variables.
        *state.bundle.write().unwrap() = bundle;
        *state.scheme.write().unwrap() = scheme;
        *state.colors.write().unwrap() = theme.colors().clone();
        *state.shadows.write().unwrap() = theme.shadows().clone();
        *state.spacing.write().unwrap() = theme.spacing().clone();
        *state.typography.write().unwrap() = theme.typography().clone();
        *state.radii.write().unwrap() = theme.radii().clone();
        *state.animations.write().unwrap() = theme.animations().clone();
        state.color_overrides.write().unwrap().clear();
        state.spacing_overrides.write().unwrap().clear();
        state.radius_overrides.write().unwrap().clear();
        state.needs_repaint.store(true, Ordering::SeqCst);
        state.needs_layout.store(true, Ordering::SeqCst);
        trigger_redraw();
    }

    /// Set the animation scheduler for theme transitions
    ///
    /// This should be called by the app layer after the window is created
    /// to enable animated theme transitions.
    pub fn set_scheduler(&self, scheduler: &Arc<Mutex<AnimationScheduler>>) {
        let handle = scheduler.lock().unwrap().handle();
        *self.scheduler_handle.write().unwrap() = Some(handle);
    }

    /// Initialize with platform-native theme and system color scheme
    ///
    /// Detects the current OS and uses the appropriate native theme:
    /// - macOS: Apple Human Interface Guidelines theme
    /// - Windows: Fluent Design System 2 theme
    /// - Linux: GNOME Adwaita theme
    pub fn init_default() {
        use crate::platform::detect_system_color_scheme;
        use crate::themes::platform::platform_theme_bundle;

        let bundle = platform_theme_bundle();
        let scheme = detect_system_color_scheme();
        Self::init(bundle, scheme);
    }

    /// Get the global theme state instance
    pub fn get() -> &'static ThemeState {
        THEME_STATE
            .get()
            .expect("ThemeState not initialized. Call ThemeState::init() at app startup.")
    }

    /// Try to get the global theme state (returns None if not initialized)
    pub fn try_get() -> Option<&'static ThemeState> {
        THEME_STATE.get()
    }

    // ========== Color Scheme ==========

    /// Get the current color scheme
    pub fn scheme(&self) -> ColorScheme {
        *self.scheme.read().unwrap()
    }

    /// Set the color scheme (animates colors if scheduler is available)
    pub fn set_scheme(&self, scheme: ColorScheme) {
        let mut current = self.scheme.write().unwrap();
        if *current != scheme {
            tracing::debug!(
                "ThemeState::set_scheme - switching from {:?} to {:?}",
                *current,
                scheme
            );
            // Get current colors before switching
            let old_colors = self.colors.read().unwrap().clone();

            *current = scheme;
            drop(current);

            // Get new theme tokens
            let theme = self.bundle.read().unwrap().for_scheme(scheme);
            let new_colors = theme.colors().clone();

            // Update non-color tokens immediately (they don't animate)
            *self.shadows.write().unwrap() = theme.shadows().clone();
            *self.spacing.write().unwrap() = theme.spacing().clone();
            *self.typography.write().unwrap() = theme.typography().clone();
            *self.radii.write().unwrap() = theme.radii().clone();
            *self.animations.write().unwrap() = theme.animations().clone();

            // Try to animate colors if scheduler handle is available
            let handle_opt = self.scheduler_handle.read().unwrap().clone();
            if let Some(handle) = handle_opt {
                // Start animated transition using AnimatedValue
                let mut transition = self.transition.lock().unwrap();
                transition.from_colors = Some(old_colors.clone());
                transition.to_colors = Some(new_colors.clone());

                // Create AnimatedValue for progress (0 to 100, scaled to avoid spring epsilon issues)
                // The animation scheduler's background thread will tick this automatically
                let mut progress = AnimatedValue::new(handle, 0.0, SpringConfig::gentle());
                progress.set_target(100.0);
                transition.progress = Some(progress);

                // Initialize colors to starting point (old colors at progress=0)
                // This ensures immediate visual feedback before first tick
                drop(transition);
                *self.colors.write().unwrap() = old_colors;
            } else {
                // No scheduler, instant swap
                *self.colors.write().unwrap() = new_colors;
            }

            // Mark for repaint and layout
            self.needs_repaint.store(true, Ordering::SeqCst);
            self.needs_layout.store(true, Ordering::SeqCst);

            // Trigger UI redraw
            trigger_redraw();
        }
    }

    /// Update theme colors based on animation progress
    ///
    /// This should be called during the render loop to update interpolated colors.
    /// Returns true if animation is still in progress and needs more frames.
    pub fn tick(&self) -> bool {
        let mut transition = self.transition.lock().unwrap();

        // Check if we have an active animation
        let progress_opt = transition.progress.as_ref();
        if progress_opt.is_none() {
            return false;
        }

        let progress_anim = transition.progress.as_ref().unwrap();

        // Get current animated value (0-100 range, normalize to 0-1)
        let raw_progress = progress_anim.get();
        let progress = (raw_progress / 100.0).clamp(0.0, 1.0);

        // Check if animation has reached target (within threshold)
        // AnimatedValue.is_animating() just checks spring existence, not actual progress
        let at_target = (raw_progress - 100.0).abs() < 1.0;

        tracing::trace!(
            "Theme tick: raw={:.1}, progress={:.3}, at_target={}",
            raw_progress,
            progress,
            at_target
        );

        // Interpolate colors based on progress
        if let (Some(ref from), Some(ref to)) = (&transition.from_colors, &transition.to_colors) {
            let interpolated = interpolate_color_tokens(from, to, progress);
            drop(transition);
            *self.colors.write().unwrap() = interpolated;

            if at_target {
                // Animation complete - clean up
                let mut transition = self.transition.lock().unwrap();
                transition.progress = None;
                transition.from_colors = None;
                transition.to_colors = None;
                return false;
            }

            // Animation still in progress - trigger rebuild so colors are re-read
            trigger_redraw();
            return true;
        }

        // No colors to interpolate, end animation
        transition.progress = None;
        false
    }

    /// Check if a theme transition animation is in progress
    pub fn is_animating(&self) -> bool {
        let transition = self.transition.lock().unwrap();
        transition
            .progress
            .as_ref()
            .map(|p| p.is_animating())
            .unwrap_or(false)
    }

    /// Toggle between light and dark mode
    pub fn toggle_scheme(&self) {
        let current = self.scheme();
        self.set_scheme(current.toggle());
    }

    // ========== Color Access ==========

    /// Get a color token value (checks override first)
    pub fn color(&self, token: ColorToken) -> Color {
        // Check override first
        if let Some(color) = self.color_overrides.read().unwrap().get(&token) {
            return *color;
        }
        self.colors.read().unwrap().get(token)
    }

    /// Get all color tokens
    pub fn colors(&self) -> ColorTokens {
        self.colors.read().unwrap().clone()
    }

    /// Set a color override (triggers repaint only)
    pub fn set_color_override(&self, token: ColorToken, color: Color) {
        self.color_overrides.write().unwrap().insert(token, color);
        self.needs_repaint.store(true, Ordering::SeqCst);
        trigger_redraw();
    }

    /// Remove a color override
    pub fn remove_color_override(&self, token: ColorToken) {
        self.color_overrides.write().unwrap().remove(&token);
        self.needs_repaint.store(true, Ordering::SeqCst);
        trigger_redraw();
    }

    // ========== CSS Variable Generation ==========

    /// Generate a CSS variable map from all color tokens.
    ///
    /// Returns a `HashMap<String, String>` where keys are variable names
    /// (without `--` prefix) and values are hex color strings.
    /// Variable names match the `theme()` CSS function token names.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let vars = ThemeState::get().to_css_variable_map();
    /// // vars["text-primary"] == "#1a1a2e"
    /// // vars["surface"] == "#ffffff"
    /// ```
    pub fn to_css_variable_map(&self) -> HashMap<String, String> {
        fn hex(c: Color) -> String {
            if c.a < 1.0 {
                format!(
                    "rgba({},{},{},{})",
                    (c.r * 255.0) as u8,
                    (c.g * 255.0) as u8,
                    (c.b * 255.0) as u8,
                    c.a
                )
            } else {
                format!(
                    "#{:02x}{:02x}{:02x}",
                    (c.r * 255.0) as u8,
                    (c.g * 255.0) as u8,
                    (c.b * 255.0) as u8
                )
            }
        }

        let mut vars = HashMap::with_capacity(44);

        // Use self.color() which checks overrides first
        vars.insert("primary".into(), hex(self.color(ColorToken::Primary)));
        vars.insert(
            "primary-hover".into(),
            hex(self.color(ColorToken::PrimaryHover)),
        );
        vars.insert(
            "primary-active".into(),
            hex(self.color(ColorToken::PrimaryActive)),
        );
        vars.insert("secondary".into(), hex(self.color(ColorToken::Secondary)));
        vars.insert(
            "secondary-hover".into(),
            hex(self.color(ColorToken::SecondaryHover)),
        );
        vars.insert(
            "secondary-active".into(),
            hex(self.color(ColorToken::SecondaryActive)),
        );
        vars.insert("success".into(), hex(self.color(ColorToken::Success)));
        vars.insert("success-bg".into(), hex(self.color(ColorToken::SuccessBg)));
        vars.insert("warning".into(), hex(self.color(ColorToken::Warning)));
        vars.insert("warning-bg".into(), hex(self.color(ColorToken::WarningBg)));
        vars.insert("error".into(), hex(self.color(ColorToken::Error)));
        vars.insert("error-bg".into(), hex(self.color(ColorToken::ErrorBg)));
        vars.insert("info".into(), hex(self.color(ColorToken::Info)));
        vars.insert("info-bg".into(), hex(self.color(ColorToken::InfoBg)));
        vars.insert("background".into(), hex(self.color(ColorToken::Background)));
        vars.insert("surface".into(), hex(self.color(ColorToken::Surface)));
        vars.insert(
            "surface-elevated".into(),
            hex(self.color(ColorToken::SurfaceElevated)),
        );
        vars.insert(
            "surface-overlay".into(),
            hex(self.color(ColorToken::SurfaceOverlay)),
        );
        vars.insert(
            "text-primary".into(),
            hex(self.color(ColorToken::TextPrimary)),
        );
        vars.insert(
            "text-secondary".into(),
            hex(self.color(ColorToken::TextSecondary)),
        );
        vars.insert(
            "text-tertiary".into(),
            hex(self.color(ColorToken::TextTertiary)),
        );
        vars.insert(
            "text-inverse".into(),
            hex(self.color(ColorToken::TextInverse)),
        );
        vars.insert("text-link".into(), hex(self.color(ColorToken::TextLink)));
        vars.insert("border".into(), hex(self.color(ColorToken::Border)));
        vars.insert(
            "border-secondary".into(),
            hex(self.color(ColorToken::BorderSecondary)),
        );
        vars.insert(
            "border-hover".into(),
            hex(self.color(ColorToken::BorderHover)),
        );
        vars.insert(
            "border-focus".into(),
            hex(self.color(ColorToken::BorderFocus)),
        );
        vars.insert(
            "border-error".into(),
            hex(self.color(ColorToken::BorderError)),
        );
        vars.insert("input-bg".into(), hex(self.color(ColorToken::InputBg)));
        vars.insert(
            "input-bg-hover".into(),
            hex(self.color(ColorToken::InputBgHover)),
        );
        vars.insert(
            "input-bg-focus".into(),
            hex(self.color(ColorToken::InputBgFocus)),
        );
        vars.insert(
            "input-bg-disabled".into(),
            hex(self.color(ColorToken::InputBgDisabled)),
        );
        vars.insert("selection".into(), hex(self.color(ColorToken::Selection)));
        vars.insert(
            "selection-text".into(),
            hex(self.color(ColorToken::SelectionText)),
        );
        vars.insert("accent".into(), hex(self.color(ColorToken::Accent)));
        vars.insert(
            "accent-subtle".into(),
            hex(self.color(ColorToken::AccentSubtle)),
        );
        vars.insert(
            "tooltip-bg".into(),
            hex(self.color(ColorToken::TooltipBackground)),
        );
        vars.insert(
            "tooltip-text".into(),
            hex(self.color(ColorToken::TooltipText)),
        );

        vars
    }

    // ========== Spacing Access ==========

    /// Get a spacing token value (checks override first)
    pub fn spacing_value(&self, token: SpacingToken) -> f32 {
        if let Some(value) = self.spacing_overrides.read().unwrap().get(&token) {
            return *value;
        }
        self.spacing.read().unwrap().get(token)
    }

    /// Get all spacing tokens
    pub fn spacing(&self) -> SpacingTokens {
        self.spacing.read().unwrap().clone()
    }

    /// Set a spacing override (triggers layout)
    pub fn set_spacing_override(&self, token: SpacingToken, value: f32) {
        self.spacing_overrides.write().unwrap().insert(token, value);
        self.needs_layout.store(true, Ordering::SeqCst);
        trigger_redraw();
    }

    /// Remove a spacing override
    pub fn remove_spacing_override(&self, token: SpacingToken) {
        self.spacing_overrides.write().unwrap().remove(&token);
        self.needs_layout.store(true, Ordering::SeqCst);
        trigger_redraw();
    }

    // ========== Typography Access ==========

    /// Get all typography tokens
    pub fn typography(&self) -> TypographyTokens {
        self.typography.read().unwrap().clone()
    }

    // ========== Radius Access ==========

    /// Get a radius token value (checks override first)
    pub fn radius(&self, token: RadiusToken) -> f32 {
        if let Some(value) = self.radius_overrides.read().unwrap().get(&token) {
            return *value;
        }
        self.radii.read().unwrap().get(token)
    }

    /// Get all radius tokens
    pub fn radii(&self) -> RadiusTokens {
        self.radii.read().unwrap().clone()
    }

    /// Set a radius override (triggers repaint - radii don't affect layout)
    pub fn set_radius_override(&self, token: RadiusToken, value: f32) {
        self.radius_overrides.write().unwrap().insert(token, value);
        self.needs_repaint.store(true, Ordering::SeqCst);
        trigger_redraw();
    }

    // ========== Shadow Access ==========

    /// Get all shadow tokens
    pub fn shadows(&self) -> ShadowTokens {
        self.shadows.read().unwrap().clone()
    }

    // ========== Animation Access ==========

    /// Get all animation tokens
    pub fn animations(&self) -> AnimationTokens {
        self.animations.read().unwrap().clone()
    }

    // ========== Dirty Flags ==========

    /// Check if theme changes require repaint
    pub fn needs_repaint(&self) -> bool {
        self.needs_repaint.load(Ordering::SeqCst)
    }

    /// Clear the repaint flag
    pub fn clear_repaint(&self) {
        self.needs_repaint.store(false, Ordering::SeqCst);
    }

    /// Check if theme changes require layout
    pub fn needs_layout(&self) -> bool {
        self.needs_layout.load(Ordering::SeqCst)
    }

    /// Clear the layout flag
    pub fn clear_layout(&self) {
        self.needs_layout.store(false, Ordering::SeqCst);
    }

    // ========== Override Management ==========

    /// Clear all overrides
    pub fn clear_overrides(&self) {
        self.color_overrides.write().unwrap().clear();
        self.spacing_overrides.write().unwrap().clear();
        self.radius_overrides.write().unwrap().clear();
        self.needs_repaint.store(true, Ordering::SeqCst);
        self.needs_layout.store(true, Ordering::SeqCst);
        trigger_redraw();
    }
}

/// Interpolate between two color token sets
fn interpolate_color_tokens(from: &ColorTokens, to: &ColorTokens, t: f32) -> ColorTokens {
    ColorTokens::lerp(from, to, t)
}
