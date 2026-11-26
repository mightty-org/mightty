//! The badge in the top-right corner of the pane keys go to, while keys
//! mean something other than usual: the start of a binding pressed and
//! waiting for the rest (`⌃A …`), or a key table switched to (`resize`).

use crate::ffi::*;

const HEIGHT: f64 = 24.0;
const PAD: f64 = 10.0;
const MARGIN: f64 = 8.0;

pub struct KeyHud {
    /// Ours (+1); holds the label, which lives as long as it does.
    badge: Id,
}

impl KeyHud {
    /// Adds the badge saying `text` to the top-right of `container`.
    ///
    /// # Safety
    /// Main thread; `container` is a live view, not flipped.
    pub unsafe fn open(container: Id, text: &str) -> Self {
        unsafe {
            let label: Id = msg!(class(c"NSTextField"), c"labelWithString:", nsstring(text));
            let font: Id = msg!(
                class(c"NSFont"),
                c"monospacedSystemFontOfSize:weight:",
                12.0f64,
                0.23f64 // medium
            );
            call!(label, c"setFont:", font);
            call!(label, c"sizeToFit");
            let size = send_rect(label, c"frame").size;
            let width = size.width + 2.0 * PAD;
            let bounds = send_rect(container, c"bounds");
            let frame = rect(
                (bounds.size.width - width - MARGIN).max(0.0),
                bounds.size.height - HEIGHT - MARGIN,
                width,
                HEIGHT,
            );
            let badge: Id = msg!(
                msg!(class(c"NSVisualEffectView"), c"alloc"),
                c"initWithFrame:",
                frame
            );
            call!(badge, c"setMaterial:", 13isize); // HUD window
            call!(badge, c"setBlendingMode:", 1isize); // within the window
            call!(badge, c"setState:", 1isize); // always active
            call!(badge, c"setWantsLayer:", YES);
            let layer: Id = msg!(badge, c"layer");
            call!(layer, c"setCornerRadius:", HEIGHT / 2.0);
            call!(layer, c"setMasksToBounds:", YES);
            call!(badge, c"setAutoresizingMask:", 1usize | 8); // pinned to the top right
            call!(
                label,
                c"setFrame:",
                rect(PAD, (HEIGHT - size.height) / 2.0, size.width, size.height)
            );
            call!(badge, c"addSubview:", label);
            call!(container, c"addSubview:", badge);
            Self { badge }
        }
    }
}

impl Drop for KeyHud {
    fn drop(&mut self) {
        // SAFETY: main thread; the label goes with the badge.
        unsafe {
            call!(self.badge, c"removeFromSuperview");
            call!(self.badge, c"release");
        }
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}
