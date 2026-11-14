use std::ops::Range;
use gpui::{
    div, App, Bounds, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, Pixels, Rgba, Styled, WeakEntity, px,
};
use multi_buffer::{Anchor, MultiBufferOffset, MultiBufferRow, MultiBufferSnapshot};
use ui::{
    h_flex, v_flex, ActiveTheme, Button, ButtonCommon, ButtonSize, ButtonStyle, Clickable,
    IconButton, IconName, IconSize, Label, LabelCommon, LabelSize, StyledExt, Tooltip,
};

use crate::Editor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorFormat {
    Hex6,
    Hex8,
    Rgb,
    Rgba,
    Hsl,
    Hsla,
}

#[derive(Debug, Clone)]
pub struct ColorPickerState {
    pub range: Range<Anchor>,
    pub original_text: String,
    pub original_color: Hsla,
    pub current_color: Hsla,
    pub format: ColorFormat,
    pub anchor: Anchor,
    pub target_position: Option<gpui::Point<Pixels>>,
    pub bounds: Option<Bounds<Pixels>>,
}

impl ColorPickerState {
    pub fn new(
        range: Range<Anchor>,
        original_text: String,
        color: Hsla,
        format: ColorFormat,
        anchor: Anchor,
        target_position: Option<gpui::Point<Pixels>>,
    ) -> Self {
        Self {
            range,
            original_text,
            original_color: color,
            current_color: color,
            format,
            anchor,
            target_position,
            bounds: None,
        }
    }

    pub fn format_current_color(&self) -> String {
        format_color(&self.current_color, self.format)
    }
}

/// Formats an HSLA color into the specified text format.
pub fn format_color(color: &Hsla, format: ColorFormat) -> String {
    let rgba = color.to_rgb();
    let r = (rgba.r * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (rgba.g * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (rgba.b * 255.0).round().clamp(0.0, 255.0) as u8;
    let a = rgba.a.clamp(0.0, 1.0);

    match format {
        ColorFormat::Hex6 => format!("#{:02x}{:02x}{:02x}", r, g, b),
        ColorFormat::Hex8 => {
            let a_u8 = (a * 255.0).round().clamp(0.0, 255.0) as u8;
            format!("#{:02x}{:02x}{:02x}{:02x}", r, g, b, a_u8)
        }
        ColorFormat::Rgb => format!("rgb({}, {}, {})", r, g, b),
        ColorFormat::Rgba => format!("rgba({}, {}, {}, {:.2})", r, g, b, a),
        ColorFormat::Hsl => {
            let h = (color.h * 360.0).round() as u16;
            let s = (color.s * 100.0).round() as u8;
            let l = (color.l * 100.0).round() as u8;
            format!("hsl({}, {}%, {}%)", h, s, l)
        }
        ColorFormat::Hsla => {
            let h = (color.h * 360.0).round() as u16;
            let s = (color.s * 100.0).round() as u8;
            let l = (color.l * 100.0).round() as u8;
            format!("hsla({}, {}%, {}%, {:.2})", h, s, l, a)
        }
    }
}

/// Parses color text (hex, rgb, rgba, hsl, hsla) into (Hsla, ColorFormat).
pub fn parse_color_str(text: &str) -> Option<(Hsla, ColorFormat)> {
    let s = text.trim();
    if s.starts_with('#') {
        let hex = s.strip_prefix('#')?;
        match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                let rgba = Rgba {
                    r: r as f32 / 255.0,
                    g: g as f32 / 255.0,
                    b: b as f32 / 255.0,
                    a: 1.0,
                };
                Some((rgba.into(), ColorFormat::Hex6))
            }
            4 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                let a = u8::from_str_radix(&hex[3..4], 16).ok()? * 17;
                let rgba = Rgba {
                    r: r as f32 / 255.0,
                    g: g as f32 / 255.0,
                    b: b as f32 / 255.0,
                    a: a as f32 / 255.0,
                };
                Some((rgba.into(), ColorFormat::Hex8))
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let rgba = Rgba {
                    r: r as f32 / 255.0,
                    g: g as f32 / 255.0,
                    b: b as f32 / 255.0,
                    a: 1.0,
                };
                Some((rgba.into(), ColorFormat::Hex6))
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
                let rgba = Rgba {
                    r: r as f32 / 255.0,
                    g: g as f32 / 255.0,
                    b: b as f32 / 255.0,
                    a: a as f32 / 255.0,
                };
                Some((rgba.into(), ColorFormat::Hex8))
            }
            _ => None,
        }
    } else if s.starts_with("rgb(") && s.ends_with(')') {
        let inner = &s[4..s.len() - 1];
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() == 3 {
            let r: f32 = parts[0].parse().ok()?;
            let g: f32 = parts[1].parse().ok()?;
            let b: f32 = parts[2].parse().ok()?;
            let rgba = Rgba {
                r: r / 255.0,
                g: g / 255.0,
                b: b / 255.0,
                a: 1.0,
            };
            Some((rgba.into(), ColorFormat::Rgb))
        } else {
            None
        }
    } else if s.starts_with("rgba(") && s.ends_with(')') {
        let inner = &s[5..s.len() - 1];
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() == 4 {
            let r: f32 = parts[0].parse().ok()?;
            let g: f32 = parts[1].parse().ok()?;
            let b: f32 = parts[2].parse().ok()?;
            let a: f32 = parts[3].parse().ok()?;
            let rgba = Rgba {
                r: r / 255.0,
                g: g / 255.0,
                b: b / 255.0,
                a,
            };
            Some((rgba.into(), ColorFormat::Rgba))
        } else {
            None
        }
    } else {
        None
    }
}

/// Popular curated web/PhpStorm palette swatches.
pub const PALETTE_SWATCHES: [&str; 18] = [
    "#ffffff", "#000000", "#e2e8f0", "#94a3b8", "#64748b",
    "#ef4444", "#f97316", "#f59e0b", "#eab308", "#84cc16",
    "#22c55e", "#10b981", "#06b6d4", "#0ea5e9", "#3574f0",
    "#6366f1", "#8b5cf6", "#ec4899",
];

/// Finds a color token around a byte offset in the multibuffer snapshot.
pub fn find_color_around_offset(
    snapshot: &MultiBufferSnapshot,
    offset: MultiBufferOffset,
) -> Option<(Range<Anchor>, String, Hsla, ColorFormat)> {
    let point = snapshot.offset_to_point(offset);
    let row_start_point = language::Point::new(point.row, 0);
    let row_start = snapshot.point_to_offset(row_start_point);
    let row_len = snapshot.line_len(MultiBufferRow(point.row)) as usize;
    let row_end = MultiBufferOffset(row_start.0 + row_len);

    let start_anchor = snapshot.anchor_after(row_start);
    let end_anchor = snapshot.anchor_after(row_end);
    let row_text: String = snapshot.text_for_range(start_anchor..end_anchor).collect();
    let col = offset.0.saturating_sub(row_start.0);

    // Look for hex string (#...)
    for (i, _) in row_text.match_indices('#') {
        let rest = &row_text[i..];
        let hex_len = rest
            .chars()
            .skip(1)
            .take_while(|c| c.is_ascii_hexdigit())
            .count();
        if matches!(hex_len, 3 | 4 | 6 | 8) {
            let token_end = i + 1 + hex_len;
            if col >= i && col <= token_end {
                let token = &row_text[i..token_end];
                if let Some((hsla, format)) = parse_color_str(token) {
                    let token_start_anchor = snapshot.anchor_after(MultiBufferOffset(row_start.0 + i));
                    let token_end_anchor = snapshot.anchor_after(MultiBufferOffset(row_start.0 + token_end));
                    return Some((
                        token_start_anchor..token_end_anchor,
                        token.to_string(),
                        hsla,
                        format,
                    ));
                }
            }
        }
    }

    // Look for rgb(...) or rgba(...)
    for prefix in ["rgb(", "rgba("] {
        for (i, _) in row_text.match_indices(prefix) {
            if let Some(close) = row_text[i..].find(')') {
                let token_end = i + close + 1;
                if col >= i && col <= token_end {
                    let token = &row_text[i..token_end];
                    if let Some((hsla, format)) = parse_color_str(token) {
                        let token_start_anchor = snapshot.anchor_after(MultiBufferOffset(row_start.0 + i));
                        let token_end_anchor = snapshot.anchor_after(MultiBufferOffset(row_start.0 + token_end));
                        return Some((
                            token_start_anchor..token_end_anchor,
                            token.to_string(),
                            hsla,
                            format,
                        ));
                    }
                }
            }
        }
    }

    None
}

/// Renders the interactive Color Picker popover element.
pub fn render_color_picker(
    state: &ColorPickerState,
    editor: WeakEntity<Editor>,
    cx: &mut App,
) -> impl IntoElement {
    let current_hsla = state.current_color;
    let format_label = match state.format {
        ColorFormat::Hex6 | ColorFormat::Hex8 => "HEX",
        ColorFormat::Rgb | ColorFormat::Rgba => "RGB",
        ColorFormat::Hsl | ColorFormat::Hsla => "HSL",
    };
    let formatted_code = state.format_current_color();

    v_flex()
        .elevation_3(cx)
        .w(px(250.0))
        .p_2p5()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border)
        .bg(cx.theme().colors().elevated_surface_background)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .child(
            // Header: Title + Format toggle + Close button
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(
                            div()
                                .size_3()
                                .rounded_full()
                                .bg(current_hsla),
                        )
                        .child(
                            Label::new("Color Picker")
                                .size(LabelSize::Small)
                                .weight(FontWeight::SEMIBOLD),
                        ),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child({
                            let editor = editor.clone();
                            Button::new("color_fmt_toggle", format_label)
                                .size(ButtonSize::Compact)
                                .style(ButtonStyle::Subtle)
                                .tooltip(Tooltip::text("Cycle Color Format (HEX / RGB / HSL)"))
                                .on_click(move |_, window, cx| {
                                    editor
                                        .update(cx, |editor, cx| {
                                            editor.cycle_color_picker_format(window, cx);
                                        })
                                        .ok();
                                })
                        })
                        .child({
                            let editor = editor.clone();
                            IconButton::new("color_picker_close", IconName::Close)
                                .icon_size(IconSize::XSmall)
                                .on_click(move |_, _window, cx| {
                                    editor
                                        .update(cx, |editor, cx| {
                                            editor.hide_color_picker(cx);
                                        })
                                        .ok();
                                })
                        }),
                ),
        )
        // Preview swatch & code display
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .h_7()
                        .flex_1()
                        .rounded_xs()
                        .border_1()
                        .border_color(cx.theme().colors().border_variant)
                        .bg(current_hsla),
                )
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_xs()
                        .bg(cx.theme().colors().surface_background)
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .text_xs()
                        .child(formatted_code),
                ),
        )
        // Hue spectrum steps
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .children([0.0f32, 0.08, 0.16, 0.33, 0.5, 0.6, 0.75, 0.9].into_iter().map(|h| {
                    let swatch = Hsla { h, s: 0.85, l: 0.5, a: 1.0 };
                    let editor = editor.clone();
                    div()
                        .size_4()
                        .rounded_xs()
                        .border_1()
                        .border_color(gpui::black().opacity(0.3))
                        .bg(swatch)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.8))
                        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            editor
                                .update(cx, |editor, cx| {
                                    editor.set_color_picker_color(swatch, window, cx);
                                })
                                .ok();
                        })
                }))
        )
        // Preset Palette Swatches (2 rows of 9 colors)
        .child(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .children(PALETTE_SWATCHES[0..9].iter().map(|hex| {
                            let (color, _) = parse_color_str(hex).unwrap();
                            let editor = editor.clone();
                            div()
                                .size_4()
                                .rounded_xs()
                                .border_1()
                                .border_color(gpui::black().opacity(0.3))
                                .bg(color)
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.8))
                                .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                                    cx.stop_propagation();
                                    editor
                                        .update(cx, |editor, cx| {
                                            editor.set_color_picker_color(color, window, cx);
                                        })
                                        .ok();
                                })
                        }))
                )
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .children(PALETTE_SWATCHES[9..18].iter().map(|hex| {
                            let (color, _) = parse_color_str(hex).unwrap();
                            let editor = editor.clone();
                            div()
                                .size_4()
                                .rounded_xs()
                                .border_1()
                                .border_color(gpui::black().opacity(0.3))
                                .bg(color)
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.8))
                                .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                                    cx.stop_propagation();
                                    editor
                                        .update(cx, |editor, cx| {
                                            editor.set_color_picker_color(color, window, cx);
                                        })
                                        .ok();
                                })
                        }))
                )
        )
        // Shading / adjustments (Darker, Lighter, Done)
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child({
                            let editor = editor.clone();
                            let mut darker = current_hsla;
                            darker.l = (darker.l - 0.1).clamp(0.0, 1.0);
                            Button::new("darker", "Darker")
                                .size(ButtonSize::Compact)
                                .style(ButtonStyle::Subtle)
                                .on_click(move |_, window, cx| {
                                    editor
                                        .update(cx, |editor, cx| {
                                            editor.set_color_picker_color(darker, window, cx);
                                        })
                                        .ok();
                                })
                        })
                        .child({
                            let editor = editor.clone();
                            let mut lighter = current_hsla;
                            lighter.l = (lighter.l + 0.1).clamp(0.0, 1.0);
                            Button::new("lighter", "Lighter")
                                .size(ButtonSize::Compact)
                                .style(ButtonStyle::Subtle)
                                .on_click(move |_, window, cx| {
                                    editor
                                        .update(cx, |editor, cx| {
                                            editor.set_color_picker_color(lighter, window, cx);
                                        })
                                        .ok();
                                })
                        }),
                )
                .child({
                    let editor = editor.clone();
                    Button::new("apply_close", "Done")
                        .size(ButtonSize::Compact)
                        .style(ButtonStyle::Filled)
                        .on_click(move |_, _window, cx| {
                            editor
                                .update(cx, |editor, cx| {
                                    editor.hide_color_picker(cx);
                                })
                                .ok();
                        })
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_and_format_hex() {
        let (color, format) = parse_color_str("#3574f0").expect("parse hex6");
        assert_eq!(format, ColorFormat::Hex6);
        let formatted = format_color(&color, format);
        assert_eq!(formatted.to_lowercase(), "#3574f0");

        let (color, format) = parse_color_str("#fff").expect("parse hex3");
        assert_eq!(format, ColorFormat::Hex6);
        let formatted = format_color(&color, format);
        assert_eq!(formatted, "#ffffff");

        let (color, format) = parse_color_str("#10b98180").expect("parse hex8");
        assert_eq!(format, ColorFormat::Hex8);
        assert!(format_color(&color, format).starts_with("#10b981"));
    }

    #[test]
    fn test_parse_and_format_rgb() {
        let (color, format) = parse_color_str("rgb(255, 0, 128)").expect("parse rgb");
        assert_eq!(format, ColorFormat::Rgb);
        assert_eq!(format_color(&color, format), "rgb(255, 0, 128)");

        let (color, format) = parse_color_str("rgba(53, 116, 240, 0.5)").expect("parse rgba");
        assert_eq!(format, ColorFormat::Rgba);
        assert_eq!(format_color(&color, format), "rgba(53, 116, 240, 0.50)");
    }

    #[test]
    fn test_parse_and_format_hsl() {
        let (color, _) = parse_color_str("#3574f0").unwrap();
        let hsl_text = format_color(&color, ColorFormat::Hsl);
        assert!(hsl_text.starts_with("hsl("));
    }

    #[test]
    fn test_palette_swatches_validity() {
        for swatch in PALETTE_SWATCHES {
            let parsed = parse_color_str(swatch);
            assert!(parsed.is_some(), "palette swatch {} must parse", swatch);
        }
    }
}
