//! Web (browser) theme
//!
//! Wraps the Universal HID Hybrid variant ([`crate::BlincTheme`],
//! itself a type alias for [`crate::HybridTheme`]). The web target
//! has no native chrome to mimic, so it inherits the framework's
//! cross-platform default identity. Apps that want a different look
//! can call [`ThemeState::init`](crate::ThemeState::init) themselves
//! with a custom [`ThemeBundle`] before [`crate::WebApp::run`] would
//! otherwise install this default.

use crate::theme::{ColorScheme, Theme, ThemeBundle};
use crate::themes::BlincTheme;
use crate::tokens::*;

/// Web-native theme. Currently a thin wrapper around the default
/// Catppuccin-derived [`BlincTheme`] — see the module docs for
/// rationale.
#[derive(Clone, Debug)]
pub struct WebTheme {
    inner: BlincTheme,
}

impl WebTheme {
    pub fn light() -> Self {
        Self {
            inner: BlincTheme::light(),
        }
    }

    pub fn dark() -> Self {
        Self {
            inner: BlincTheme::dark(),
        }
    }

    pub fn bundle() -> ThemeBundle {
        ThemeBundle::new("Web", Self::light(), Self::dark())
    }
}

impl Theme for WebTheme {
    fn name(&self) -> &str {
        "Web"
    }

    fn color_scheme(&self) -> ColorScheme {
        self.inner.color_scheme()
    }

    fn colors(&self) -> &ColorTokens {
        self.inner.colors()
    }

    fn typography(&self) -> &TypographyTokens {
        self.inner.typography()
    }

    fn spacing(&self) -> &SpacingTokens {
        self.inner.spacing()
    }

    fn radii(&self) -> &RadiusTokens {
        self.inner.radii()
    }

    fn shadows(&self) -> &ShadowTokens {
        self.inner.shadows()
    }

    fn animations(&self) -> &AnimationTokens {
        self.inner.animations()
    }
}
