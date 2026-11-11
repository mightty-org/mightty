//! The tab bar, behaving as AppKit's: tabs sharing the window's width
//! (never narrower than `MIN_TAB`, scrolling sideways past that, in a real
//! `NSScrollView` for its momentum and bounce), the selected one in the
//! terminal's background and the rest a shade of it, thin lines between
//! them. A tab's close button shows while the pointer is over it, and an
//! unselected tab lightens under it. Selecting is instant; scrolled away,
//! the selected tab stays pinned at the edge; when tabs come or go the
//! others slide to their places. A + sits at the right end. A tab dragged
//! along the bar moves there, the others making way; dragged off it, it's
//! for a window of its own, or another window's bar. Clicks and drops reach
//! `app` as `Click`; dragging where there's no tab moves the window.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::OnceLock;

use crate::ffi::*;

/// The strip's height, a title bar's, in points.
pub const HEIGHT: f64 = 28.0;
const PLUS: f64 = 28.0;
const CLOSE: f64 = 16.0;
const MIN_TAB: f64 = 120.0;

/// What a click, drag or menu item on a tab bar asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    Select(usize),
    Close(usize),
    /// Double-clicked, or Rename Tab: its title to edit in place (see `TabBar::edit`).
    Rename(usize),
    /// Its title edited: the new one (empty: the program's), or `None` if cancelled.
    Retitle(usize, Option<String>),
    CloseOthers(usize),
    /// Move Tab to New Window.
    NewWindow(usize),
    New,
    /// Dragged along the bar: from one place to another (or back to its own).
    Move(usize, usize),
    /// Dragged off the bar and let go at this point on the screen.
    Detach(usize, (f64, f64)),
    /// Dragged onto another window's tab bar (by its view), to go at this place there.
    Transfer(usize, Id, usize),
}

/// How far the pointer moves before a press on a tab is a drag, and how
/// far off the bar a dragged tab leaves it, in points.
const DRAG_START: f64 = 4.0;
const TEAR_OFF: f64 = 24.0;

/// A tab being dragged: which, where it was grabbed (from its left), where
/// the press was, whether it's moved yet, where it would go, and whether
/// it's off the bar.
#[derive(Clone, Copy)]
struct Dragging {
    index: usize,
    grab: f64,
    start: f64,
    moved: bool,
    to: usize,
    off: bool,
    /// Over another window's tab bar: that bar, and where it would go there.
    target: Option<(Id, usize)>,
}

static ON_CLICK: OnceLock<fn(Id, Click)> = OnceLock::new();

/// Who gets the tab bars' clicks: the bar's view, and what it asks for.
pub fn set_click_handler(handler: fn(Id, Click)) {
    let _ = ON_CLICK.set(handler);
}

/// A tab's views: itself (its background), title, close button, and the
/// line at its right. Placed by `State`, none resizing with another.
#[derive(Clone, Copy)]
struct Tab {
    view: Id,
    title: Id,
    close: Id,
    line: Id,
}

impl Tab {
    /// Its title, close button and line placed for `width`.
    fn fit(&self, width: f64) {
        // SAFETY: main thread; documented signatures.
        unsafe {
            call!(
                self.title,
                c"setFrame:",
                rect(
                    CLOSE + 8.0,
                    (HEIGHT - 16.0) / 2.0,
                    (width - 2.0 * (CLOSE + 8.0)).max(0.0),
                    16.0
                )
            );
            call!(
                self.close,
                c"setFrame:",
                rect(8.0, (HEIGHT - 17.0) / 2.0, CLOSE, 17.0)
            );
            call!(self.line, c"setFrame:", rect(width - 1.0, 0.0, 1.0, HEIGHT));
        }
    }
}

/// A bar's views and what it shows, for drawing, clicks and the pointer.
struct State {
    bar: Id,
    /// A hairline along the bar's bottom.
    rule: Id,
    /// Scrolls `strip`, the tabs side by side, through `clip`.
    scroller: Id,
    clip: Id,
    strip: Id,
    plus: Id,
    tabs: Vec<Tab>,
    left: f64,
    visible: f64,
    width: f64,
    selected: usize,
    hovered: Option<usize>,
    dragging: Option<Dragging>,
    /// The window a tab dragged off the bar would become, following the pointer (ours, +1).
    preview: Option<Id>,
    /// Tabs dragged in from elsewhere: where they would go, and a stand-in
    /// for each there, with its title.
    incoming: Option<(usize, Vec<(Id, Id)>)>,
    /// Shown only for a tab dragged over its window (one tab, bar hidden).
    peeking: bool,
    /// A tab whose title is being edited, and the field it's edited in.
    editing: Option<(usize, Id)>,
    /// The selected tab's colour, the bar's, and the text's.
    colors: [(u8, u8, u8); 3],
}

impl State {
    /// How far the tabs are scrolled.
    fn scrolled(&self) -> f64 {
        // SAFETY: main thread; documented signature.
        unsafe { send_rect(self.clip, c"bounds") }.origin.x
    }

    /// Where the selected tab is drawn along the strip: in its place, or
    /// pinned to the visible edge it was scrolled past.
    fn selected_x(&self, scrolled: f64) -> f64 {
        let at = self.selected as f64 * self.width;
        at.clamp(
            scrolled,
            (scrolled + self.visible - self.width).max(scrolled),
        )
    }

    /// The tab at `x` in the bar, and how far into it.
    fn tab_at(&self, x: f64) -> Option<(usize, f64)> {
        if x < self.left || x >= self.left + self.visible || self.tabs.is_empty() {
            return None;
        }
        let scrolled = self.scrolled();
        let along = x - self.left + scrolled;
        // The selected tab, pinned or not, is over the others.
        let pinned = self.selected_x(scrolled);
        if (pinned..pinned + self.width).contains(&along) {
            return Some((self.selected, along - pinned));
        }
        let index = (along / self.width) as usize;
        (index < self.tabs.len()).then_some((index, along - index as f64 * self.width))
    }

    /// The selected tab in its place, or pinned at the edge, over the rest.
    fn pin(&self) {
        if self.dragging.is_some_and(|d| d.moved) {
            return; // it's where the pointer is
        }
        let Some(tab) = self.tabs.get(self.selected) else {
            return;
        };
        let x = self.selected_x(self.scrolled());
        // SAFETY: main thread; documented signatures.
        unsafe {
            call!(tab.view, c"setFrameOrigin:", CGPoint { x, y: 0.0 });
            // Above its neighbours (NSWindowAbove, relative to none).
            call!(
                self.strip,
                c"addSubview:positioned:relativeTo:",
                tab.view,
                1isize,
                NIL
            );
        }
    }

    /// Shows the window the dragged tab `index` would become, its title bar
    /// under the pointer: translucent, the size of this one, in its colours.
    fn show_preview(&mut self, index: usize) {
        // SAFETY: main thread; documented signatures; the window is ours until `close_preview`.
        unsafe {
            let window: Id = msg!(self.bar, c"window");
            let size = send_rect(window, c"frame").size;
            let preview = match self.preview {
                Some(preview) => preview,
                None => {
                    let preview: Id = msg!(
                        msg!(class(c"NSWindow"), c"alloc"),
                        c"initWithContentRect:styleMask:backing:defer:",
                        rect(0.0, 0.0, size.width, size.height),
                        0usize, // borderless
                        2usize, // buffered
                        NO
                    );
                    call!(preview, c"setReleasedWhenClosed:", NO);
                    call!(preview, c"setOpaque:", NO);
                    let clear: Id = msg!(class(c"NSColor"), c"clearColor");
                    call!(preview, c"setBackgroundColor:", clear);
                    call!(preview, c"setHasShadow:", YES);
                    call!(preview, c"setIgnoresMouseEvents:", YES);
                    call!(preview, c"setLevel:", 3isize); // floating
                    call!(preview, c"setAlphaValue:", 0.8f64);
                    let [background, bar, text] = self.colors;
                    let content: Id = msg!(preview, c"contentView");
                    call!(content, c"setWantsLayer:", YES);
                    call!(layer(content), c"setCornerRadius:", 10.0f64);
                    call!(layer(content), c"setMasksToBounds:", YES);
                    call!(layer(content), c"setBackgroundColor:", cg(background, 1.0));
                    let strip: Id = msg!(
                        msg!(class(c"NSView"), c"alloc"),
                        c"initWithFrame:",
                        rect(0.0, size.height - HEIGHT, size.width, HEIGHT)
                    );
                    call!(strip, c"setWantsLayer:", YES);
                    call!(layer(strip), c"setBackgroundColor:", cg(bar, 1.0));
                    call!(content, c"addSubview:", strip);
                    call!(strip, c"release");
                    let title: Id = msg!(self.tabs[index].title, c"stringValue");
                    let label: Id = msg!(class(c"NSTextField"), c"labelWithString:", title);
                    let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 12.0f64);
                    call!(label, c"setFont:", font);
                    call!(label, c"setAlignment:", 1isize); // centred
                    call!(label, c"setTextColor:", ns_color(text, 1.0));
                    call!(
                        label,
                        c"setFrame:",
                        rect(0.0, (HEIGHT - 16.0) / 2.0, size.width, 16.0)
                    );
                    call!(strip, c"addSubview:", label);
                    call!(preview, c"orderFront:", NIL);
                    self.preview = Some(preview);
                    preview
                }
            };
            // As `app` places the window: its title bar where the pointer is.
            let mouse: CGPoint = msg!(class(c"NSEvent"), c"mouseLocation");
            call!(
                preview,
                c"setFrameOrigin:",
                CGPoint {
                    x: mouse.x - 60.0,
                    y: mouse.y - size.height + 14.0
                }
            );
        }
    }

    /// Where a tab can be dropped on the screen: the bar, shown or not, and
    /// the window's title bar above it, as with AppKit's.
    fn on_screen(&self) -> Option<CGRect> {
        // SAFETY: main thread; documented signatures.
        unsafe {
            let window: Id = msg!(self.bar, c"window");
            let visible: Bool = if window.is_null() {
                NO
            } else {
                msg!(window, c"isVisible")
            };
            if visible == NO {
                return None;
            }
            let in_window = send_rect_with(
                self.bar,
                c"convertRect:toView:",
                (send_rect(self.bar, c"bounds"), NIL),
            );
            let mut rect = send_rect_with(window, c"convertRectToScreen:", (in_window,));
            let top = send_rect(window, c"frame");
            rect.size.height =
                (top.origin.y + top.size.height - rect.origin.y).max(rect.size.height);
            Some(rect)
        }
    }

    /// Shows a hidden bar while a tab is dragged over its window, or hides it again.
    fn peek(&mut self, on: bool) {
        // SAFETY: main thread; documented signature.
        let hidden: Bool = unsafe { msg!(self.bar, c"isHidden") };
        if on && hidden != NO {
            self.peeking = true;
            // SAFETY: as above.
            unsafe { call!(self.bar, c"setHidden:", NO) };
        } else if !on && self.peeking {
            self.peeking = false;
            // SAFETY: as above.
            unsafe { call!(self.bar, c"setHidden:", YES) };
        }
    }

    /// Opens a gap at `at` for tabs titled `titles` coming from elsewhere,
    /// a faint stand-in for each in it; with `None`, closes it. The tabs slide.
    fn set_incoming(&mut self, incoming: Option<(usize, &[String])>) {
        // Where, and how many: the stand-ins' titles don't change during a drag.
        if incoming.map(|i| (i.0, i.1.len())) == self.incoming.as_ref().map(|i| (i.0, i.1.len())) {
            return;
        }
        // SAFETY: main thread; documented signatures; the stand-ins are held by the strip.
        unsafe {
            let n = self.tabs.len();
            let k = incoming.map_or(0, |i| i.1.len());
            let width = if k > 0 {
                (self.visible / (n + k) as f64).max(MIN_TAB)
            } else {
                self.width
            };
            // Stand-ins made again when there's another number of them.
            let mut stand_ins = match self.incoming.take() {
                Some((_, views)) if views.len() == k => views,
                Some((_, views)) => {
                    for (view, _) in views {
                        call!(view, c"removeFromSuperview");
                    }
                    Vec::new()
                }
                None => Vec::new(),
            };
            if let Some((at, titles)) = incoming.filter(|_| k > 0) {
                for title in titles.iter().skip(stand_ins.len()) {
                    let view: Id = msg!(
                        msg!(class(c"NSView"), c"alloc"),
                        c"initWithFrame:",
                        rect(at as f64 * width, 0.0, width, HEIGHT)
                    );
                    call!(view, c"setWantsLayer:", YES);
                    call!(view, c"setAlphaValue:", 0.8f64);
                    call!(layer(view), c"setBackgroundColor:", cg(self.colors[0], 1.0));
                    let label: Id =
                        msg!(class(c"NSTextField"), c"labelWithString:", nsstring(title));
                    let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 12.0f64);
                    call!(label, c"setFont:", font);
                    call!(label, c"setAlignment:", 1isize); // centred
                    call!(label, c"setLineBreakMode:", 5usize);
                    call!(label, c"setTextColor:", ns_color(self.colors[2], 1.0));
                    call!(view, c"addSubview:", label);
                    call!(self.strip, c"addSubview:", view);
                    call!(view, c"release");
                    stand_ins.push((view, label));
                }
                for (i, &(view, label)) in stand_ins.iter().enumerate() {
                    call!(
                        label,
                        c"setFrame:",
                        rect(
                            CLOSE + 8.0,
                            (HEIGHT - 16.0) / 2.0,
                            (width - 2.0 * (CLOSE + 8.0)).max(0.0),
                            16.0
                        )
                    );
                    let animator: Id = msg!(view, c"animator");
                    call!(
                        animator,
                        c"setFrame:",
                        rect((at + i) as f64 * width, 0.0, width, HEIGHT)
                    );
                }
                self.incoming = Some((at, stand_ins));
            }
            let gap = incoming.filter(|_| k > 0).map(|i| i.0);
            call!(
                self.strip,
                c"setFrameSize:",
                CGSize {
                    width: width * (n + k) as f64,
                    height: HEIGHT
                }
            );
            for (j, tab) in self.tabs.iter().enumerate() {
                let slot = if gap.is_some_and(|at| j >= at) {
                    j + k
                } else {
                    j
                };
                tab.fit(width);
                let animator: Id = msg!(tab.view, c"animator");
                call!(
                    animator,
                    c"setFrame:",
                    rect(slot as f64 * width, 0.0, width, HEIGHT)
                );
            }
        }
    }

    fn close_preview(&mut self) {
        if let Some(preview) = self.preview.take() {
            // SAFETY: main thread; ours.
            unsafe {
                call!(preview, c"orderOut:", NIL);
                call!(preview, c"release");
            }
        }
    }

    /// Each tab's background, close button and line, for the selection and
    /// the pointer.
    fn paint(&self) {
        let [selected, bar, text] = self.colors;
        // SAFETY: main thread; documented signatures.
        unsafe {
            for (i, tab) in self.tabs.iter().enumerate() {
                let on = i == self.selected;
                let hovered = self.hovered == Some(i);
                // Unselected tabs drawn in the bar's shade, so a pinned tab covers them.
                let background = if on {
                    cg(selected, 1.0)
                } else if hovered {
                    cg(blend(bar, text, 0.06), 1.0)
                } else {
                    cg(bar, 1.0)
                };
                call!(layer(tab.view), c"setBackgroundColor:", background);
                call!(
                    tab.title,
                    c"setTextColor:",
                    ns_color(text, if on { 1.0 } else { 0.62 })
                );
                call!(tab.close, c"setHidden:", if hovered { NO } else { YES });
                // None beside the selected tab, nor after the last.
                let line = on || i + 1 == self.selected || i + 1 == self.tabs.len();
                call!(tab.line, c"setHidden:", if line { YES } else { NO });
            }
        }
    }
}

thread_local! {
    static STATES: RefCell<Vec<State>> = const { RefCell::new(Vec::new()) };
}

/// `f` on `bar`'s state; not while it's being changed already (AppKit
/// reporting a scroll as `show` resizes the strip: `show` pins itself).
fn with_state<R>(bar: Id, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATES.with(|states| {
        states
            .try_borrow_mut()
            .ok()?
            .iter_mut()
            .find(|s| s.bar == bar)
            .map(f)
    })
}

/// `a` moved `amount` of the way to `b`.
fn blend(a: (u8, u8, u8), b: (u8, u8, u8), amount: f64) -> (u8, u8, u8) {
    let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * amount).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn ns_color((r, g, b): (u8, u8, u8), alpha: f64) -> Id {
    let c = |v: u8| v as f64 / 255.0;
    // SAFETY: documented signature.
    unsafe {
        msg!(
            class(c"NSColor"),
            c"colorWithSRGBRed:green:blue:alpha:",
            c(r),
            c(g),
            c(b),
            alpha
        )
    }
}

fn cg(rgb: (u8, u8, u8), alpha: f64) -> Id {
    // SAFETY: documented signature.
    unsafe { msg!(ns_color(rgb, alpha), c"CGColor") }
}

fn layer(view: Id) -> Id {
    // SAFETY: documented signature.
    unsafe { msg!(view, c"layer") }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}

/// Where `event` is along `bar`.
fn x_in(bar: Id, event: Id) -> f64 {
    // SAFETY: a mouse NSEvent; documented signatures.
    unsafe {
        let point: CGPoint = msg!(event, c"locationInWindow");
        let local: CGPoint = msg!(bar, c"convertPoint:fromView:", point, NIL);
        local.x
    }
}

fn bar_class() -> Id {
    static CLASS: OnceLock<usize> = OnceLock::new();
    extern "C" fn mouse_down(bar: Id, _: Sel, event: Id) {
        let x = x_in(bar, event);
        // SAFETY: an NSEvent; documented signatures.
        let (clicks, width): (isize, f64) = unsafe {
            (
                msg!(event, c"clickCount"),
                send_rect(bar, c"bounds").size.width,
            )
        };
        let click = with_state(bar, |s| {
            s.dragging = None;
            if x >= width - PLUS {
                return Some(Click::New);
            }
            let (index, within) = s.tab_at(x)?;
            Some(match clicks {
                2.. => Click::Rename(index),
                // The close button shows under the pointer, so it's there to click.
                _ if within < CLOSE + 8.0 => Click::Close(index),
                _ => {
                    // Selected, and maybe dragged next.
                    // The only tab is the window's handle: it moves the window, as AppKit's does.
                    if s.tabs.len() > 1 {
                        s.dragging = Some(Dragging {
                            index,
                            grab: within,
                            start: x,
                            moved: false,
                            to: index,
                            off: false,
                            target: None,
                        });
                    }
                    Click::Select(index)
                }
            })
        })
        .flatten();
        let alone = with_state(bar, |s| s.tabs.len() == 1).unwrap_or(false);
        match (click, ON_CLICK.get()) {
            (Some(click @ Click::Select(_)), Some(on_click)) if alone => {
                on_click(bar, click);
                // SAFETY: main thread; `event` is this mouse-down.
                unsafe {
                    let window: Id = msg!(bar, c"window");
                    call!(window, c"performWindowDragWithEvent:", event);
                }
            }
            (Some(click), Some(on_click)) => on_click(bar, click),
            // Not on a tab: the window moves, as by its title bar.
            // SAFETY: main thread; `event` is this mouse-down.
            _ => unsafe {
                let window: Id = msg!(bar, c"window");
                call!(window, c"performWindowDragWithEvent:", event);
            },
        }
    }
    /// A grabbed tab follows the pointer along the bar, the others making
    /// way; off the bar, it fades, to be torn off.
    extern "C" fn mouse_dragged(bar: Id, _: Sel, event: Id) {
        let x = x_in(bar, event);
        // SAFETY: a mouse NSEvent; documented signatures.
        let y = unsafe {
            let point: CGPoint = msg!(event, c"locationInWindow");
            let local: CGPoint = msg!(bar, c"convertPoint:fromView:", point, NIL);
            local.y
        };
        with_state(bar, |s| {
            let Some(mut d) = s.dragging else { return };
            if !d.moved && (x - d.start).abs() < DRAG_START && (0.0..HEIGHT).contains(&y) {
                return;
            }
            d.moved = true;
            let n = s.tabs.len();
            let along = x - s.left + s.scrolled() - d.grab;
            let at = along.clamp(0.0, s.width * (n - 1) as f64);
            d.to = ((at + s.width / 2.0) / s.width) as usize;
            d.to = d.to.min(n - 1);
            // Off the bar, with other tabs to stay: the window it would become follows the pointer.
            d.off = !(-TEAR_OFF..HEIGHT + TEAR_OFF).contains(&y) && n > 1;
            s.dragging = Some(d);
            // SAFETY: main thread; documented signatures.
            unsafe {
                let tab = s.tabs[d.index];
                call!(tab.view, c"setFrameOrigin:", CGPoint { x: at, y: 0.0 });
                call!(
                    tab.view,
                    c"setAlphaValue:",
                    if d.off { 0.0f64 } else { 1.0f64 }
                );
                call!(
                    s.strip,
                    c"addSubview:positioned:relativeTo:",
                    tab.view,
                    1isize,
                    NIL
                );
                // The rest in order without it, a gap where it would go.
                for (j, other) in s.tabs.iter().enumerate().filter(|&(j, _)| j != d.index) {
                    let k = if j > d.index { j - 1 } else { j };
                    let slot = if k >= d.to && !d.off { k + 1 } else { k };
                    let animator: Id = msg!(other.view, c"animator");
                    call!(
                        animator,
                        c"setFrameOrigin:",
                        CGPoint {
                            x: slot as f64 * s.width,
                            y: 0.0
                        }
                    );
                }
            }
        });
        // Off the bar: over another window's tab bar, a gap opens there; else
        // the window it would become follows the pointer.
        let Some(Some((index, off, title, window))) = with_state(bar, |s| {
            let d = s.dragging?;
            // SAFETY: main thread; documented signatures.
            let (title, window): (Id, Id) = unsafe {
                (
                    msg!(s.tabs[d.index].title, c"stringValue"),
                    msg!(s.bar, c"window"),
                )
            };
            Some((d.index, d.off, title, window))
        }) else {
            return;
        };
        // SAFETY: main thread; documented signatures.
        let (mouse, title) = unsafe {
            (
                msg!(class(c"NSEvent"), c"mouseLocation"),
                string_from_ns(title),
            )
        };
        let target = hover(off.then_some(mouse), window, &[title]);
        with_state(bar, |s| {
            if let Some(d) = &mut s.dragging {
                d.target = target;
            }
            if off && target.is_none() {
                s.show_preview(index);
            } else {
                s.close_preview();
            }
        });
    }
    /// A drag let go: the tab moves there, or out.
    extern "C" fn mouse_up(bar: Id, _: Sel, event: Id) {
        let Some(Some(d)) = with_state(bar, |s| {
            s.close_preview();
            s.dragging.take()
        }) else {
            return;
        };
        if !d.moved {
            return;
        }
        // SAFETY: a mouse NSEvent of this window; documented signatures.
        let screen = unsafe {
            let window: Id = msg!(bar, c"window");
            let point: CGPoint = msg!(event, c"locationInWindow");
            let at = CGRect {
                origin: point,
                size: CGSize::default(),
            };
            send_rect_with(window, c"convertRectToScreen:", (at,)).origin
        };
        hover(None, NIL, &[]);
        let click = match (d.off, d.target) {
            (true, Some((into, at))) => Click::Transfer(d.index, into, at),
            (true, None) => Click::Detach(d.index, (screen.x, screen.y)),
            (false, _) => Click::Move(d.index, d.to),
        };
        if let Some(on_click) = ON_CLICK.get() {
            on_click(bar, click);
        }
    }
    /// The pointer over a tab shows its close button.
    extern "C" fn mouse_moved(bar: Id, _: Sel, event: Id) {
        let x = x_in(bar, event);
        with_state(bar, |s| {
            let hovered = s.tab_at(x).map(|t| t.0);
            if hovered != s.hovered {
                s.hovered = hovered;
                s.paint();
            }
        });
    }
    extern "C" fn mouse_exited(bar: Id, _: Sel, _event: Id) {
        with_state(bar, |s| {
            if s.hovered.take().is_some() {
                s.paint();
            }
        });
    }
    /// Every click in the bar is the bar's, not its tabs' views': those would
    /// let a press under a transparent title bar move the window.
    extern "C" fn hit_test(bar: Id, _: Sel, point: CGPoint) -> Id {
        // SAFETY: main thread; documented signatures. `point` is in the superview's coordinates.
        unsafe {
            let hidden: Bool = msg!(bar, c"isHidden");
            let f = send_rect(bar, c"frame");
            let inside = (f.origin.x..f.origin.x + f.size.width).contains(&point.x)
                && (f.origin.y..f.origin.y + f.size.height).contains(&point.y);
            if hidden != NO || !inside {
                return NIL;
            }
            // A title being edited takes its own clicks, to place the caret.
            if let Some(Some(field)) = with_state(bar, |s| s.editing.map(|e| e.1)) {
                let superview: Id = msg!(bar, c"superview");
                let in_bar: CGPoint = msg!(bar, c"convertPoint:fromView:", point, superview);
                let holder: Id = msg!(field, c"superview");
                let in_holder: CGPoint = msg!(holder, c"convertPoint:fromView:", in_bar, bar);
                let hit: Id = msg!(field, c"hitTest:", in_holder);
                if !hit.is_null() {
                    return hit;
                }
            }
            bar
        }
    }
    /// A right-click on a tab: its menu, as AppKit's tabs have.
    extern "C" fn right_mouse_down(bar: Id, _: Sel, event: Id) {
        let x = x_in(bar, event);
        let Some(Some((index, _))) = with_state(bar, |s| s.tab_at(x)) else {
            return;
        };
        // SAFETY: main thread; documented signatures; the menu is autoreleased once shown.
        unsafe {
            let menu: Id = msg!(
                msg!(class(c"NSMenu"), c"alloc"),
                c"initWithTitle:",
                nsstring("")
            );
            for (title, action) in [
                ("Rename Tab…", c"tabMenuRename:"),
                ("Move Tab to New Window", c"tabMenuNewWindow:"),
                ("", c""),
                ("Close Tab", c"tabMenuClose:"),
                ("Close Other Tabs", c"tabMenuCloseOthers:"),
            ] {
                if title.is_empty() {
                    let separator: Id = msg!(class(c"NSMenuItem"), c"separatorItem");
                    call!(menu, c"addItem:", separator);
                    continue;
                }
                let item: Id = msg!(
                    menu,
                    c"addItemWithTitle:action:keyEquivalent:",
                    nsstring(title),
                    sel(action),
                    nsstring("")
                );
                call!(item, c"setTarget:", bar);
                call!(item, c"setTag:", index as isize);
            }
            call!(
                class(c"NSMenu"),
                c"popUpContextMenu:withEvent:forView:",
                menu,
                event,
                bar
            );
            call!(menu, c"release");
        }
    }
    fn menu_click(bar: Id, item: Id, click: fn(usize) -> Click) {
        // SAFETY: a menu item of ours; documented signature.
        let index: isize = unsafe { msg!(item, c"tag") };
        if let Some(on_click) = ON_CLICK.get() {
            on_click(bar, click(index as usize));
        }
    }
    extern "C" fn menu_rename(bar: Id, _: Sel, item: Id) {
        menu_click(bar, item, Click::Rename);
    }
    extern "C" fn menu_new_window(bar: Id, _: Sel, item: Id) {
        menu_click(bar, item, Click::NewWindow);
    }
    extern "C" fn menu_close(bar: Id, _: Sel, item: Id) {
        menu_click(bar, item, Click::Close);
    }
    extern "C" fn menu_close_others(bar: Id, _: Sel, item: Id) {
        menu_click(bar, item, Click::CloseOthers);
    }
    /// Keys in a title being edited: Return keeps it, Escape doesn't.
    extern "C" fn edit_command(
        bar: Id,
        _: Sel,
        _control: Id,
        _text_view: Id,
        command: Sel,
    ) -> Bool {
        if command == sel(c"insertNewline:") {
            finish_edit(bar, true);
        } else if command == sel(c"cancelOperation:") {
            finish_edit(bar, false);
        } else {
            return NO;
        }
        YES
    }
    /// Clicking elsewhere ends the edit, keeping the title.
    extern "C" fn edit_ended(bar: Id, _: Sel, _notification: Id) {
        finish_edit(bar, true);
    }
    /// Scrolling over it scrolls the tabs, as their scroll view would.
    extern "C" fn scroll_wheel(bar: Id, _: Sel, event: Id) {
        if let Some(scroller) = with_state(bar, |s| s.scroller) {
            // SAFETY: main thread; the scroll view handles its own events.
            unsafe { call!(scroller, c"scrollWheel:", event) };
        }
    }
    /// The tabs scrolled (the clip view's bounds moved): the selected one pinned.
    extern "C" fn scrolled(bar: Id, _: Sel, _notification: Id) {
        with_state(bar, |s| s.pin());
    }
    extern "C" fn yes(_: Id, _: Sel, _: Id) -> Bool {
        YES
    }
    extern "C" fn no(_: Id, _: Sel) -> Bool {
        NO
    }
    *CLASS.get_or_init(|| {
        let event = |f: extern "C" fn(Id, Sel, Id)| f as *const c_void;
        // SAFETY: a new subclass, its methods with their signatures.
        unsafe {
            register_class(
                c"MighttyTabBar",
                c"NSView",
                &[
                    (c"mouseDown:", event(mouse_down), c"v@:@"),
                    (c"mouseDragged:", event(mouse_dragged), c"v@:@"),
                    (c"mouseUp:", event(mouse_up), c"v@:@"),
                    (c"mouseMoved:", event(mouse_moved), c"v@:@"),
                    (c"mouseExited:", event(mouse_exited), c"v@:@"),
                    (c"tabsScrolled:", event(scrolled), c"v@:@"),
                    (c"scrollWheel:", event(scroll_wheel), c"v@:@"),
                    (c"rightMouseDown:", event(right_mouse_down), c"v@:@"),
                    (c"tabMenuRename:", event(menu_rename), c"v@:@"),
                    (c"tabMenuNewWindow:", event(menu_new_window), c"v@:@"),
                    (c"tabMenuClose:", event(menu_close), c"v@:@"),
                    (c"tabMenuCloseOthers:", event(menu_close_others), c"v@:@"),
                    (c"controlTextDidEndEditing:", event(edit_ended), c"v@:@"),
                    (
                        c"control:textView:doCommandBySelector:",
                        edit_command as extern "C" fn(Id, Sel, Id, Id, Sel) -> Bool
                            as *const c_void,
                        c"c@:@@:",
                    ),
                    (
                        c"hitTest:",
                        hit_test as extern "C" fn(Id, Sel, CGPoint) -> Id as *const c_void,
                        c"@@:{CGPoint=dd}",
                    ),
                    (
                        c"acceptsFirstMouse:",
                        yes as extern "C" fn(Id, Sel, Id) -> Bool as *const c_void,
                        c"c@:@",
                    ),
                    // Clicks reach it under a transparent title bar, not the window's dragging.
                    (
                        c"mouseDownCanMoveWindow",
                        no as extern "C" fn(Id, Sel) -> Bool as *const c_void,
                        c"c@:",
                    ),
                ],
            ) as usize
        }
    }) as Id
}

/// The tab bar under `point` on the screen, of a window other than
/// `except`: a gap opens there for tabs titled `titles`, any other closing.
/// Returns that bar and where the tabs would go. `None` closes every gap.
pub fn hover(point: Option<CGPoint>, except: Id, titles: &[String]) -> Option<(Id, usize)> {
    STATES.with(|states| {
        let mut states = states.try_borrow_mut().ok()?;
        let mut found = None;
        for s in states.iter_mut() {
            // SAFETY: main thread; documented signature.
            let window: Id = unsafe { msg!(s.bar, c"window") };
            let over = point
                .filter(|_| window != except)
                .zip(s.on_screen())
                .filter(|(p, f)| {
                    (f.origin.x..f.origin.x + f.size.width).contains(&p.x)
                        && (f.origin.y..f.origin.y + f.size.height).contains(&p.y)
                });
            match over {
                Some((p, f)) if found.is_none() => {
                    s.peek(true);
                    let along = p.x - f.origin.x - s.left + s.scrolled();
                    let at = ((along + s.width / 2.0) / s.width).max(0.0) as usize;
                    let at = at.min(s.tabs.len());
                    s.set_incoming(Some((at, titles)));
                    found = Some((s.bar, at));
                }
                _ => {
                    s.set_incoming(None);
                    s.peek(false);
                }
            }
        }
        found
    })
}

/// Ends the title edit in `bar`: its text for the tab if `keep`.
fn finish_edit(bar: Id, keep: bool) {
    // Taken first: removing the field ends its editing, which comes back here.
    let Some(Some((index, field))) = with_state(bar, |s| s.editing.take()) else {
        return;
    };
    // SAFETY: main thread; the field is ours until removed here.
    let text = unsafe {
        let text = keep.then(|| string_from_ns(msg!(field, c"stringValue")));
        call!(field, c"setDelegate:", NIL);
        call!(field, c"removeFromSuperview");
        text
    };
    with_state(bar, |s| {
        if let Some(tab) = s.tabs.get(index) {
            // SAFETY: main thread; documented signature.
            unsafe { call!(tab.title, c"setHidden:", NO) };
        }
    });
    if let Some(on_click) = ON_CLICK.get() {
        on_click(
            bar,
            Click::Retitle(index, text.map(|t| t.trim().to_owned())),
        );
    }
}

pub struct TabBar {
    /// Ours (+1); holds the rest.
    pub bar: Id,
}

impl TabBar {
    /// Adds a bar to the top of `root`.
    pub unsafe fn new(root: Id) -> Self {
        unsafe {
            let bounds = send_rect(root, c"bounds");
            let frame = rect(0.0, bounds.size.height - HEIGHT, bounds.size.width, HEIGHT);
            let bar: Id = msg!(msg!(bar_class(), c"alloc"), c"initWithFrame:", frame);
            call!(bar, c"setAutoresizingMask:", 2usize | 8); // full width, pinned to the top
            call!(bar, c"setWantsLayer:", YES);
            // The pointer's moves while over it, for the close buttons.
            let options = 0x01usize | 0x02 | 0x80 | 0x200; // entered/exited, moved, always active, visible rect
            let area: Id = msg!(
                msg!(class(c"NSTrackingArea"), c"alloc"),
                c"initWithRect:options:owner:userInfo:",
                CGRect::default(),
                options,
                bar,
                NIL
            );
            call!(bar, c"addTrackingArea:", area);
            call!(area, c"release");

            // The tabs scroll as any view does: momentum, and a bounce at the ends.
            let scroller: Id = msg!(
                msg!(class(c"NSScrollView"), c"alloc"),
                c"initWithFrame:",
                rect(0.0, 0.0, 0.0, HEIGHT)
            );
            call!(scroller, c"setDrawsBackground:", NO);
            call!(scroller, c"setHasHorizontalScroller:", NO);
            call!(scroller, c"setHasVerticalScroller:", NO);
            call!(scroller, c"setHorizontalScrollElasticity:", 2isize); // allowed
            call!(scroller, c"setVerticalScrollElasticity:", 1isize); // none
            call!(bar, c"addSubview:", scroller);
            call!(scroller, c"release");
            let strip: Id = msg!(
                msg!(class(c"NSView"), c"alloc"),
                c"initWithFrame:",
                rect(0.0, 0.0, 0.0, HEIGHT)
            );
            call!(strip, c"setWantsLayer:", YES);
            call!(scroller, c"setDocumentView:", strip);
            call!(strip, c"release");
            let clip: Id = msg!(scroller, c"contentView");
            call!(clip, c"setPostsBoundsChangedNotifications:", YES);
            let center: Id = msg!(class(c"NSNotificationCenter"), c"defaultCenter");
            let name = nsstring("NSViewBoundsDidChangeNotification");
            call!(
                center,
                c"addObserver:selector:name:object:",
                bar,
                sel(c"tabsScrolled:"),
                name,
                clip
            );

            let rule: Id = msg!(
                msg!(class(c"NSView"), c"alloc"),
                c"initWithFrame:",
                rect(0.0, 0.0, bounds.size.width, 1.0)
            );
            call!(rule, c"setWantsLayer:", YES);
            call!(rule, c"setAutoresizingMask:", 2usize); // full width
            call!(bar, c"addSubview:", rule);
            call!(rule, c"release");
            let plus: Id = msg!(class(c"NSTextField"), c"labelWithString:", nsstring("+"));
            let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 16.0f64);
            call!(plus, c"setFont:", font);
            call!(plus, c"setAlignment:", 1isize); // centred
            call!(bar, c"addSubview:", plus);
            call!(root, c"addSubview:", bar);
            STATES.with_borrow_mut(|states| {
                states.push(State {
                    bar,
                    rule,
                    scroller,
                    clip,
                    strip,
                    plus,
                    tabs: Vec::new(),
                    left: 0.0,
                    visible: 0.0,
                    width: 0.0,
                    selected: 0,
                    hovered: None,
                    dragging: None,
                    preview: None,
                    incoming: None,
                    peeking: false,
                    editing: None,
                    colors: [(0, 0, 0); 3],
                })
            });
            Self { bar }
        }
    }

    /// Shows `titles`, `selected` picked out, from `left` points in (past
    /// the window's buttons, when they're over it); in shades of
    /// `background`, text in `foreground`. With `animate`, tabs slide to
    /// their places, as when one comes or goes.
    pub fn show(
        &self,
        titles: &[String],
        selected: usize,
        left: f64,
        background: (u8, u8, u8),
        foreground: (u8, u8, u8),
        animate: bool,
    ) {
        with_state(self.bar, |s| {
            s.peeking = false;
            // SAFETY: main thread; documented signatures; labels autoreleased, held by their tabs.
            unsafe {
                let dark = 0.2126 * background.0 as f64
                    + 0.7152 * background.1 as f64
                    + 0.0722 * background.2 as f64
                    <= 127.0;
                // The bar a shade off the background, darker on a dark theme, as AppKit's.
                let shade = blend(
                    background,
                    if dark { (0, 0, 0) } else { (255, 255, 255) },
                    0.3,
                );
                s.colors = [background, shade, foreground];
                call!(layer(s.bar), c"setBackgroundColor:", cg(shade, 1.0));
                call!(layer(s.rule), c"setBackgroundColor:", cg(foreground, 0.12));

                let bounds = send_rect(s.bar, c"bounds");
                s.left = left;
                s.visible = (bounds.size.width - left - PLUS).max(0.0);
                s.width = (s.visible / titles.len().max(1) as f64).max(MIN_TAB);
                call!(s.scroller, c"setFrame:", rect(left, 0.0, s.visible, HEIGHT));
                call!(s.plus, c"setTextColor:", ns_color(foreground, 0.6));
                call!(
                    s.plus,
                    c"setFrame:",
                    rect(bounds.size.width - PLUS, (HEIGHT - 20.0) / 2.0, PLUS, 20.0)
                );

                // Tabs kept as they are, added or dropped at the end, so they can slide.
                let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 12.0f64);
                let close_font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 13.0f64);
                while s.tabs.len() > titles.len() {
                    let tab = s.tabs.pop().unwrap();
                    call!(tab.view, c"removeFromSuperview");
                }
                while s.tabs.len() < titles.len() {
                    // A new tab grows from nothing at the end.
                    let x = s.width * s.tabs.len() as f64;
                    let view: Id = msg!(
                        msg!(class(c"NSView"), c"alloc"),
                        c"initWithFrame:",
                        rect(x, 0.0, 0.0, HEIGHT)
                    );
                    call!(view, c"setWantsLayer:", YES);
                    // Its contents placed here, not stretched as it grows.
                    call!(view, c"setAutoresizesSubviews:", NO);
                    call!(layer(view), c"setMasksToBounds:", YES);
                    let title: Id = msg!(class(c"NSTextField"), c"labelWithString:", nsstring(""));
                    call!(title, c"setFont:", font);
                    call!(title, c"setAlignment:", 1isize); // centred
                    call!(title, c"setLineBreakMode:", 5usize); // truncate in the middle
                    call!(view, c"addSubview:", title);
                    let close: Id = msg!(class(c"NSTextField"), c"labelWithString:", nsstring("×"));
                    call!(close, c"setFont:", close_font);
                    call!(view, c"addSubview:", close);
                    let line: Id = msg!(
                        msg!(class(c"NSView"), c"alloc"),
                        c"initWithFrame:",
                        CGRect::default()
                    );
                    call!(line, c"setWantsLayer:", YES);
                    call!(view, c"addSubview:", line);
                    call!(line, c"release");
                    call!(s.strip, c"addSubview:", view);
                    call!(view, c"release");
                    s.tabs.push(Tab {
                        view,
                        title,
                        close,
                        line,
                    });
                }
                s.selected = selected;
                s.hovered = s.hovered.filter(|&h| h < titles.len());

                let animator = |view: Id| -> Id { msg!(view, c"animator") };
                let place = |view: Id, frame: CGRect| {
                    if animate {
                        call!(animator(view), c"setFrame:", frame);
                    } else {
                        call!(view, c"setFrame:", frame);
                    }
                };
                call!(
                    s.strip,
                    c"setFrameSize:",
                    CGSize {
                        width: s.width * titles.len() as f64,
                        height: HEIGHT
                    }
                );
                for (i, (tab, title)) in s.tabs.iter().zip(titles).enumerate() {
                    call!(tab.title, c"setStringValue:", nsstring(title));
                    call!(tab.close, c"setTextColor:", ns_color(foreground, 0.6));
                    tab.fit(s.width);
                    call!(
                        layer(tab.line),
                        c"setBackgroundColor:",
                        cg(foreground, 0.12)
                    );
                    place(tab.view, rect(i as f64 * s.width, 0.0, s.width, HEIGHT));
                    call!(tab.view, c"setAlphaValue:", 1.0f64);
                }
                s.paint();

                // Scrolled as far as needed for the selected tab to be in its place, in view.
                let scrolled = s.scrolled();
                let at = selected as f64 * s.width;
                let max = (s.width * titles.len() as f64 - s.visible).max(0.0);
                let to = if at < scrolled {
                    at
                } else if at + s.width > scrolled + s.visible {
                    at + s.width - s.visible
                } else {
                    scrolled
                };
                let to = to.clamp(0.0, max);
                if to != scrolled {
                    call!(
                        animator(s.clip),
                        c"setBoundsOrigin:",
                        CGPoint { x: to, y: 0.0 }
                    );
                    call!(s.scroller, c"reflectScrolledClipView:", s.clip);
                }
                s.pin();
            }
        });
    }

    /// Edits tab `index`'s title in place, starting from `text`, selected.
    pub fn edit(&self, index: usize, text: &str) {
        with_state(self.bar, |s| {
            let Some(tab) = s.tabs.get(index).copied() else {
                return;
            };
            if s.editing.is_some() {
                return;
            }
            let [background, _, _] = s.colors;
            // SAFETY: main thread; documented signatures; the field is held by the tab.
            unsafe {
                // The system's rounded text field across the tab, even margins, centred.
                let width = send_rect(tab.view, c"frame").size.width;
                let field: Id = msg!(
                    msg!(class(c"NSTextField"), c"alloc"),
                    c"initWithFrame:",
                    rect(6.0, (HEIGHT - 20.0) / 2.0, (width - 12.0).max(40.0), 20.0)
                );
                call!(field, c"setStringValue:", nsstring(text));
                call!(
                    field,
                    c"setPlaceholderString:",
                    nsstring("The program's title")
                );
                let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 12.0f64);
                call!(field, c"setFont:", font);
                call!(field, c"setAlignment:", 1isize); // centred
                call!(field, c"setBezeled:", YES);
                call!(field, c"setBezelStyle:", 1usize); // rounded
                let cell: Id = msg!(field, c"cell");
                call!(cell, c"setUsesSingleLineMode:", YES);
                call!(cell, c"setScrollable:", YES);
                // Dark on a dark terminal, light on a light one, whatever the system's.
                let dark = 0.2126 * background.0 as f64
                    + 0.7152 * background.1 as f64
                    + 0.0722 * background.2 as f64
                    <= 127.0;
                let name = if dark {
                    "NSAppearanceNameDarkAqua"
                } else {
                    "NSAppearanceNameAqua"
                };
                let appearance: Id =
                    msg!(class(c"NSAppearance"), c"appearanceNamed:", nsstring(name));
                call!(field, c"setAppearance:", appearance);
                call!(field, c"setDelegate:", self.bar);
                call!(tab.title, c"setHidden:", YES);
                call!(tab.close, c"setHidden:", YES);
                call!(tab.view, c"addSubview:", field);
                call!(field, c"release");
                let window: Id = msg!(self.bar, c"window");
                call!(window, c"makeFirstResponder:", field);
                call!(field, c"selectText:", NIL);
                s.editing = Some((index, field));
            }
        });
    }

    pub fn set_hidden(&self, hidden: bool) {
        // SAFETY: main thread; documented signature.
        unsafe { call!(self.bar, c"setHidden:", if hidden { YES } else { NO }) };
    }
}

impl Drop for TabBar {
    fn drop(&mut self) {
        STATES.with_borrow_mut(|states| states.retain(|s| s.bar != self.bar));
        // SAFETY: main thread; ours, its subviews going with it.
        unsafe {
            let center: Id = msg!(class(c"NSNotificationCenter"), c"defaultCenter");
            call!(center, c"removeObserver:", self.bar);
            call!(self.bar, c"removeFromSuperview");
            call!(self.bar, c"release");
        }
    }
}
