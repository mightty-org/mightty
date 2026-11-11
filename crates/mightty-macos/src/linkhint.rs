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
            let panel: Id = msg!(
                msg!(class(c"NSVisualEffectView"), c"alloc"),
                c"initWithFrame:",
                CGRect::default()
            );
            call!(panel, c"setMaterial:", 6isize); // popover
            call!(panel, c"setBlendingMode:", 1isize); // within the window
            call!(panel, c"setState:", 1isize); // always active
            let name = if dark {
                "NSAppearanceNameDarkAqua"
            } else {
                "NSAppearanceNameAqua"
            };
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
            let width = (size.width + 2.0 * PAD)
                .min(bounds.size.width - 2.0 * MARGIN)
                .max(0.0);
            let frame = CGRect {
                origin: CGPoint {
                    x: MARGIN,
                    y: MARGIN,
                },
                size: CGSize {
                    width,
                    height: HEIGHT,
                },
            };
            call!(self.panel, c"setFrame:", frame);
            let label = CGRect {
                origin: CGPoint {
                    x: PAD,
                    y: ((HEIGHT - size.height) / 2.0).round(),
                },
                size: CGSize {
                    width: width - 2.0 * PAD,
                    height: size.height,
                },
            };
            call!(self.label, c"setFrame:", label);
            call!(self.panel, c"setHidden:", NO);
        }
    }
}

/// What the hint says for a link to `uri`: its host first, so trimming a
/// long target never hides it, and anything that could make the text pass
/// for another address made visible (`<U+202E>`): invisible and
/// direction-changing characters, and in the host anything not ASCII
/// (a Cyrillic "а" looks like "a").
pub fn describe(uri: &str) -> String {
    let visible = |s: &str, ascii: bool| {
        s.chars()
            .map(|c| {
                let hide = c.is_control() || is_invisible(c) || (ascii && !c.is_ascii());
                if hide {
                    format!("<U+{:04X}>", c as u32)
                } else {
                    c.to_string()
                }
            })
            .collect::<String>()
    };
    match host(uri).filter(|h| !h.is_empty()) {
        Some(host) => format!(
            "{}  —  {}",
            tail(&visible(host, true), MAX_HOST_CHARS),
            visible(uri, false)
        ),
        None => visible(uri, false),
    }
}

/// The most of a host the hint shows. A long one is cut at the front: a
/// host is read from the end, and a cut at the window's edge would leave
/// `google.com.aaaa…` with `.evil.com` out of sight.
const MAX_HOST_CHARS: usize = 48;

fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().skip(count - (max - 1)).collect();
    format!("…{kept}")
}

/// The host a URL with an authority goes to: past any `user@` (so
/// `https://google.com@evil.com` is evil.com), up to the path. A backslash
/// ends it too, as browsers read one in a web address as a slash.
fn host(uri: &str) -> Option<&str> {
    let (_, rest) = uri.split_once("://")?;
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or_default();
    Some(
        authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host),
    )
}

/// Format characters and other default-ignorable code points: they render
/// as nothing or reorder the text around them.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{0600}'..='\u{0605}' | '\u{06DD}' | '\u{070F}'
        | '\u{0890}'..='\u{0891}' | '\u{08E2}' | '\u{115F}'..='\u{1160}' | '\u{17B4}'..='\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}'
        | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF0}'..='\u{FFFB}'
        | '\u{110BD}' | '\u{110CD}' | '\u{13430}'..='\u{1343F}' | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E0FFF}')
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
        assert_eq!(
            describe("https://example.com/a?b"),
            "example.com  —  https://example.com/a?b"
        );
        assert_eq!(describe("mailto:me@x.org"), "mailto:me@x.org");
        // Right-to-left override could make "evil.com/moc.elgoog" read as google.com.
        assert_eq!(
            describe("https://evil.com/\u{202E}moc.elgoog"),
            "evil.com  —  https://evil.com/<U+202E>moc.elgoog"
        );
        assert_eq!(
            describe("https://a\u{200B}b.com"),
            "a<U+200B>b.com  —  https://a<U+200B>b.com"
        );
        // What comes before an @ is a user name, not where the link goes.
        assert_eq!(
            describe("https://google.com@evil.com/x"),
            "evil.com  —  https://google.com@evil.com/x"
        );
        assert_eq!(
            describe("https://evil.com\\@google.com/"),
            "evil.com  —  https://evil.com\\@google.com/"
        );
        // Look-alike letters, variation selectors and tag characters.
        assert_eq!(
            describe("https://\u{0430}pple.com"),
            "<U+0430>pple.com  —  https://\u{0430}pple.com"
        );
        assert_eq!(
            describe("https://x.com/a\u{FE0F}\u{E0041}"),
            "x.com  —  https://x.com/a<U+FE0F><U+E0041>"
        );
        // A long host keeps its end, where the real domain is.
        let long = format!("https://google.com.{}.evil.com/", "a".repeat(100));
        let shown = describe(&long);
        assert!(
            shown.starts_with("…")
                && shown
                    .split("  —  ")
                    .next()
                    .unwrap()
                    .ends_with("aaa.evil.com"),
            "{shown}"
        );
        assert_eq!(
            shown.split("  —  ").next().unwrap().chars().count(),
            MAX_HOST_CHARS
        );
    }
}
