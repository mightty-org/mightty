//! What programs ask of the window around the terminal: a progress bar
//! (ConEmu's OSC 9 ; 4, as Ghostty and Windows Terminal show it), the
//! pointer's shape (OSC 22, as in xterm and kitty), and the pane's font
//! family (OSC 50, as in xterm), as `mightty config` previews fonts.

use std::time::{Duration, Instant};

use crate::Term;

/// A progress bar left without updates this long goes, as in Ghostty: the
/// program that set it may have died.
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Percent done.
    Normal(u8),
    Error(u8),
    Paused(u8),
    /// Busy, for an unknown time.
    Indeterminate,
}

/// Pointer shapes, by their CSS names; X11 names too, as kitty takes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerShape {
    #[default]
    Default,
    Text,
    VerticalText,
    Pointer,
    Crosshair,
    NotAllowed,
    Grab,
    Grabbing,
    ColResize,
    RowResize,
    Copy,
    Alias,
    ContextMenu,
}

impl PointerShape {
    fn from_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"" | b"default" | b"left_ptr" | b"arrow" | b"wait" | b"progress" | b"help"
            | b"move" | b"all-scroll" | b"watch" => Self::Default,
            b"text" | b"xterm" | b"ibeam" => Self::Text,
            b"vertical-text" => Self::VerticalText,
            b"pointer" | b"hand" | b"hand1" | b"hand2" | b"pointing_hand" => Self::Pointer,
            b"crosshair" | b"cell" | b"cross" | b"tcross" => Self::Crosshair,
            b"not-allowed" | b"no-drop" | b"forbidden" | b"crossed_circle" => Self::NotAllowed,
            b"grab" | b"openhand" => Self::Grab,
            b"grabbing" | b"closedhand" => Self::Grabbing,
            b"col-resize" | b"ew-resize" | b"e-resize" | b"w-resize" | b"sb_h_double_arrow" => {
                Self::ColResize
            }
            b"row-resize" | b"ns-resize" | b"n-resize" | b"s-resize" | b"sb_v_double_arrow" => {
                Self::RowResize
            }
            b"copy" | b"dnd-copy" => Self::Copy,
            b"alias" | b"dnd-link" => Self::Alias,
            b"context-menu" => Self::ContextMenu,
            _ => return None,
        })
    }
}

impl Term {
    /// The progress bar to show, if any.
    pub fn progress(&self) -> Option<Progress> {
        self.progress
            .filter(|(_, at)| at.elapsed() < PROGRESS_TIMEOUT)
            .map(|(progress, _)| progress)
    }

    /// When the progress bar goes for want of updates, to draw it gone.
    pub fn progress_expiry(&self) -> Option<Instant> {
        self.progress()
            .and(self.progress.map(|(_, at)| at + PROGRESS_TIMEOUT))
    }

    /// The pointer's shape over the terminal.
    pub fn pointer_shape(&self) -> PointerShape {
        self.pointer_shape
    }

    /// OSC 9 ; 4 ; state [; percent]: 0 hides the bar, 1 shows it, 2 as an
    /// error, 3 busy for an unknown time, 4 paused. Error and paused keep
    /// the percentage when they're given none.
    pub(crate) fn set_progress(&mut self, params: &[&[u8]]) {
        let number = |i: usize| -> Option<u8> {
            std::str::from_utf8(params.get(i)?)
                .ok()?
                .parse::<u32>()
                .ok()
                .map(|n| n.min(100) as u8)
        };
        let last = match self.progress() {
            Some(Progress::Normal(p) | Progress::Error(p) | Progress::Paused(p)) => p,
            _ => 0,
        };
        let progress = match number(0) {
            Some(0) => None,
            Some(1) => Some(Progress::Normal(number(1).unwrap_or(0))),
            Some(2) => Some(Progress::Error(number(1).unwrap_or(last))),
            Some(3) => Some(Progress::Indeterminate),
            Some(4) => Some(Progress::Paused(number(1).unwrap_or(last))),
            _ => return,
        };
        self.progress = progress.map(|p| (p, Instant::now()));
    }

    /// A font family the pane's program asked for (OSC 50): `Some(None)`
    /// to go back to the configured one. Taken once.
    pub fn take_font_request(&mut self) -> Option<Option<String>> {
        self.font_request.take()
    }

    /// OSC 50 ; family: the pane's font, by family name; empty for the
    /// configured one. Queries (`?`) aren't answered.
    pub(crate) fn set_font(&mut self, name: &[u8]) {
        let Ok(name) = std::str::from_utf8(name) else {
            return;
        };
        if name.starts_with('?') || name.len() > 256 || name.chars().any(char::is_control) {
            return;
        }
        self.font_request = Some((!name.is_empty()).then(|| name.to_owned()));
    }

    /// OSC 22 ; name: unknown names leave the shape as it is.
    pub(crate) fn set_pointer_shape(&mut self, name: &[u8]) {
        if let Some(shape) = PointerShape::from_name(name) {
            self.pointer_shape = shape;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress() {
        let mut t = Term::new(5, 1);
        t.feed(b"\x1b]9;4;1;40\x07");
        assert_eq!(t.progress(), Some(Progress::Normal(40)));
        t.feed(b"\x1b]9;4;2\x1b\\");
        assert_eq!(t.progress(), Some(Progress::Error(40)));
        t.feed(b"\x1b]9;4;4;250\x07");
        assert_eq!(t.progress(), Some(Progress::Paused(100)));
        t.feed(b"\x1b]9;4;3\x07");
        assert_eq!(t.progress(), Some(Progress::Indeterminate));
        t.feed(b"\x1b]9;4;7;1\x07");
        assert_eq!(t.progress(), Some(Progress::Indeterminate));
        t.feed(b"\x1b]9;4;0\x07");
        assert_eq!(t.progress(), None);
        // Not a notification, nor shown once stale.
        t.feed(b"\x1b]9;4;1;5\x07");
        assert!(t.take_notifications().is_empty());
        assert!(t.progress_expiry().is_some_and(|at| at > Instant::now()));
        t.progress.as_mut().unwrap().1 -= PROGRESS_TIMEOUT;
        assert_eq!((t.progress(), t.progress_expiry()), (None, None));
    }

    #[test]
    fn pointer_shapes() {
        let mut t = Term::new(5, 1);
        assert_eq!(t.pointer_shape(), PointerShape::Default);
        t.feed(b"\x1b]22;pointer\x07");
        assert_eq!(t.pointer_shape(), PointerShape::Pointer);
        t.feed(b"\x1b]22;no-such-shape\x07");
        assert_eq!(t.pointer_shape(), PointerShape::Pointer);
        t.feed(b"\x1b]22;xterm\x1b\\");
        assert_eq!(t.pointer_shape(), PointerShape::Text);
        t.feed(b"\x1b]22;\x07");
        assert_eq!(t.pointer_shape(), PointerShape::Default);
        t.feed(b"\x1b]22;grab\x07\x1bc");
        assert_eq!(t.pointer_shape(), PointerShape::Default);
    }

    #[test]
    fn font_requests() {
        let mut t = Term::new(5, 1);
        t.feed(b"\x1b]50;JetBrains Mono\x07");
        assert_eq!(t.take_font_request(), Some(Some("JetBrains Mono".into())));
        assert_eq!(t.take_font_request(), None);
        // Queries aren't answered, nor taken as a family.
        t.feed(b"\x1b]50;?\x07");
        assert_eq!(t.take_font_request(), None);
        t.feed(b"\x1b]50;\x1b\\");
        assert_eq!(t.take_font_request(), Some(None));
        // A reset gives the pane its own font back.
        t.feed(b"\x1b]50;Menlo\x07\x1bc");
        assert_eq!(t.take_font_request(), Some(None));
    }
}
