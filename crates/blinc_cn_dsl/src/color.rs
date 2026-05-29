//! Colour-prop helpers — shared parsing for every `cn.*` widget that
//! exposes a colour override via a string-hex prop.
//!
//! ## Why string-hex, not packed `i32`?
//!
//! Three reasons:
//!
//! * Empty string is an unambiguous "no override" sentinel. The
//!   alternative — packed `i32` with `0` as the sentinel — would
//!   collide with explicit black (`0x000000`), forcing every black
//!   colour to be written as `0x000001`. Awkward, surprising,
//!   error-prone in code review.
//! * The DSL `i32` literal can already encode hex (`0xFF0000`), but
//!   strings let users write either form (`"#FF0000"`, `"FF0000"`,
//!   `"0xFF0000"`, `"#F00"`) without macro acrobatics.
//! * Runtime parsing cost is one trim + one `u32::from_str_radix`
//!   per widget construction. Cn widgets aren't on a hot path.
//!
//! Trade: signal-bound colour (`cn.Button(color = my_signal)`)
//! isn't supported by this surface. Signals go through the
//! `lower_styling_args_to_overlays` pass which only handles
//! bare-Variable references to typed signals on built-in Div-like
//! prop slots, not on `cn.*` extern widgets. Closing that gap is
//! a follow-up; this commit ships the literal-colour path.

use blinc_core::layer::Color;

/// Parse a hex colour string. Returns `None` for empty input (the
/// "no override" sentinel) or for unparseable shapes.
///
/// Accepted shapes (case-insensitive):
///   * `""` — no override.
///   * `"#RGB"` — three-digit shorthand, expanded `R→RR`, `G→GG`,
///     `B→BB`.
///   * `"#RRGGBB"` — six-digit RGB.
///   * `"RGB"` / `"RRGGBB"` — leading `#` is optional.
///   * `"0xRRGGBB"` — explicit hex prefix.
///
/// Unrecognised inputs emit a `tracing::warn!` and return `None`
/// so the widget falls back to its cn-side default without
/// crashing the render.
pub(crate) fn parse_color_prop(widget_name: &str, prop_name: &str, value: &str) -> Option<Color> {
    if value.is_empty() {
        return None;
    }
    // Strip optional `#` then optional `0x`. Both are common; tolerate either.
    let body = value
        .strip_prefix('#')
        .unwrap_or(value)
        .strip_prefix("0x")
        .or_else(|| {
            // `"#0xFFFFFF"` would land here with the leading `#`
            // already stripped. Strip `0x` again as a second pass.
            value.strip_prefix('#').and_then(|s| s.strip_prefix("0x"))
        })
        .unwrap_or_else(|| value.strip_prefix('#').unwrap_or(value));

    let hex = match body.len() {
        3 => {
            // Short form: "RGB" → 0xRRGGBB by doubling each nibble.
            let chars: Vec<char> = body.chars().collect();
            let r = u8::from_str_radix(&format!("{0}{0}", chars[0]), 16).ok()?;
            let g = u8::from_str_radix(&format!("{0}{0}", chars[1]), 16).ok()?;
            let b = u8::from_str_radix(&format!("{0}{0}", chars[2]), 16).ok()?;
            ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
        }
        6 => match u32::from_str_radix(body, 16) {
            Ok(h) => h,
            Err(_) => {
                tracing::warn!(
                    widget = widget_name,
                    prop = prop_name,
                    value = value,
                    "cn.* colour prop: not a valid hex string — using default",
                );
                return None;
            }
        },
        _ => {
            tracing::warn!(
                widget = widget_name,
                prop = prop_name,
                value = value,
                "cn.* colour prop: expected #RGB or #RRGGBB shape — using default",
            );
            return None;
        }
    };
    Some(Color::from_hex(hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_none() {
        assert!(parse_color_prop("w", "p", "").is_none());
    }

    #[test]
    fn six_digit_hex_with_hash() {
        let c = parse_color_prop("w", "p", "#FF0000").expect("parsed");
        assert!((c.r - 1.0).abs() < 1e-6);
        assert!(c.g.abs() < 1e-6);
        assert!(c.b.abs() < 1e-6);
    }

    #[test]
    fn six_digit_hex_without_hash() {
        let c = parse_color_prop("w", "p", "00FF00").expect("parsed");
        assert!(c.r.abs() < 1e-6);
        assert!((c.g - 1.0).abs() < 1e-6);
        assert!(c.b.abs() < 1e-6);
    }

    #[test]
    fn six_digit_hex_with_0x() {
        let c = parse_color_prop("w", "p", "0x0000FF").expect("parsed");
        assert!(c.r.abs() < 1e-6);
        assert!(c.g.abs() < 1e-6);
        assert!((c.b - 1.0).abs() < 1e-6);
    }

    #[test]
    fn three_digit_shorthand() {
        // #F00 → 0xFF0000
        let c = parse_color_prop("w", "p", "#F00").expect("parsed");
        assert!((c.r - 1.0).abs() < 1e-6);
        assert!(c.g.abs() < 1e-6);
        assert!(c.b.abs() < 1e-6);
    }

    #[test]
    fn explicit_black_is_supported() {
        // #000000 must NOT be treated as the "no override" sentinel —
        // that's the whole point of using empty-string as the sentinel
        // and not packed `i32 == 0`.
        let c = parse_color_prop("w", "p", "#000000").expect("explicit black parses");
        assert!(c.r.abs() < 1e-6);
        assert!(c.g.abs() < 1e-6);
        assert!(c.b.abs() < 1e-6);
    }

    #[test]
    fn garbage_returns_none() {
        assert!(parse_color_prop("w", "p", "not-a-color").is_none());
        assert!(parse_color_prop("w", "p", "#12").is_none());
        assert!(parse_color_prop("w", "p", "#1234567").is_none());
        assert!(parse_color_prop("w", "p", "#GGGGGG").is_none());
    }
}
