//! Text measurement using actual font metrics
//!
//! Provides accurate text measurement for layout by using the same font
//! as the renderer.

use blinc_layout::text_measure::{TextLayoutOptions, TextMeasurer, TextMetrics};
use blinc_layout::GenericFont as LayoutGenericFont;
use blinc_text::{FontFace, FontRegistry, GenericFont, LayoutOptions, TextLayoutEngine};
use std::sync::{Arc, Mutex};

/// Convert from layout's GenericFont to text's GenericFont
fn to_text_generic_font(layout_font: LayoutGenericFont) -> GenericFont {
    match layout_font {
        LayoutGenericFont::System => GenericFont::System,
        LayoutGenericFont::Monospace => GenericFont::Monospace,
        LayoutGenericFont::Serif => GenericFont::Serif,
        LayoutGenericFont::SansSerif => GenericFont::SansSerif,
    }
}

/// A text measurer that uses actual font metrics
///
/// This measurer uses the same font loading logic as the renderer
/// to provide accurate text dimensions for layout.
pub struct FontTextMeasurer {
    /// The font face to use for measurement (default/sans-serif)
    font: Arc<Mutex<Option<FontFace>>>,
    /// Font registry for loading different font families
    font_registry: Arc<Mutex<FontRegistry>>,
    /// The layout engine for measuring text
    layout_engine: Mutex<TextLayoutEngine>,
}

impl FontTextMeasurer {
    /// Create a new font text measurer.
    ///
    /// Uses the global shared font registry to minimize memory usage.
    /// Apple Color Emoji alone is 180MB - sharing prevents loading it multiple times.
    pub fn new() -> Self {
        let mut measurer = Self {
            font: Arc::new(Mutex::new(None)),
            font_registry: blinc_text::global_font_registry(),
            layout_engine: Mutex::new(TextLayoutEngine::new()),
        };
        measurer.load_system_font();
        measurer
    }

    /// Create a font text measurer with a shared font registry
    ///
    /// Use this to share the font registry with the text renderer,
    /// ensuring consistent font loading and metrics between measurement
    /// and rendering.
    pub fn with_shared_registry(font_registry: Arc<Mutex<FontRegistry>>) -> Self {
        // Note: system font loading is skipped since the registry is shared
        // and should already be initialized by the renderer
        Self {
            font: Arc::new(Mutex::new(None)),
            font_registry,
            layout_engine: Mutex::new(TextLayoutEngine::new()),
        }
    }

    /// Load the system default font
    fn load_system_font(&mut self) {
        for font_path in crate::system_font_paths() {
            let path = std::path::Path::new(font_path);
            if path.exists() {
                if let Ok(data) = std::fs::read(path) {
                    if let Ok(font) = FontFace::from_data(data) {
                        *self.font.lock().unwrap() = Some(font);
                        break;
                    }
                }
            }
        }
    }

    /// Load a custom font from data
    pub fn load_font_data(&self, data: Vec<u8>) -> Result<(), blinc_text::TextError> {
        let font = FontFace::from_data(data)?;
        *self.font.lock().unwrap() = Some(font);
        Ok(())
    }

    /// Fallback estimation when no font is loaded
    fn estimate_size(text: &str, font_size: f32, options: &TextLayoutOptions) -> TextMetrics {
        let char_count = text.chars().count() as f32;
        let word_count = text.split_whitespace().count().max(1) as f32;

        // Base width: ~0.55 * font_size per character
        let base_char_width = font_size * 0.55;
        let base_width = char_count * base_char_width;

        // Add letter spacing
        let letter_spacing_total = if char_count > 1.0 {
            (char_count - 1.0) * options.letter_spacing
        } else {
            0.0
        };

        // Add word spacing
        let word_spacing_total = if word_count > 1.0 {
            (word_count - 1.0) * options.word_spacing
        } else {
            0.0
        };

        let total_width = base_width + letter_spacing_total + word_spacing_total;

        // Handle wrapping
        let (width, line_count) = if let Some(max_width) = options.max_width {
            if total_width > max_width && max_width > 0.0 {
                let lines = (total_width / max_width).ceil() as u32;
                (max_width, lines.max(1))
            } else {
                (total_width, 1)
            }
        } else {
            (total_width, 1)
        };

        let line_height_px = font_size * options.line_height;
        let height = line_height_px * line_count as f32;

        TextMetrics {
            width,
            height,
            ascender: font_size * 0.8,
            descender: font_size * -0.2,
            line_count,
        }
    }
}

impl Default for FontTextMeasurer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextMeasurer for FontTextMeasurer {
    fn measure_with_options(
        &self,
        text: &str,
        font_size: f32,
        options: &TextLayoutOptions,
    ) -> TextMetrics {
        // Determine which font to use based on options
        let generic_font = to_text_generic_font(options.generic_font);

        // Fast path: use cached fonts only (never load during measurement)
        // Use weight and italic from options to get the correct font variant
        let registry = self.font_registry.lock().unwrap();
        let font = match registry.get_for_render_with_style(
            options.font_name.as_deref(),
            generic_font,
            options.font_weight,
            options.italic,
        ) {
            Some(f) => f,
            None => return Self::estimate_size(text, font_size, options),
        };
        drop(registry); // Release lock before layout

        // Convert our options to blinc_text options.
        //
        // `max_width: Some(0.0)` is how `blinc_layout::tree::text_measure_function`
        // propagates taffy's `AvailableSpace::MinContent` to us — it's requesting
        // the min-content width, which per CSS is the width of the longest
        // unbreakable sequence (typically the longest word). If we forward `0.0`
        // as-is to `blinc_text`'s layout engine with `LineBreakMode::Word`, no
        // word fits in zero pixels and the engine falls back to character-level
        // wrapping, reporting a height of chars × line_height. Taffy's flex-col
        // then treats that inflated height as the item's hypothetical main size,
        // pushes the container into multi-line wrap, and clamps the container's
        // main-axis to `max(content, available)` — which at fullscreen makes
        // `h_fit` cards balloon to the viewport height.
        //
        // Clamp any non-positive `max_width` to the min-content width
        // (measured as the width of the layout with no wrap) so the engine
        // keeps words whole.
        let effective_max_width = match options.max_width {
            Some(mw) if mw > 0.0 => Some(mw),
            Some(_) => {
                // min-content: measure without wrapping to get the natural
                // width of the longest unbreakable run, then layout at that.
                let probe = LayoutOptions {
                    line_height: options.line_height,
                    letter_spacing: options.letter_spacing,
                    max_width: None,
                    line_break: blinc_text::LineBreakMode::None,
                    ..LayoutOptions::default()
                };
                let probe_layout = self
                    .layout_engine
                    .lock()
                    .unwrap()
                    .layout(text, &font, font_size, &probe);
                // Use the widest word (by splitting on whitespace) as the
                // practical min-content width. Fall back to total width if the
                // text has no whitespace.
                let word_widths: Vec<f32> = text
                    .split_whitespace()
                    .map(|word| {
                        self.layout_engine
                            .lock()
                            .unwrap()
                            .layout(word, &font, font_size, &probe)
                            .width
                    })
                    .collect();
                let longest_word = word_widths.into_iter().fold(0.0_f32, f32::max);
                Some(longest_word.max(1.0).min(probe_layout.width.max(1.0)))
            }
            None => None,
        };
        let line_break = if effective_max_width.is_some() {
            blinc_text::LineBreakMode::Word
        } else {
            blinc_text::LineBreakMode::None
        };
        let layout_opts = LayoutOptions {
            line_height: options.line_height,
            letter_spacing: options.letter_spacing,
            max_width: effective_max_width,
            line_break,
            ..LayoutOptions::default()
        };

        let layout_engine = self.layout_engine.lock().unwrap();
        let layout = layout_engine.layout(text, &font, font_size, &layout_opts);

        // Get font metrics
        let metrics = font.metrics();
        let ascender = metrics.ascender_px(font_size);
        let descender = metrics.descender_px(font_size);

        TextMetrics {
            width: layout.width,
            height: layout.height,
            ascender,
            descender,
            line_count: layout.lines.len() as u32,
        }
    }
}

/// Initialize the global text measurer with font support
///
/// Call this at application startup to enable accurate text measurement.
/// This should be called before any UI elements are created.
///
/// Note: For optimal text rendering, use `init_text_measurer_with_registry`
/// to share the font registry with the text renderer.
pub fn init_text_measurer() {
    let measurer = Arc::new(FontTextMeasurer::new());
    blinc_layout::set_text_measurer(measurer);
}

/// Initialize the global text measurer with a shared font registry
///
/// This ensures the text measurer uses the same fonts as the renderer,
/// providing accurate text measurement that matches rendered text exactly.
///
/// Call this after creating the BlincApp/TextRenderingContext:
///
/// ```ignore
/// let (app, surface) = BlincApp::with_window(window, None)?;
/// init_text_measurer_with_registry(app.font_registry());
/// ```
pub fn init_text_measurer_with_registry(font_registry: Arc<Mutex<FontRegistry>>) {
    let measurer = Arc::new(FontTextMeasurer::with_shared_registry(font_registry));
    blinc_layout::set_text_measurer(measurer);
}
