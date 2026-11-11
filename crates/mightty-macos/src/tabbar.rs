//! The tab bar, behaving as AppKit's: tabs sharing the window's width
//! (never narrower than `MIN_TAB`, scrolling sideways past that, in a real
//! `NSScrollView` for its momentum and bounce), the selected one in the
//! terminal's background and the rest a shade of it, thin lines between
//! them. A tab's close button shows while the pointer is over it, and an
//! unselected tab lightens under it. Selecting is instant; scrolled away,
//! the selected tab stays pinned at the edge; when tabs come or go the
//! others slide to their places. A + sits at the right end. Clicks reach
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

/// What a click on a tab bar asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Select(usize),
    Close(usize),
    /// Double-clicked: a new title for the tab.
    Rename(usize),
    New,
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
        at.clamp(scrolled, (scrolled + self.visible - self.width).max(scrolled))
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
        let Some(tab) = self.tabs.get(self.selected) else { return };
        let x = self.selected_x(self.scrolled());
        // SAFETY: main thread; documented signatures.
        unsafe {
            call!(tab.view, c"setFrameOrigin:", CGPoint { x, y: 0.0 });
            // Above its neighbours (NSWindowAbove, relative to none).
            call!(self.strip, c"addSubview:positioned:relativeTo:", tab.view, 1isize, NIL);
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
                let background = if on { cg(selected, 1.0) } else if hovered { cg(blend(bar, text, 0.06), 1.0) } else { cg(bar, 1.0) };
                call!(layer(tab.view), c"setBackgroundColor:", background);
                call!(tab.title, c"setTextColor:", ns_color(text, if on { 1.0 } else { 0.62 }));
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
    STATES.with(|states| states.try_borrow_mut().ok()?.iter_mut().find(|s| s.bar == bar).map(f))
}

/// `a` moved `amount` of the way to `b`.
fn blend(a: (u8, u8, u8), b: (u8, u8, u8), amount: f64) -> (u8, u8, u8) {
    let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * amount).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn ns_color((r, g, b): (u8, u8, u8), alpha: f64) -> Id {
    let c = |v: u8| v as f64 / 255.0;
    // SAFETY: documented signature.
    unsafe { msg!(class(c"NSColor"), c"colorWithSRGBRed:green:blue:alpha:", c(r), c(g), c(b), alpha) }
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
    CGRect { origin: CGPoint { x, y }, size: CGSize { width, height } }
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
        let (clicks, width): (isize, f64) = unsafe { (msg!(event, c"clickCount"), send_rect(bar, c"bounds").size.width) };
        let click = with_state(bar, |s| {
            if x >= width - PLUS {
                return Some(Click::New);
            }
            let (index, within) = s.tab_at(x)?;
            Some(match clicks {
                2.. => Click::Rename(index),
                // The close button shows under the pointer, so it's there to click.
                _ if within < CLOSE + 8.0 => Click::Close(index),
                _ => Click::Select(index),
            })
        })
        .flatten();
        match (click, ON_CLICK.get()) {
            (Some(click), Some(on_click)) => on_click(bar, click),
            // Not on a tab: the window moves, as by its title bar.
            // SAFETY: main thread; `event` is this mouse-down.
            _ => unsafe {
                let window: Id = msg!(bar, c"window");
                call!(window, c"performWindowDragWithEvent:", event);
            },
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
            register_class(c"MighttyTabBar", c"NSView", &[
                (c"mouseDown:", event(mouse_down), c"v@:@"),
                (c"mouseMoved:", event(mouse_moved), c"v@:@"),
                (c"mouseExited:", event(mouse_exited), c"v@:@"),
                (c"tabsScrolled:", event(scrolled), c"v@:@"),
                (c"acceptsFirstMouse:", yes as extern "C" fn(Id, Sel, Id) -> Bool as *const c_void, c"c@:@"),
                // Clicks reach it under a transparent title bar, not the window's dragging.
                (c"mouseDownCanMoveWindow", no as extern "C" fn(Id, Sel) -> Bool as *const c_void, c"c@:"),
            ]) as usize
        }
    }) as Id
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
            let area: Id = msg!(msg!(class(c"NSTrackingArea"), c"alloc"), c"initWithRect:options:owner:userInfo:", CGRect::default(), options, bar, NIL);
            call!(bar, c"addTrackingArea:", area);
            call!(area, c"release");

            // The tabs scroll as any view does: momentum, and a bounce at the ends.
            let scroller: Id = msg!(msg!(class(c"NSScrollView"), c"alloc"), c"initWithFrame:", rect(0.0, 0.0, 0.0, HEIGHT));
            call!(scroller, c"setDrawsBackground:", NO);
            call!(scroller, c"setHasHorizontalScroller:", NO);
            call!(scroller, c"setHasVerticalScroller:", NO);
            call!(scroller, c"setHorizontalScrollElasticity:", 2isize); // allowed
            call!(scroller, c"setVerticalScrollElasticity:", 1isize); // none
            call!(bar, c"addSubview:", scroller);
            call!(scroller, c"release");
            let strip: Id = msg!(msg!(class(c"NSView"), c"alloc"), c"initWithFrame:", rect(0.0, 0.0, 0.0, HEIGHT));
            call!(strip, c"setWantsLayer:", YES);
            call!(scroller, c"setDocumentView:", strip);
            call!(strip, c"release");
            let clip: Id = msg!(scroller, c"contentView");
            call!(clip, c"setPostsBoundsChangedNotifications:", YES);
            let center: Id = msg!(class(c"NSNotificationCenter"), c"defaultCenter");
            let name = nsstring("NSViewBoundsDidChangeNotification");
            call!(center, c"addObserver:selector:name:object:", bar, sel(c"tabsScrolled:"), name, clip);

            let rule: Id = msg!(msg!(class(c"NSView"), c"alloc"), c"initWithFrame:", rect(0.0, 0.0, bounds.size.width, 1.0));
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
    pub fn show(&self, titles: &[String], selected: usize, left: f64, background: (u8, u8, u8), foreground: (u8, u8, u8), animate: bool) {
        with_state(self.bar, |s| {
            // SAFETY: main thread; documented signatures; labels autoreleased, held by their tabs.
            unsafe {
                let dark = 0.2126 * background.0 as f64 + 0.7152 * background.1 as f64 + 0.0722 * background.2 as f64 <= 127.0;
                // The bar a shade off the background, darker on a dark theme, as AppKit's.
                let shade = blend(background, if dark { (0, 0, 0) } else { (255, 255, 255) }, 0.3);
                s.colors = [background, shade, foreground];
                call!(layer(s.bar), c"setBackgroundColor:", cg(shade, 1.0));
                call!(layer(s.rule), c"setBackgroundColor:", cg(foreground, 0.12));

                let bounds = send_rect(s.bar, c"bounds");
                s.left = left;
                s.visible = (bounds.size.width - left - PLUS).max(0.0);
                s.width = (s.visible / titles.len().max(1) as f64).max(MIN_TAB);
                call!(s.scroller, c"setFrame:", rect(left, 0.0, s.visible, HEIGHT));
                call!(s.plus, c"setTextColor:", ns_color(foreground, 0.6));
                call!(s.plus, c"setFrame:", rect(bounds.size.width - PLUS, (HEIGHT - 20.0) / 2.0, PLUS, 20.0));

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
                    let view: Id = msg!(msg!(class(c"NSView"), c"alloc"), c"initWithFrame:", rect(x, 0.0, 0.0, HEIGHT));
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
                    let line: Id = msg!(msg!(class(c"NSView"), c"alloc"), c"initWithFrame:", CGRect::default());
                    call!(line, c"setWantsLayer:", YES);
                    call!(view, c"addSubview:", line);
                    call!(line, c"release");
                    call!(s.strip, c"addSubview:", view);
                    call!(view, c"release");
                    s.tabs.push(Tab { view, title, close, line });
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
                call!(s.strip, c"setFrameSize:", CGSize { width: s.width * titles.len() as f64, height: HEIGHT });
                for (i, (tab, title)) in s.tabs.iter().zip(titles).enumerate() {
                    call!(tab.title, c"setStringValue:", nsstring(title));
                    call!(tab.title, c"setFrame:", rect(CLOSE + 8.0, (HEIGHT - 16.0) / 2.0, (s.width - 2.0 * (CLOSE + 8.0)).max(0.0), 16.0));
                    call!(tab.close, c"setTextColor:", ns_color(foreground, 0.6));
                    call!(tab.close, c"setFrame:", rect(8.0, (HEIGHT - 17.0) / 2.0, CLOSE, 17.0));
                    call!(tab.line, c"setFrame:", rect(s.width - 1.0, 0.0, 1.0, HEIGHT));
                    call!(layer(tab.line), c"setBackgroundColor:", cg(foreground, 0.12));
                    place(tab.view, rect(i as f64 * s.width, 0.0, s.width, HEIGHT));
                }
                s.paint();

                // Scrolled as far as needed for the selected tab to be in its place, in view.
                let scrolled = s.scrolled();
                let at = selected as f64 * s.width;
                let max = (s.width * titles.len() as f64 - s.visible).max(0.0);
                let to = if at < scrolled { at } else if at + s.width > scrolled + s.visible { at + s.width - s.visible } else { scrolled };
                let to = to.clamp(0.0, max);
                if to != scrolled {
                    call!(animator(s.clip), c"setBoundsOrigin:", CGPoint { x: to, y: 0.0 });
                    call!(s.scroller, c"reflectScrolledClipView:", s.clip);
                }
                s.pin();
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
