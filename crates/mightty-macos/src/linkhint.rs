//! Where a hovered link goes, shown at the window's bottom left like a
//! browser's status bar: with OSC 8 the link's text can say anything.

use crate::ffi::*;

const HEIGHT: f64 = 22.0;
const MARGIN: f64 = 8.0;
const PAD: f64 = 8.0;

pub struct LinkHint {
    /// Ours (+1); holds the label.
    panel: Id,
    label: Id,
}

impl LinkHint {
    /// A hint in `container` (the window's content view); `dark` as for the find bar.
    pub unsafe fn new(container: Id, dark: bool) -> Self {
        unsafe {
            let panel: Id = msg!(msg!(class(c"NSVisualEffectView"), c"alloc"), c"initWithFrame:", CGRect::default());
            call!(panel, c"setMaterial:", 6isize); // popover
            call!(panel, c"setBlendingMode:", 1isize); // within the window
            call!(panel, c"setState:", 1isize); // always active
            let name = if dark { "NSAppearanceNameDarkAqua" } else { "NSAppearanceNameAqua" };
            let appearance: Id = msg!(class(c"NSAppearance"), c"appearanceNamed:", nsstring(name));
            call!(panel, c"setAppearance:", appearance);
            call!(panel, c"setWantsLayer:", YES);
            let layer: Id = msg!(panel, c"layer");
            call!(layer, c"setCornerRadius:", 6.0f64);
            call!(layer, c"setMasksToBounds:", YES);
            call!(layer, c"setBorderWidth:", 1.0f64);
            let border: Id = msg!(msg!(class(c"NSColor"), c"separatorColor"), c"CGColor");
            call!(layer, c"setBorderColor:", border);
            let label: Id = msg!(class(c"NSTextField"), c"labelWithString:", nsstring(""));
            let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 11.0f64);
            call!(label, c"setFont:", font);
            call!(label, c"setLineBreakMode:", 5usize); // truncate in the middle
            call!(panel, c"addSubview:", label);
            call!(panel, c"setHidden:", YES);
            call!(container, c"addSubview:", panel);
            Self { panel, label }
        }
    }

    /// Shows `text`, or hides the hint for `None`.
    pub fn show(&self, text: Option<&str>) {
        // SAFETY: main thread; documented signatures.
        unsafe {
            let Some(text) = text else {
                call!(self.panel, c"setHidden:", YES);
                return;
            };
            call!(self.label, c"setStringValue:", nsstring(text));
            call!(self.label, c"sizeToFit");
            let container: Id = msg!(self.panel, c"superview");
            let bounds = send_rect(container, c"bounds");
            let size = send_rect(self.label, c"frame").size;
            let width = (size.width + 2.0 * PAD).min(bounds.size.width - 2.0 * MARGIN).max(0.0);
            let frame = CGRect { origin: CGPoint { x: MARGIN, y: MARGIN }, size: CGSize { width, height: HEIGHT } };
            call!(self.panel, c"setFrame:", frame);
            let label = CGRect {
                origin: CGPoint { x: PAD, y: ((HEIGHT - size.height) / 2.0).round() },
                size: CGSize { width: width - 2.0 * PAD, height: size.height },
            };
            call!(self.label, c"setFrame:", label);
            call!(self.panel, c"setHidden:", NO);
        }
    }
}

/// What the hint says for a link to `uri`: its host first, so trimming a
/// long target never hides it, and invisible or direction-changing
/// characters made visible (`<U+202E>`), so the text can't pass for
/// another address.
pub fn describe(uri: &str) -> String {
    let visible = |s: &str| {
        s.chars()
            .map(|c| if c.is_control() || is_invisible(c) { format!("<U+{:04X}>", c as u32) } else { c.to_string() })
            .collect::<String>()
    };
    let host = uri.split_once("://").map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default());
    match host.filter(|h| !h.is_empty()) {
        Some(host) => format!("{}  —  {}", visible(host), visible(uri)),
        None => visible(uri),
    }
}

/// Format characters that render as nothing or reorder text around them.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

impl Drop for LinkHint {
    fn drop(&mut self) {
        // SAFETY: main thread; the label goes with the panel.
        unsafe {
            call!(self.panel, c"removeFromSuperview");
            call!(self.panel, c"release");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_shows_host_and_hidden_characters() {
        assert_eq!(describe("https://example.com/a?b"), "example.com  —  https://example.com/a?b");
        assert_eq!(describe("mailto:me@x.org"), "mailto:me@x.org");
        // Right-to-left override could make "evil.com/moc.elgoog" read as google.com.
        assert_eq!(describe("https://evil.com/\u{202E}moc.elgoog"), "evil.com  —  https://evil.com/<U+202E>moc.elgoog");
        assert_eq!(describe("https://a\u{200B}b.com"), "a<U+200B>b.com  —  https://a<U+200B>b.com");
    }
}
