//! The bar along the bottom of a pane whose program has ended and which
//! stayed open (`close-on-exit`, or a pane an agent opened): how the
//! program ended, red if it failed, with buttons to start it again and to
//! close the pane. What they do is in `app`; this is only the AppKit side.

use crate::ffi::*;

pub const HEIGHT: f64 = 32.0;
const PAD: f64 = 8.0;
const ICON: f64 = 16.0;
const BUTTON_HEIGHT: f64 = 22.0;
const GAP: f64 = 6.0;
/// The longest command shown in full; longer ones end in "…".
const MAX_COMMAND: usize = 60;

/// What the bar says: `command` is what the pane ran, `code` how it ended
/// (`None`: a signal ended it). Whether it failed, too.
pub fn message(command: &str, code: Option<i32>) -> (String, bool) {
    let command = if command.chars().count() > MAX_COMMAND {
        let cut: String = command.chars().take(MAX_COMMAND - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        command.to_owned()
    };
    match code {
        Some(0) => (format!("“{command}” finished"), false),
        Some(code) => (format!("“{command}” exited with code {code}"), true),
        None => (format!("“{command}” was ended by a signal"), true),
    }
}

pub struct ExitBar {
    /// Ours (+1); holds the controls, which live as long as it does.
    bar: Id,
}

impl ExitBar {
    /// Adds the bar along the bottom of `container`, over a terminal view
    /// the caller moves up to make room. The buttons send `target`
    /// `restartPane:` and `closeExitedPane:`. `dark` picks a dark bar, to
    /// sit well on a dark terminal theme.
    pub unsafe fn open(container: Id, target: Id, text: &str, failed: bool, dark: bool) -> Self {
        unsafe {
            let width = send_rect(container, c"bounds").size.width;
            let bar: Id = msg!(
                msg!(class(c"NSVisualEffectView"), c"alloc"),
                c"initWithFrame:",
                rect(0.0, 0.0, width, HEIGHT)
            );
            call!(bar, c"setMaterial:", 3isize); // titlebar
            call!(bar, c"setBlendingMode:", 1isize); // within the window
            call!(bar, c"setState:", 1isize); // always active
            call!(bar, c"setAutoresizingMask:", 2usize | 32); // full width, pinned to the bottom
            let name = if dark {
                "NSAppearanceNameDarkAqua"
            } else {
                "NSAppearanceNameAqua"
            };
            let appearance: Id = msg!(class(c"NSAppearance"), c"appearanceNamed:", nsstring(name));
            call!(bar, c"setAppearance:", appearance);

            // A line along the top: red when the program failed.
            let (color_name, line) = if failed {
                (c"systemRedColor", 2.0)
            } else {
                (c"separatorColor", 1.0)
            };
            let line_color: Id = msg!(class(c"NSColor"), color_name);
            let edge: Id = msg!(
                msg!(class(c"NSView"), c"alloc"),
                c"initWithFrame:",
                rect(0.0, HEIGHT - line, width, line)
            );
            call!(edge, c"setWantsLayer:", YES);
            let cg: Id = msg!(line_color, c"CGColor");
            call!(msg!(edge, c"layer"), c"setBackgroundColor:", cg);
            call!(edge, c"setAutoresizingMask:", 2usize | 8); // full width, pinned to the top
            add(bar, edge);

            let (symbol, tint) = if failed {
                ("xmark.octagon.fill", c"systemRedColor")
            } else {
                ("checkmark.circle.fill", c"systemGreenColor")
            };
            let image: Id = msg!(
                class(c"NSImage"),
                c"imageWithSystemSymbolName:accessibilityDescription:",
                nsstring(symbol),
                nsstring(if failed { "Failed" } else { "Finished" })
            );
            let icon: Id = msg!(class(c"NSImageView"), c"imageViewWithImage:", image);
            let tint: Id = msg!(class(c"NSColor"), tint);
            call!(icon, c"setContentTintColor:", tint);
            let y = (HEIGHT - ICON) / 2.0;
            call!(icon, c"setFrame:", rect(PAD, y, ICON, ICON));
            call!(bar, c"addSubview:", icon);

            // The buttons, right to left.
            let mut right = width - PAD;
            // Light buttons, as in the find bar: a symbol, framed only
            // while the pointer is over them; the tooltip says what it does.
            // `size` in points: the arrow looks as large as the × a point
            // smaller.
            let mut button = |symbol: &str, size: f64, tip: &str, action: &std::ffi::CStr| {
                let image: Id = msg!(
                    class(c"NSImage"),
                    c"imageWithSystemSymbolName:accessibilityDescription:",
                    nsstring(symbol),
                    nsstring(tip)
                );
                let config: Id = msg!(
                    class(c"NSImageSymbolConfiguration"),
                    c"configurationWithPointSize:weight:scale:",
                    size,
                    0.0f64, // regular
                    2isize  // medium
                );
                let image: Id = msg!(image, c"imageWithSymbolConfiguration:", config);
                let button: Id = msg!(
                    class(c"NSButton"),
                    c"buttonWithImage:target:action:",
                    image,
                    target,
                    sel(action)
                );
                call!(button, c"setBezelStyle:", 13usize); // recessed
                call!(button, c"setShowsBorderOnlyWhileMouseInside:", YES);
                call!(button, c"setToolTip:", nsstring(tip));
                right -= BUTTON_HEIGHT;
                let y = (HEIGHT - BUTTON_HEIGHT) / 2.0;
                call!(
                    button,
                    c"setFrame:",
                    rect(right, y, BUTTON_HEIGHT, BUTTON_HEIGHT)
                );
                call!(button, c"setAutoresizingMask:", 1usize); // stays at the right
                call!(bar, c"addSubview:", button);
                right -= 2.0;
            };
            button("xmark", 12.0, "Close this pane", c"closeExitedPane:");
            button(
                "arrow.clockwise",
                11.0,
                "Restart: start it again (Return)",
                c"restartPane:",
            );
            right -= GAP;

            let label: Id = msg!(class(c"NSTextField"), c"labelWithString:", nsstring(text));
            let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 12.0f64);
            call!(label, c"setFont:", font);
            call!(label, c"setLineBreakMode:", 4usize); // truncate at the end
            call!(label, c"sizeToFit");
            let label_height = send_rect(label, c"frame").size.height;
            let x = PAD + ICON + GAP;
            call!(
                label,
                c"setFrame:",
                rect(
                    x,
                    (HEIGHT - label_height) / 2.0,
                    (right - x).max(0.0),
                    label_height
                )
            );
            call!(label, c"setAutoresizingMask:", 2usize); // takes the width the buttons leave
            call!(bar, c"addSubview:", label);

            call!(container, c"addSubview:", bar);
            Self { bar }
        }
    }
}

impl Drop for ExitBar {
    fn drop(&mut self) {
        // SAFETY: main thread; the bar's subviews go with it.
        unsafe {
            call!(self.bar, c"removeFromSuperview");
            call!(self.bar, c"release");
        }
    }
}

/// Adds `view` (+1, from `alloc`) to `parent`, which keeps it.
unsafe fn add(parent: Id, view: Id) {
    unsafe {
        call!(parent, c"addSubview:", view);
        call!(view, c"release");
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_say_how_the_program_ended() {
        assert_eq!(message("zsh", Some(0)), ("“zsh” finished".into(), false));
        assert_eq!(
            message("make test", Some(2)),
            ("“make test” exited with code 2".into(), true)
        );
        assert_eq!(
            message("sleep 100", None),
            ("“sleep 100” was ended by a signal".into(), true)
        );
        let long = "x".repeat(100);
        let (text, _) = message(&long, Some(1));
        assert!(text.starts_with(&format!("“{}…”", "x".repeat(MAX_COMMAND - 1))));
    }
}
