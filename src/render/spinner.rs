//! Tiny, time-based braille spinner for loading indicators.
//!
//! `glyph()` returns one of 10 braille frames based on wall-clock time, so
//! the spinner advances every ~100ms regardless of how often callers redraw.
//! As long as the UI loop redraws at least every 100ms (it does — event
//! poll timeout is 100ms), the user sees a smooth animation.

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const FRAME_MS: u128 = 100;

pub fn glyph() -> &'static str {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    FRAMES[((ms / FRAME_MS) as usize) % FRAMES.len()]
}

/// True when `status` looks like an in-progress operation (we use a trailing
/// `…` as the convention).
pub fn looks_in_progress(status: &str) -> bool {
    status.ends_with('…')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_returns_one_of_known_frames() {
        let g = glyph();
        assert!(FRAMES.contains(&g));
    }

    #[test]
    fn looks_in_progress_matches_trailing_ellipsis() {
        assert!(looks_in_progress("loading…"));
        assert!(looks_in_progress("merging #482 (squash)…"));
        assert!(!looks_in_progress("merged #482"));
        assert!(!looks_in_progress(""));
        assert!(!looks_in_progress("refresh failed: foo"));
    }
}
