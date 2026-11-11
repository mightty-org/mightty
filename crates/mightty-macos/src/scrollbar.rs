//! The scrollbar: a rounded knob at a pane's right edge in the style of
//! macOS's overlay scrollers, shown while scrolling through history and
//! hidden a moment after. Output arriving doesn't show it. It's drawn here
//! rather than by `NSScroller`, whose overlay knob only a scroll view shows.
//! The mouse on it reaches `app` through `set_mouse_handler`.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::ffi::*;

/// How long it stays after the last scroll.
pub const LINGER: Duration = Duration::from_millis(1200);
/// The strip at the edge it sits in, and the knob within it, in points.
const WIDTH: f64 = 12.0;
const KNOB: f64 = 7.0;
const INSET: f64 = 2.5;
const MIN_KNOB: f64 = 24.0;

/// What the mouse did on a scrollbar, at a height up the bar in points.
#[derive(Clone, Copy, Debug)]
pub enum Drag {
    Press(f64),
    Move(f64),
    Release,
}

/// `MighttyScrollbar`: the strip, passing its mouse events to the handler.
fn bar_class() -> Id {
    static CLASS: OnceLock<usize> = OnceLock::new();
    extern "C" fn mouse(bar: Id, _: Sel, event: Id) {
        // SAFETY: an NSEvent for this view; documented signatures.
        let (kind, y) = unsafe {
            let kind: usize = msg!(event, c"type");
            let point: CGPoint = msg!(event, c"locationInWindow");
            let local: CGPoint = msg!(bar, c"convertPoint:fromView:", point, NIL);
            (kind, local.y)
        };
        // NSEventType: left mouse down 1, up 2, dragged 6.
        let drag = match kind {
            1 => Drag::Press(y),
            2 => Drag::Release,
            _ => Drag::Move(y),
        };
        if let Some(on_mouse) = ON_MOUSE.get() {
            on_mouse(bar, drag);
        }
    }
    extern "C" fn yes(_: Id, _: Sel, _: Id) -> Bool {
        YES
    }
    *CLASS.get_or_init(|| {
        let mouse = mouse as extern "C" fn(Id, Sel, Id) as *const c_void;
        // SAFETY: a new subclass, its methods with their signatures.
        unsafe {
            register_class(
                c"MighttyScrollbar",
                c"NSView",
                &[
                    (c"mouseDown:", mouse, c"v@:@"),
                    (c"mouseDragged:", mouse, c"v@:@"),
                    (c"mouseUp:", mouse, c"v@:@"),
                    // A click on an inactive window grabs at once, as scrollers do.
                    (
                        c"acceptsFirstMouse:",
                        yes as extern "C" fn(Id, Sel, Id) -> Bool as *const c_void,
                        c"c@:@",
                    ),
                ],
            ) as usize
        }
    }) as Id
}

static ON_MOUSE: OnceLock<fn(Id, Drag)> = OnceLock::new();

/// Who gets the scrollbars' mouse events: the bar's view, and what happened.
pub fn set_mouse_handler(handler: fn(Id, Drag)) {
    let _ = ON_MOUSE.set(handler);
}

pub struct Scrollbar {
    /// Ours (+1): the strip; the knob is its subview.
    pub bar: Id,
    knob: Id,
    /// What it last showed: lines of history, rows on screen, lines scrolled back.
    shown: (usize, usize, usize),
    dark: Option<bool>,
    /// When it hides, while shown.
    hide_at: Option<Instant>,
    /// Where the knob was grabbed, up from its bottom, while dragged.
    grab: Option<f64>,
}

impl Scrollbar {
    /// Adds a hidden scrollbar to `container`'s right edge.
    pub unsafe fn new(container: Id) -> Self {
        unsafe {
            let bounds = send_rect(container, c"bounds");
            let frame = CGRect {
                origin: CGPoint {
                    x: bounds.size.width - WIDTH,
                    y: 0.0,
                },
                size: CGSize {
                    width: WIDTH,
                    height: bounds.size.height,
                },
            };
            let bar: Id = msg!(msg!(bar_class(), c"alloc"), c"initWithFrame:", frame);
            call!(bar, c"setAutoresizingMask:", 1usize | 16); // pinned right, full height
            // Hidden, not just clear: a clear one would still take the clicks at the pane's edge.
            call!(bar, c"setHidden:", YES);
            let knob: Id = msg!(
                msg!(class(c"NSView"), c"alloc"),
                c"initWithFrame:",
                CGRect::default()
            );
            call!(knob, c"setWantsLayer:", YES);
            let layer: Id = msg!(knob, c"layer");
            call!(layer, c"setCornerRadius:", KNOB / 2.0);
            call!(bar, c"addSubview:", knob);
            call!(knob, c"release");
            call!(container, c"addSubview:", bar);
            Self {
                bar,
                knob,
                shown: (0, 0, 0),
                dark: None,
                hide_at: None,
                grab: None,
            }
        }
    }

    /// Shows `offset` lines scrolled back through `history` with `rows` on
    /// screen; it appears when the offset changed (`enabled` and with
    /// history to show), light on a `dark` terminal.
    pub fn update(
        &mut self,
        history: usize,
        rows: usize,
        offset: usize,
        enabled: bool,
        dark: bool,
    ) {
        if self.dark != Some(dark) {
            self.dark = Some(dark);
            // SAFETY: main thread; documented signatures.
            unsafe {
                let color: Id = msg!(
                    class(c"NSColor"),
                    c"colorWithWhite:alpha:",
                    if dark { 1.0f64 } else { 0.0f64 },
                    0.45f64
                );
                let cg: Id = msg!(color, c"CGColor");
                let layer: Id = msg!(self.knob, c"layer");
                call!(layer, c"setBackgroundColor:", cg);
            }
        }
        let moved = offset != self.shown.2;
        self.shown = (history, rows, offset);
        self.place_knob();
        if !enabled || history == 0 {
            self.hide();
        } else if moved {
            self.flash();
        }
    }

    /// The knob's height and the distance it travels, in the bar's points.
    fn geometry(&self) -> (f64, f64) {
        let (history, rows, _) = self.shown;
        // SAFETY: main thread; documented signature.
        let height = (unsafe { send_rect(self.bar, c"bounds") }.size.height - 2.0 * INSET).max(0.0);
        let knob = (height * rows as f64 / (history + rows).max(1) as f64)
            .clamp(MIN_KNOB.min(height), height);
        (knob, height - knob)
    }

    /// The knob's bottom, up the bar: at the bottom when live, at the top
    /// scrolled all the way back.
    fn knob_bottom(&self) -> f64 {
        let (history, _, offset) = self.shown;
        let (_, travel) = self.geometry();
        INSET
            + if history == 0 {
                0.0
            } else {
                travel * offset as f64 / history as f64
            }
    }

    fn place_knob(&self) {
        let (knob, _) = self.geometry();
        let frame = CGRect {
            origin: CGPoint {
                x: WIDTH - KNOB - INSET,
                y: self.knob_bottom(),
            },
            size: CGSize {
                width: KNOB,
                height: knob,
            },
        };
        // SAFETY: main thread; documented signature.
        unsafe { call!(self.knob, c"setFrame:", frame) };
    }

    /// Shows it until `LINGER` from now.
    pub fn flash(&mut self) {
        // SAFETY: main thread; documented signature.
        unsafe { call!(self.bar, c"setHidden:", NO) };
        self.hide_at = Some(Instant::now() + LINGER);
    }

    /// Hides it if it's time; else says when it's due to.
    pub fn fade(&mut self, now: Instant) -> Option<Instant> {
        match self.hide_at {
            // Not from under the pointer dragging it.
            Some(at) if at <= now && self.grab.is_none() => {
                self.hide();
                None
            }
            Some(at) => Some(at.max(now + Duration::from_millis(100))),
            None => None,
        }
    }

    fn hide(&mut self) {
        self.hide_at = None;
        // SAFETY: main thread; documented signature.
        unsafe { call!(self.bar, c"setHidden:", YES) };
    }

    /// Where `drag` moves the screen: lines scrolled back, out of `history`
    /// with `rows` on screen and `offset` scrolled now. A press on the knob
    /// grabs it; above or below, it moves a page that way.
    pub fn drag(&mut self, drag: Drag, history: usize, rows: usize, offset: usize) -> usize {
        self.shown = (history, rows, offset);
        let (knob, travel) = self.geometry();
        let bottom = self.knob_bottom();
        match drag {
            Drag::Press(y) if (bottom..bottom + knob).contains(&y) => {
                self.grab = Some(y - bottom);
                offset
            }
            Drag::Press(y) if y > bottom => (offset + rows).min(history),
            Drag::Press(_) => offset.saturating_sub(rows),
            Drag::Move(y) => match self.grab {
                Some(grab) if travel > 0.0 => ((y - grab - INSET).clamp(0.0, travel) / travel
                    * history as f64)
                    .round() as usize,
                _ => offset,
            },
            Drag::Release => {
                self.grab = None;
                offset
            }
        }
    }
}

impl Drop for Scrollbar {
    fn drop(&mut self) {
        // SAFETY: main thread; ours, and the knob goes with it.
        unsafe {
            call!(self.bar, c"removeFromSuperview");
            call!(self.bar, c"release");
        }
    }
}
