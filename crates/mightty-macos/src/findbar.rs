//! The find bar: a rounded translucent panel pinned to the window's top
//! right, holding the search field, with the match count inside it, and
//! buttons for the older match, the newer match and closing. What it
//! searches lives in `app::Search`; this is only the AppKit side.

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::ffi::*;

const HEIGHT: f64 = 34.0;
const MARGIN: f64 = 10.0;
const PAD: f64 = 5.0;
const FIELD_WIDTH: f64 = 250.0;
const FIELD_HEIGHT: f64 = 24.0;
const FIELD_MIN_WIDTH: f64 = 120.0;
const BUTTON: f64 = 24.0;
/// Between the count and what's beside it.
const GAP: f64 = 4.0;

/// `MighttySearchField`, an NSSearchField whose cell leaves room at the
/// right of its text for the match count (the cell's `tag`, in points), so
/// typing never runs under it.
fn search_field_class() -> Id {
    static CLASS: OnceLock<usize> = OnceLock::new();
    extern "C" fn text_rect(cell: Id, _: Sel, bounds: CGRect) -> CGRect {
        // SAFETY: `cell` is a MighttySearchFieldCell; documented signatures.
        unsafe {
            let mut rect = send_super_rect(
                cell,
                class(c"NSSearchFieldCell"),
                c"searchTextRectForBounds:",
                (bounds,),
            );
            let reserved: isize = msg!(cell, c"tag");
            rect.size.width = (rect.size.width - reserved as f64).max(0.0);
            rect
        }
    }
    extern "C" fn cell_class(_: Id, _: Sel) -> Id {
        class(c"MighttySearchFieldCell")
    }
    *CLASS.get_or_init(|| {
        // SAFETY: registering classes with documented method signatures.
        unsafe {
            register_class(
                c"MighttySearchFieldCell",
                c"NSSearchFieldCell",
                &[(
                    c"searchTextRectForBounds:",
                    text_rect as *const c_void,
                    c"{CGRect={CGPoint=dd}{CGSize=dd}}@:{CGRect={CGPoint=dd}{CGSize=dd}}",
                )],
            );
            let field = register_class(c"MighttySearchField", c"NSSearchField", &[]);
            // `+cellClass` is a class method: it goes on the metaclass.
            class_addMethod(
                object_getClass(field),
                sel(c"cellClass"),
                cell_class as *const c_void,
                c"#@:".as_ptr(),
            );
            field as usize
        }
    }) as Id
}

pub struct FindBar {
    /// Ours (+1); holds the controls below, which live as long as it does.
    panel: Id,
    pub field: Id,
    count: Id,
    older: Id,
    newer: Id,
    shown: Option<(String, bool)>,
}

impl FindBar {
    /// Adds the bar to `container` (the window's content view), fading in.
    /// `target` gets the field's delegate calls and the buttons' actions
    /// (`searchOlder:`, `searchNewer:`, `closeSearch:`). `dark` picks a dark
    /// panel, to sit well on a dark terminal theme.
    pub unsafe fn open(container: Id, target: Id, dark: bool) -> Self {
        unsafe {
            let bounds = send_rect(container, c"bounds");
            let buttons = 3.0 * BUTTON + 2.0;
            let fixed = PAD + PAD + buttons + PAD;
            let field_width =
                (bounds.size.width - 2.0 * MARGIN - fixed).clamp(FIELD_MIN_WIDTH, FIELD_WIDTH);
            let width = fixed + field_width;
            let frame = rect(
                (bounds.size.width - width - MARGIN).max(0.0),
                bounds.size.height - HEIGHT - MARGIN,
                width,
                HEIGHT,
            );

            let panel: Id = msg!(
                msg!(class(c"NSVisualEffectView"), c"alloc"),
                c"initWithFrame:",
                frame
            );
            call!(panel, c"setMaterial:", 6isize); // popover
            call!(panel, c"setBlendingMode:", 1isize); // within the window
            call!(panel, c"setState:", 1isize); // always active
            call!(panel, c"setAutoresizingMask:", 1usize | 8); // pinned to the top right
            let name = if dark {
                "NSAppearanceNameDarkAqua"
            } else {
                "NSAppearanceNameAqua"
            };
            let appearance: Id = msg!(class(c"NSAppearance"), c"appearanceNamed:", nsstring(name));
            call!(panel, c"setAppearance:", appearance);
            call!(panel, c"setWantsLayer:", YES);
            let layer: Id = msg!(panel, c"layer");
            call!(layer, c"setCornerRadius:", 9.0f64);
            call!(layer, c"setMasksToBounds:", YES);
            call!(layer, c"setBorderWidth:", 1.0f64);
            let border: Id = msg!(msg!(class(c"NSColor"), c"separatorColor"), c"CGColor");
            call!(layer, c"setBorderColor:", border);

            let mut x = PAD;
            let field_y = (HEIGHT - FIELD_HEIGHT) / 2.0;
            let field: Id = msg!(
                msg!(search_field_class(), c"alloc"),
                c"initWithFrame:",
                rect(x, field_y, field_width, FIELD_HEIGHT)
            );
            call!(field, c"setPlaceholderString:", nsstring("Find"));
            call!(field, c"setDelegate:", target);
            call!(field, c"setFocusRingType:", 1usize); // none: the panel already frames it
            add(panel, field);
            x += field_width + PAD;

            // Inside the field, just left of its clear button; placed by `show_count`.
            let count: Id = msg!(class(c"NSTextField"), c"labelWithString:", nsstring(""));
            let font: Id = msg!(
                class(c"NSFont"),
                c"monospacedDigitSystemFontOfSize:weight:",
                11.0f64,
                0.0f64
            );
            call!(count, c"setFont:", font);
            call!(field, c"addSubview:", count);

            let button_y = (HEIGHT - BUTTON) / 2.0;
            let mut button = |symbol: &str, tip: &str, action: &std::ffi::CStr| {
                let image: Id = msg!(
                    class(c"NSImage"),
                    c"imageWithSystemSymbolName:accessibilityDescription:",
                    nsstring(symbol),
                    nsstring(tip)
                );
                let button: Id = msg!(
                    class(c"NSButton"),
                    c"buttonWithImage:target:action:",
                    image,
                    target,
                    sel(action)
                );
                call!(button, c"setFrame:", rect(x, button_y, BUTTON, BUTTON));
                call!(button, c"setBezelStyle:", 13usize); // recessed
                call!(button, c"setShowsBorderOnlyWhileMouseInside:", YES);
                call!(button, c"setToolTip:", nsstring(tip));
                call!(panel, c"addSubview:", button);
                x += BUTTON + 1.0;
                button
            };
            let older = button("chevron.up", "Older match (Return, ⌘G)", c"searchOlder:");
            let newer = button(
                "chevron.down",
                "Newer match (⇧Return, ⇧⌘G)",
                c"searchNewer:",
            );
            button("xmark", "Close (Esc)", c"closeSearch:");

            call!(panel, c"setAlphaValue:", 0.0f64);
            call!(container, c"addSubview:", panel);
            call!(msg!(panel, c"animator"), c"setAlphaValue:", 1.0f64);
            let mut bar = Self {
                panel,
                field,
                count,
                older,
                newer,
                shown: None,
            };
            bar.show_count(None, false);
            bar
        }
    }

    /// "3 of 17" (`Some((3, 17))`), or "No results" for a query that
    /// matched nothing; blank with no query. The buttons are enabled only
    /// with matches to move between.
    pub fn show_count(&mut self, position: Option<(usize, usize)>, have_query: bool) {
        let (text, missing) = match position {
            Some((i, n)) => (format!("{i} of {n}"), false),
            None if have_query => ("No results".to_owned(), true),
            None => (String::new(), false),
        };
        if self.shown.as_ref() == Some(&(text.clone(), missing)) {
            return;
        }
        // SAFETY: main thread; documented signatures.
        unsafe {
            call!(self.count, c"setStringValue:", nsstring(&text));
            let color: Id = msg!(
                class(c"NSColor"),
                if missing {
                    c"systemRedColor"
                } else {
                    c"secondaryLabelColor"
                }
            );
            call!(self.count, c"setTextColor:", color);
            // As wide as its text, centred on the field's height, ending at the clear button.
            call!(self.count, c"sizeToFit");
            let size: CGRect = send_rect(self.count, c"frame");
            let bounds = send_rect(self.field, c"bounds");
            let cell: Id = msg!(self.field, c"cell");
            let clear = send_rect_with(cell, c"cancelButtonRectForBounds:", (bounds,));
            let (width, height) = (size.size.width, size.size.height);
            let frame = rect(
                clear.origin.x - width - GAP / 2.0,
                ((bounds.size.height - height) / 2.0).round(),
                width,
                height,
            );
            call!(self.count, c"setFrame:", frame);
            // Keep the text clear of it, also while editing.
            let reserved = if text.is_empty() {
                0
            } else {
                (width + GAP).ceil() as isize
            };
            call!(cell, c"setTag:", reserved);
            let editor: Id = msg!(self.field, c"currentEditor");
            if !editor.is_null() {
                let clip: Id = msg!(editor, c"superview");
                call!(
                    clip,
                    c"setFrame:",
                    send_rect_with(cell, c"searchTextRectForBounds:", (bounds,))
                );
            }
            call!(self.field, c"setNeedsDisplay:", YES);
            let enabled = if position.is_some() { YES } else { NO };
            call!(self.older, c"setEnabled:", enabled);
            call!(self.newer, c"setEnabled:", enabled);
        }
        self.shown = Some((text, missing));
    }
}

impl Drop for FindBar {
    fn drop(&mut self) {
        // SAFETY: main thread; the panel's subviews go with it.
        unsafe {
            call!(self.panel, c"removeFromSuperview");
            call!(self.panel, c"release");
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
