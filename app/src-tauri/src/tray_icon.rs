//! Drawing the number into the tray icon.
//!
//! macOS lets a status item carry a text title next to its icon, so on macOS the
//! readout is real text and this module only draws the small severity glyph.
//! Windows has no such thing — `Shell_NotifyIcon` takes an HICON and a tooltip,
//! full stop — so if the pace ratio is going to be visible on Windows at all, it
//! has to be *painted into* the icon.
//!
//! Hence the hand-rolled 3x5 bitmap font below. It looks like an odd thing to
//! carry until you try the alternatives: a real font rasteriser is a megabyte of
//! dependency and still renders mush at 16 physical pixels, whereas a bitmap
//! font designed for the size is crisp by construction.

/// 3x5 glyphs, one byte per row, low three bits, MSB-first within those bits.
/// Only the characters the readouts can actually produce are here; anything else
/// falls back to a blank cell rather than panicking.
fn glyph(c: char) -> Option<[u8; 5]> {
    Some(match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        '%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        'K' => [0b101, 0b110, 0b100, 0b110, 0b101],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'h' => [0b100, 0b100, 0b110, 0b101, 0b101],
        'm' => [0b000, 0b000, 0b111, 0b111, 0b101],
        'd' => [0b001, 0b001, 0b111, 0b101, 0b111],
        's' => [0b000, 0b011, 0b110, 0b011, 0b110],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        // The multiplication sign renders as a lowercase x at this size.
        '\u{d7}' | 'x' => [0b000, 0b101, 0b010, 0b101, 0b000],
        '-' | '\u{2014}' => [0b000, 0b000, 0b111, 0b000, 0b000],
        ' ' => [0, 0, 0, 0, 0],
        _ => return None,
    })
}

const GLYPH_W: usize = 3;
const GLYPH_H: usize = 5;

/// Height of the fill bar along the bottom edge. A floor of 2 keeps it visible
/// on a 16px icon, where an eighth would round down to nothing useful.
fn bar_height(size: usize) -> usize {
    (size / 8).max(2)
}

pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Paint `text` into a `size`x`size` RGBA icon, scaled to fit, plus a fill bar
/// along the bottom edge showing `percent`.
///
/// `percent` is drawn even when the text is unreadably small, because a bar is
/// legible at any size and gives the icon a usable meaning on a 16px display.
pub fn render(text: &str, rgb: (u8, u8, u8), percent: f64, size: u32) -> Rgba {
    let size_us = size as usize;
    let mut bytes = vec![0u8; size_us * size_us * 4];

    let mut glyphs: Vec<[u8; 5]> = text.chars().filter_map(glyph).collect();
    let bar_h = bar_height(size_us);

    if !glyphs.is_empty() {
        let text_area_h = size_us.saturating_sub(bar_h + 2);
        // Width of `n` glyphs at `s` pixels per cell: one gap between each pair,
        // none trailing.
        let width = |n: usize, s: usize| (n * (GLYPH_W + 1) - 1) * s;

        // Largest integer scale where the whole string fits.
        let scale = (1..=8)
            .rev()
            .find(|s| width(glyphs.len(), *s) <= size_us && GLYPH_H * s <= text_area_h)
            .unwrap_or(1);

        // Even at 1:1 a long readout can overrun a 16px icon — "418K/h" needs
        // 23. Drop trailing glyphs until it fits, so the result is a decided
        // truncation ("418K") rather than whatever happens to fall off the edge.
        // `put` clips silently, so without this the overflow is invisible here
        // and only shows up on someone's Windows taskbar.
        while glyphs.len() > 1 && width(glyphs.len(), scale) > size_us {
            glyphs.pop();
        }

        let cells_w = glyphs.len() * (GLYPH_W + 1) - 1;

        let draw_w = cells_w * scale;
        let draw_h = GLYPH_H * scale;
        let x0 = (size_us.saturating_sub(draw_w)) / 2;
        let y0 = (text_area_h.saturating_sub(draw_h)) / 2;

        for (index, g) in glyphs.iter().enumerate() {
            let gx = x0 + index * (GLYPH_W + 1) * scale;
            for (row, bits) in g.iter().enumerate() {
                for col in 0..GLYPH_W {
                    if bits & (1 << (GLYPH_W - 1 - col)) == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let x = gx + col * scale + dx;
                            let y = y0 + row * scale + dy;
                            put(&mut bytes, size_us, x, y, rgb, 255);
                        }
                    }
                }
            }
        }
    }

    // Fill bar: a dim full-width track with a bright portion for `percent`.
    let filled = ((percent.clamp(0.0, 100.0) / 100.0) * size as f64).round() as usize;
    for y in size_us - bar_h..size_us {
        for x in 0..size_us {
            let alpha = if x < filled { 255 } else { 70 };
            put(&mut bytes, size_us, x, y, rgb, alpha);
        }
    }

    Rgba {
        width: size,
        height: size,
        bytes,
    }
}

/// A hollow ring, for "no data yet".
pub fn placeholder(rgb: (u8, u8, u8), size: u32) -> Rgba {
    let size_us = size as usize;
    let mut bytes = vec![0u8; size_us * size_us * 4];
    let centre = (size as f64 - 1.0) / 2.0;
    let outer = size as f64 * 0.42;
    let inner = outer - (size as f64 * 0.12).max(1.5);

    for y in 0..size_us {
        for x in 0..size_us {
            let dx = x as f64 - centre;
            let dy = y as f64 - centre;
            let d = (dx * dx + dy * dy).sqrt();
            if d <= outer && d >= inner {
                put(&mut bytes, size_us, x, y, rgb, 200);
            }
        }
    }
    Rgba {
        width: size,
        height: size,
        bytes,
    }
}

fn put(bytes: &mut [u8], size: usize, x: usize, y: usize, rgb: (u8, u8, u8), alpha: u8) {
    if x >= size || y >= size {
        return;
    }
    let i = (y * size + x) * 4;
    bytes[i] = rgb.0;
    bytes[i + 1] = rgb.1;
    bytes[i + 2] = rgb.2;
    bytes[i + 3] = alpha;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write the icons out so a human can look at them:
    ///
    /// ```sh
    /// cargo test -p runway-app -- --ignored dump_previews
    /// ```
    ///
    /// Ignored because it's an inspection aid, not an assertion. It exists
    /// because the hard question about this module — is the number *readable*
    /// at the size Windows actually draws it — can't be answered by a test, and
    /// waiting for someone with a Windows machine to send a screenshot is a
    /// slow way to find out. Each preview is composited on the tray greys and
    /// magnified with nearest-neighbour, so one image pixel is one icon pixel.
    #[test]
    #[ignore]
    fn dump_previews() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tray-previews");
        std::fs::create_dir_all(&dir).unwrap();

        for (name, text) in [
            ("pace", "2.3\u{d7}"),
            ("allowance", "418K/h"),
            ("percent", "42%"),
        ] {
            for size in [16u32, 32] {
                let icon = render(text, (61, 173, 115), 62.0, size);
                for (bg_name, bg) in [("dark", 40u8), ("light", 235u8)] {
                    let path = dir.join(format!("{name}-{size}px-{bg_name}.ppm"));
                    std::fs::write(&path, to_ppm(&icon, bg, 8)).unwrap();
                }
            }
        }
        eprintln!("wrote previews to {}", dir.display());
    }

    /// Composite over an opaque background and magnify, as a binary PPM — no
    /// image crate needed for something only a human ever opens.
    fn to_ppm(icon: &Rgba, background: u8, zoom: u32) -> Vec<u8> {
        let (w, h) = (icon.width * zoom, icon.height * zoom);
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for y in 0..h {
            for x in 0..w {
                let i = (((y / zoom) * icon.width + (x / zoom)) * 4) as usize;
                let alpha = icon.bytes[i + 3] as f32 / 255.0;
                for channel in 0..3 {
                    let fg = icon.bytes[i + channel] as f32;
                    out.push((fg * alpha + background as f32 * (1.0 - alpha)) as u8);
                }
            }
        }
        out
    }

    fn opaque_pixels(icon: &Rgba) -> usize {
        icon.bytes.chunks(4).filter(|p| p[3] == 255).count()
    }

    #[test]
    fn every_character_the_readouts_emit_has_a_glyph() {
        // These are exactly the strings `menu_bar_text` can produce.
        for text in [
            "1.8\u{d7}",
            "418K/h",
            "2.4M/h",
            "42%",
            "4.3%",
            "2h 14m",
            "31s",
            "3d 6h",
            "\u{2014}",
        ] {
            for c in text.chars() {
                assert!(glyph(c).is_some(), "no glyph for {c:?} in {text:?}");
            }
        }
    }

    /// A readout too long for the icon must be truncated deliberately, not
    /// clipped by `put` falling off the right edge where nobody can see it.
    #[test]
    fn long_readouts_are_truncated_not_clipped() {
        // "418K/h" needs 23px of the 16 available.
        let long = render("418K/h", (255, 255, 255), 0.0, 16);
        let short = render("418K", (255, 255, 255), 0.0, 16);
        assert_eq!(
            opaque_pixels(&long),
            opaque_pixels(&short),
            "the overflowing suffix should be dropped, leaving exactly '418K'"
        );

        // Nothing may be drawn in the rightmost column beyond the fill bar.
        let bar_h = bar_height(16);
        for y in 0..16 - bar_h {
            let i = (y * 16 + 15) * 4 + 3;
            assert_eq!(long.bytes[i], 0, "text reached the edge at row {y}");
        }
    }

    #[test]
    fn unknown_characters_are_skipped_not_fatal() {
        let icon = render("\u{4e2d}\u{6587}", (255, 255, 255), 0.0, 32);
        assert_eq!(icon.bytes.len(), 32 * 32 * 4);
    }

    #[test]
    fn text_is_drawn_and_scaled_to_the_icon() {
        let small = render("1.8\u{d7}", (255, 255, 255), 0.0, 16);
        let large = render("1.8\u{d7}", (255, 255, 255), 0.0, 32);
        assert!(opaque_pixels(&small) > 0, "nothing drawn at 16px");
        // Same string at double the size must use meaningfully more ink.
        assert!(
            opaque_pixels(&large) > opaque_pixels(&small) * 2,
            "{} vs {}",
            opaque_pixels(&large),
            opaque_pixels(&small)
        );
    }

    #[test]
    fn the_fill_bar_tracks_percent() {
        let empty = render("", (255, 255, 255), 0.0, 32);
        let half = render("", (255, 255, 255), 50.0, 32);
        let full = render("", (255, 255, 255), 100.0, 32);
        assert_eq!(opaque_pixels(&empty), 0);
        assert!(opaque_pixels(&half) > 0);
        assert!((opaque_pixels(&full) as f64 / opaque_pixels(&half) as f64 - 2.0).abs() < 0.2);
    }

    #[test]
    fn percent_outside_the_range_cannot_overflow_the_icon() {
        for p in [-50.0, 0.0, 100.0, 500.0, f64::NAN] {
            let icon = render("9", (1, 2, 3), p, 16);
            assert_eq!(icon.bytes.len(), 16 * 16 * 4);
        }
    }

    #[test]
    fn placeholder_draws_a_ring() {
        let icon = placeholder((200, 200, 200), 32);
        let lit = icon.bytes.chunks(4).filter(|p| p[3] > 0).count();
        assert!(
            lit > 20 && lit < 32 * 32 / 2,
            "ring should be a ring, got {lit} px"
        );
        // The centre must be hollow.
        let centre = (16 * 32 + 16) * 4;
        assert_eq!(icon.bytes[centre + 3], 0);
    }
}
