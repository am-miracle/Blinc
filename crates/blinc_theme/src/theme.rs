//! Theme trait and core types

use crate::tokens::*;
use std::sync::Arc;

/// Color scheme variant
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColorScheme {
    #[default]
    Light,
    Dark,
}

impl ColorScheme {
    /// Toggle to the opposite scheme
    pub fn toggle(self) -> Self {
        match self {
            ColorScheme::Light => ColorScheme::Dark,
            ColorScheme::Dark => ColorScheme::Light,
        }
    }
}

/// The main theme trait that all themes must implement
pub trait Theme: Send + Sync + std::fmt::Debug {
    /// Get the theme name for debugging
    fn name(&self) -> &str;

    /// Get the current color scheme
    fn color_scheme(&self) -> ColorScheme;

    /// Get color tokens
    fn colors(&self) -> &ColorTokens;

    /// Get typography tokens
    fn typography(&self) -> &TypographyTokens;

    /// Get spacing tokens
    fn spacing(&self) -> &SpacingTokens;

    /// Get radius tokens
    fn radii(&self) -> &RadiusTokens;

    /// Get corner-shape tokens (squircle / superellipse policy).
    ///
    /// Default implementation returns the no-op
    /// [`ShapeTokens::default()`] — themes that don't opt into
    /// squircle rendering can omit this method and existing impls
    /// stay source-compatible. The Universal HID themes override it
    /// to advertise their preferred squircle exponent / threshold;
    /// the paint walker reads it via
    /// [`ThemeState::shape`](crate::ThemeState::shape) and stamps
    /// the effective `n` on each rounded corner that passes the
    /// per-corner threshold check.
    fn shape(&self) -> &ShapeTokens {
        // Static "off" instance returned by reference. Cheap because
        // `ShapeTokens` is `Copy` and 12 bytes, but the trait
        // signature is `&ShapeTokens` to match the rest of the
        // getters, so we hand out a pointer to a const.
        const OFF: ShapeTokens = ShapeTokens {
            corner_smoothing: 0.0,
            corner_exponent: 2.0,
            smoothing_threshold: f32::INFINITY,
        };
        &OFF
    }

    /// Get shadow tokens
    fn shadows(&self) -> &ShadowTokens;

    /// Get animation tokens
    fn animations(&self) -> &AnimationTokens;
}

/// A theme bundle containing both light and dark variants.
///
/// Optionally carries CSS sources via [`Self::with_css`] /
/// [`Self::with_css_file`]; the windowed app's `run_with_theme`
/// entry point registers each attached source into the stylesheet
/// after installing the theme, so a single bundle ships both
/// tokens and the stylesheet that consumes them.
#[derive(Clone)]
pub struct ThemeBundle {
    /// Theme name
    pub name: String,
    /// Light theme variant
    pub light: Arc<dyn Theme>,
    /// Dark theme variant
    pub dark: Arc<dyn Theme>,
    /// CSS sources attached to the bundle, applied in order by the
    /// runtime after `ThemeState::init`.
    pub css_sources: Vec<String>,
}

impl ThemeBundle {
    /// Create a new theme bundle
    pub fn new(
        name: impl Into<String>,
        light: impl Theme + 'static,
        dark: impl Theme + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            light: Arc::new(light),
            dark: Arc::new(dark),
            css_sources: Vec::new(),
        }
    }

    /// Get the theme for the specified color scheme
    pub fn for_scheme(&self, scheme: ColorScheme) -> Arc<dyn Theme> {
        match scheme {
            ColorScheme::Light => Arc::clone(&self.light),
            ColorScheme::Dark => Arc::clone(&self.dark),
        }
    }

    /// Attach an inline CSS source to the bundle.
    ///
    /// The string is appended to [`Self::css_sources`] verbatim and
    /// gets registered via `ctx.add_css(...)` by `run_with_theme`
    /// (and the equivalent mobile / web entry points) after the
    /// bundle's tokens are installed — so CSS `var()` references
    /// resolve against this bundle's variables.
    ///
    /// Multiple calls cascade in the order they were attached.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let bundle = cn_bundle()
    ///     .with_css(blinc_cn::cn_styles::CN_STYLES)
    ///     .with_css(r#"
    ///         #app { padding: 16px; }
    ///         .cn-card { border-width: 2px; }
    ///     "#);
    /// ```
    pub fn with_css(mut self, css: impl Into<String>) -> Self {
        self.css_sources.push(css.into());
        self
    }

    /// Load and attach a CSS file to the bundle.
    ///
    /// On read error the path is appended as a single `/* unread */`
    /// comment containing the error so the failure is surfaced when
    /// the stylesheet is parsed, rather than panicking inside the
    /// builder chain. For strict failure semantics, load the file
    /// yourself and call [`Self::with_css`] with the contents.
    ///
    /// Not available on `wasm32` — the browser sandbox doesn't
    /// expose blocking file reads; fetch the CSS at startup and
    /// pass it through [`Self::with_css`] instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_css_file(self, path: impl AsRef<std::path::Path>) -> Self {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(css) => self.with_css(css),
            Err(e) => self.with_css(format!(
                "/* ThemeBundle::with_css_file({}) failed: {} */",
                path.display(),
                e
            )),
        }
    }
}

impl std::fmt::Debug for ThemeBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThemeBundle")
            .field("name", &self.name)
            .field("css_source_count", &self.css_sources.len())
            .finish()
    }
}
