//! Catppuccin Mocha named colors used across the UI.
//! https://github.com/catppuccin/catppuccin

use ratatui::style::Color;

// Surfaces
pub const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
pub const SURFACE0: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const SURFACE1: Color = Color::Rgb(0x45, 0x47, 0x5a);
pub const SURFACE2: Color = Color::Rgb(0x58, 0x5b, 0x70);
pub const OVERLAY0: Color = Color::Rgb(0x6c, 0x70, 0x86);
pub const OVERLAY1: Color = Color::Rgb(0x7f, 0x84, 0x9c);

// Text
pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);

// Diff
pub const DIFF_ADD_FG: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const DIFF_ADD_BG: Color = Color::Rgb(0x1f, 0x2a, 0x1f);
pub const DIFF_DEL_FG: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const DIFF_DEL_BG: Color = Color::Rgb(0x2a, 0x1f, 0x23);

// Commit slot palette — fixed order, oldest in window = slot 0.
// Green and red are deliberately absent.
pub const COMMIT_PALETTE: [Color; 7] = [
    Color::Rgb(0x89, 0xb4, 0xfa), // 1 blue
    Color::Rgb(0xcb, 0xa6, 0xf7), // 2 mauve
    Color::Rgb(0xfa, 0xb3, 0x87), // 3 peach
    Color::Rgb(0x94, 0xe2, 0xd5), // 4 teal
    Color::Rgb(0xf9, 0xe2, 0xaf), // 5 yellow
    Color::Rgb(0xf5, 0xc2, 0xe7), // 6 pink
    Color::Rgb(0x74, 0xc7, 0xec), // 7 sapphire
];

// Used for commits that fall outside the window.
pub const OLDER_COMMIT: Color = SURFACE2;

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb_distance(a: Color, b: Color) -> f64 {
        match (a, b) {
            (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
                let dr = ar as i32 - br as i32;
                let dg = ag as i32 - bg as i32;
                let db = ab as i32 - bb as i32;
                ((dr * dr + dg * dg + db * db) as f64).sqrt()
            }
            _ => f64::MAX,
        }
    }

    #[test]
    fn commit_palette_distinguishable_from_diff_colors() {
        // Each commit color must be perceptually distinct from the diff
        // add (green) and remove (red) foreground colors. RGB-distance is a
        // crude proxy but works for the small, hand-picked palette.
        for c in COMMIT_PALETTE.iter() {
            assert!(
                rgb_distance(*c, DIFF_ADD_FG) > 40.0,
                "commit color {:?} too close to DIFF_ADD_FG",
                c,
            );
            assert!(
                rgb_distance(*c, DIFF_DEL_FG) > 40.0,
                "commit color {:?} too close to DIFF_DEL_FG",
                c,
            );
        }
    }

    #[test]
    fn palette_size_matches_default_window() {
        assert_eq!(COMMIT_PALETTE.len(), 7);
    }
}
