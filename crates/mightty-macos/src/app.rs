//! Cocoa application: windows showing panes from a [`Mux`].
//!
//! Threads: the main thread owns every Objective-C object and renderer. Pane
//! reader threads (inside the mux) parse output and notify [`GuiHost`],
//! which hands redraws and window opening/closing to the main thread through
//! libdispatch. The mux also serves the control socket, so `mightty ctl` and
//! AI agents can open, drive and read panes; panes they open get windows.

use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::fs;
use std::io;
use std::mem;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mightty_mux::{Host, Mux, Pane, PaneId, SpawnOptions};
use mightty_term::{MouseMode, Term};

use crate::config::Config;
use crate::ffi::*;
use crate::keys::{self, Mods, MouseAction};
use crate::latency;
use crate::renderer::{self, Renderer, Snapshot};

struct App {
    mux: Arc<Mux>,
    windows: Vec<Window>,
    /// App delegate and delegate of every window.
    delegate: Id,
    view_class: Id,
    config: Config,
    /// Where the next window's top-left corner goes.
    cascade: CGPoint,
}

/// One NSWindow showing one pane.
struct Window {
    window: Id,
    view: Id,
    layer: Id,
    renderer: Renderer,
    /// Reused between frames; filled under the terminal lock.
    snapshot: Snapshot,
    pane: Arc<Pane>,
    /// Keep the window open after the pane's process exits (panes opened by
    /// agents, so their output stays readable).
    hold: bool,
    /// Lines scrolled up into history; 0 follows the live screen.
    scroll_offset: usize,
    /// Sub-line remainder of trackpad scrolling, in lines.
    scroll_remainder: f64,
    title: String,
    selection: Option<Selection>,
    /// Output redraws wait until then: one frame per display refresh.
    next_frame: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Granularity {
    Cell,
    Word,
    Line,
}

/// A selection in absolute lines, so it stays on its text while output scrolls.
#[derive(Clone, Copy, Debug)]
struct Selection {
    granularity: Granularity,
    anchor: (u64, usize),
    head: (u64, usize),
    /// A single click only becomes a selection once the mouse moves.
    dragged: bool,
}

/// The selected range, expanded to words or lines, start first.
fn selection_range(selection: Option<Selection>, term: &Term) -> Option<renderer::Selection> {
    let s = selection.filter(|s| s.dragged || s.granularity != Granularity::Cell)?;
    let (mut start, mut end) = if s.anchor <= s.head { (s.anchor, s.head) } else { (s.head, s.anchor) };
    match s.granularity {
        Granularity::Cell => {}
        Granularity::Word => {
            if let Some((first, _)) = term.word_at(start.0, start.1) {
                start.1 = first;
            }
            if let Some((_, last)) = term.word_at(end.0, end.1) {
                end.1 = last;
            }
        }
        Granularity::Line => {
            start.1 = 0;
            end.1 = usize::MAX;
        }
    }
    Some((start, end))
}

fn modifiers(flags: usize) -> Mods {
    Mods { shift: flags & (1 << 17) != 0, ctrl: flags & (1 << 18) != 0, alt: flags & (1 << 19) != 0 }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app. Returns `None`, skipping `f`, when called
/// re-entrantly from an AppKit callback triggered inside another `f`.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut app = cell.try_borrow_mut().ok()?;
        app.as_mut().map(f)
    })
}

/// Runs `f` on the window whose NSWindow or NSView is `id`.
fn with_window<R>(id: Id, f: impl FnOnce(&mut Window) -> R) -> Option<R> {
    with_app(|app| app.windows.iter_mut().find(|w| w.window == id || w.view == id).map(f)).flatten()
}

// ---- Mux host ----

/// Receives pane events on reader threads and forwards them to the main thread.
#[derive(Default)]
struct GuiHost {
    /// Panes with output not yet drawn; one main-thread redraw drains them all.
    pending: Mutex<Vec<PaneId>>,
}

static HOST: OnceLock<Arc<GuiHost>> = OnceLock::new();

impl Host for GuiHost {
    fn pane_output(&self, id: PaneId) {
        let mut pending = self.pending.lock().unwrap();
        if !pending.contains(&id) {
            pending.push(id);
            if pending.len() == 1 {
                run_on_main(on_redraw);
            }
        }
    }

    fn pane_exited(&self, id: PaneId) {
        run_on_main_with(id as usize, on_pane_exited);
    }

    fn show_pane(&self, id: PaneId) -> bool {
        run_on_main_with(id as usize, on_show_pane);
        true
    }

    fn pane_closed(&self, id: PaneId) {
        run_on_main_with(id as usize, on_pane_closed);
    }
}

/// Draws panes with new output, at most once per display refresh each.
///
/// Waiting for the refresh here, rather than in `nextDrawable`, keeps the
/// main thread free for input while output streams in. Output after a pause
/// is drawn at once. Invariant: while `pending` isn't empty, a call to this
/// is scheduled.
extern "C" fn on_redraw(_: *mut c_void) {
    let Some(host) = HOST.get() else { return };
    let ids = mem::take(&mut *host.pending.lock().unwrap());
    let now = Instant::now();
    let mut wait: Option<(Duration, Vec<PaneId>)> = None;
    with_app(|app| {
        for window in app.windows.iter_mut().filter(|w| ids.contains(&w.pane.id)) {
            if window.next_frame <= now {
                window.render();
            } else {
                let (delay, deferred) = wait.get_or_insert((Duration::MAX, Vec::new()));
                *delay = (*delay).min(window.next_frame - now);
                deferred.push(window.pane.id);
            }
        }
    });
    if let Some((delay, deferred)) = wait {
        let mut pending = host.pending.lock().unwrap();
        // Not empty: output arrived meanwhile and another call is on its way.
        let schedule = pending.is_empty();
        for id in deferred {
            if !pending.contains(&id) {
                pending.push(id);
            }
        }
        if schedule {
            run_on_main_after(delay, on_redraw);
        }
    }
}

extern "C" fn on_pane_exited(context: *mut c_void) {
    let id = context as PaneId;
    // Collect first: closing re-enters through windowWillClose:.
    let to_close = with_app(|app| {
        let mut to_close = Vec::new();
        for window in app.windows.iter_mut().filter(|w| w.pane.id == id) {
            if window.hold {
                window.render(); // the title now says it exited
            } else {
                to_close.push(window.window);
            }
        }
        to_close
    });
    close_windows(to_close.unwrap_or_default());
}

extern "C" fn on_pane_closed(context: *mut c_void) {
    let id = context as PaneId;
    let windows = with_app(|app| app.windows.iter().filter(|w| w.pane.id == id).map(|w| w.window).collect());
    close_windows(windows.unwrap_or_default());
}

/// Must run outside `with_app`: closing re-enters through windowWillClose:.
fn close_windows(windows: Vec<Id>) {
    for window in windows {
        // SAFETY: main thread; a live NSWindow.
        unsafe { call!(window, c"close") };
    }
}

extern "C" fn on_show_pane(context: *mut c_void) {
    let id = context as PaneId;
    with_app(|app| {
        if let Some(pane) = app.mux.pane(id) {
            app.open_window(pane, true);
        }
    });
}

/// Opens the first window and runs the Cocoa event loop. Returns only on setup errors.
pub fn run(config: Config) -> io::Result<()> {
    // SAFETY: AppKit calls on the main thread with their documented signatures.
    unsafe {
        objc_autoreleasePoolPush(); // outermost pool; the event loop adds its own per event

        let ns_app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
        call!(ns_app, c"setActivationPolicy:", 0isize); // regular app: Dock icon, menu bar
        install_menu(ns_app);
        let delegate: Id = msg!(register_delegate_class(), c"new");
        call!(ns_app, c"setDelegate:", delegate);

        let host = Arc::new(GuiHost::default());
        let _ = HOST.set(host.clone());
        let mux = Mux::with_options(host, config.term_options());
        if let Err(e) = mightty_mux::listen(mux.clone()) {
            eprintln!("mightty: control socket unavailable: {e}");
        }
        let pane = mux.spawn(SpawnOptions {
            command: config.command.clone(),
            // A command runs under plain sh, not the user's login shell and profile.
            shell: config.command.as_ref().map(|_| "/bin/sh".into()),
            cwd: config.cwd.clone(),
            cols: config.cols,
            rows: config.rows,
            ..Default::default()
        })?;

        let app = App { mux, windows: Vec::new(), delegate, view_class: register_view_class(), config, cascade: CGPoint::default() };
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        with_app(|app| app.open_window(pane, false));
        call!(ns_app, c"activateIgnoringOtherApps:", YES);
        call!(ns_app, c"run");
    }
    Ok(())
}

impl App {
    fn open_window(&mut self, pane: Arc<Pane>, hold: bool) {
        // SAFETY: main thread; AppKit calls with documented signatures.
        unsafe {
            let screen: Id = msg!(class(c"NSScreen"), c"mainScreen");
            let scale: f64 = if screen.is_null() { 2.0 } else { msg!(screen, c"backingScaleFactor") };
            let layer: Id = msg!(msg!(class(c"CAMetalLayer"), c"layer"), c"retain");
            let renderer = Renderer::new(layer, &self.config.font, self.config.font_size, scale, self.config.theme.clone());

            let (cols, rows) = {
                let term = pane.term.lock().unwrap();
                (term.cols(), term.rows())
            };
            let (cw, ch) = renderer.cell_size();
            let pad = 2.0 * renderer.padding_px();
            let content = CGRect {
                origin: CGPoint::default(),
                size: CGSize {
                    width: (cols as f64 * cw as f64 + pad) / scale,
                    height: (rows as f64 * ch as f64 + pad) / scale,
                },
            };
            // titled | closable | miniaturizable | resizable; buffered backing.
            let window: Id = msg!(
                msg!(class(c"NSWindow"), c"alloc"),
                c"initWithContentRect:styleMask:backing:defer:",
                content,
                15usize,
                2usize,
                NO
            );
            call!(window, c"setTitle:", nsstring("mightty"));
            call!(window, c"setDelegate:", self.delegate);
            call!(window, c"setReleasedWhenClosed:", NO);

            let view: Id = msg!(msg!(self.view_class, c"alloc"), c"initWithFrame:", content);
            // Layer-hosting view: set the layer before enabling it.
            call!(view, c"setLayer:", layer);
            call!(view, c"setWantsLayer:", YES);
            call!(window, c"setContentView:", view);
            call!(window, c"makeFirstResponder:", view);
            if self.windows.is_empty() {
                call!(window, c"center");
                latency::start(window);
            }
            // With (0,0) this leaves the window in place and returns the next spot.
            self.cascade = msg!(window, c"cascadeTopLeftFromPoint:", self.cascade);
            call!(window, c"makeKeyAndOrderFront:", NIL);

            pane.set_visible(true);
            let mut window = Window {
                window,
                view,
                layer,
                renderer,
                snapshot: Snapshot::default(),
                pane,
                hold,
                scroll_offset: 0,
                scroll_remainder: 0.0,
                title: String::new(),
                selection: None,
                next_frame: Instant::now(),
            };
            window.resize();
            self.windows.push(window);
        }
    }

    fn new_shell_window(&mut self) {
        let opts = SpawnOptions { cols: self.config.cols, rows: self.config.rows, ..Default::default() };
        match self.mux.spawn(opts) {
            Ok(pane) => self.open_window(pane, false),
            Err(e) => eprintln!("mightty: could not start a shell: {e}"),
        }
    }

    /// Forgets a closed window and ends its pane unless another window shows it.
    fn window_closed(&mut self, window: Id) {
        let Some(i) = self.windows.iter().position(|w| w.window == window) else { return };
        let closed = self.windows.remove(i);
        if !self.windows.iter().any(|w| w.pane.id == closed.pane.id) {
            closed.pane.set_visible(false);
            self.mux.close(closed.pane.id);
        }
    }
}

impl Window {
    fn render(&mut self) {
        // SAFETY: main thread; AppKit calls with documented signatures.
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let frame = {
                let mut term = self.pane.term.lock().unwrap();
                if term.take_bell() {
                    NSBeep();
                }
                let mut title = if term.title().is_empty() { self.pane.command.clone() } else { term.title().to_owned() };
                if let Some(code) = self.pane.exit_status() {
                    title += &code.map_or(" — exited".into(), |c| format!(" — exited ({c})"));
                }
                if title != self.title {
                    call!(self.window, c"setTitle:", nsstring(&title));
                    self.title = title;
                }
                // Synchronized output (mode 2026): hold the frame until the app finishes.
                if term.modes().sync_output {
                    None
                } else {
                    self.snapshot.capture(&term, self.scroll_offset);
                    Some(selection_range(self.selection, &term))
                }
            };
            // The terminal is unlocked again: build and draw the frame.
            if let Some(selection) = frame {
                let frame = self.renderer.prepare(&self.snapshot, selection);
                let presented = latency::frame(self.window, &self.snapshot);
                self.renderer.present(frame, presented);
                let screen: Id = msg!(self.window, c"screen");
                let fps: isize = if screen.is_null() { 0 } else { msg!(screen, c"maximumFramesPerSecond") };
                self.next_frame = Instant::now() + Duration::from_secs(1) / fps.clamp(30, 240) as u32;
            }
            objc_autoreleasePoolPop(pool);
        }
    }

    fn resize(&mut self) {
        // SAFETY: main thread; documented signatures.
        unsafe {
            let bounds = send_rect(self.view, c"bounds");
            let scale: f64 = msg!(self.window, c"backingScaleFactor");
            if scale != self.renderer.scale() {
                self.renderer.set_scale(scale);
            }
            let (width, height) = (bounds.size.width * scale, bounds.size.height * scale);
            call!(self.layer, c"setContentsScale:", scale);
            call!(self.layer, c"setDrawableSize:", CGSize { width, height });
            let (cols, rows) = self.renderer.grid_size(width, height);
            self.pane.resize(cols as u16, rows as u16, width as u16, height as u16);
        }
        self.render();
    }

    /// Sends input to the pane and jumps back to the live screen.
    fn write(&mut self, bytes: &[u8]) {
        let _ = self.pane.write(bytes);
        if self.scroll_offset != 0 || self.selection.is_some() {
            self.scroll_offset = 0;
            self.selection = None;
            self.render();
        }
    }

    fn mouse(&mut self, action: MouseAction, location: CGPoint, clicks: isize, mods: Mods) {
        let (col, row) = self.cell_at(location);
        let (mode, sgr, cols, rows, lines_scrolled) = {
            let term = self.pane.term.lock().unwrap();
            let m = term.modes();
            (m.mouse, m.mouse_sgr, term.cols(), term.rows(), term.lines_scrolled())
        };
        let (col, row) = (col.min(cols - 1), row.min(rows - 1));

        // Programs that asked for the mouse get it; Shift forces a local selection.
        if mode != MouseMode::Off && !mods.shift {
            if action == MouseAction::Motion && mode == MouseMode::Click {
                return;
            }
            let _ = self.pane.write(&keys::mouse_report(0, action, col, row, mods, sgr));
            return;
        }

        let point = (lines_scrolled - self.scroll_offset as u64 + row as u64, col);
        match action {
            MouseAction::Press => {
                let granularity = match clicks {
                    ..=1 => Granularity::Cell,
                    2 => Granularity::Word,
                    _ => Granularity::Line,
                };
                self.selection = Some(Selection { granularity, anchor: point, head: point, dragged: false });
            }
            MouseAction::Motion => {
                if let Some(selection) = &mut self.selection {
                    selection.head = point;
                    selection.dragged = true;
                }
            }
            MouseAction::Release => return,
        }
        self.render();
    }

    fn copy(&mut self) {
        let text = {
            let term = self.pane.term.lock().unwrap();
            let Some((start, end)) = selection_range(self.selection, &term) else { return };
            term.text_range(start, end)
        };
        // SAFETY: main thread; documented signatures.
        unsafe {
            let pasteboard: Id = msg!(class(c"NSPasteboard"), c"generalPasteboard");
            let _: isize = msg!(pasteboard, c"clearContents");
            let _: Bool = msg!(pasteboard, c"setString:forType:", nsstring(&text), NSPasteboardTypeString);
        }
    }

    /// `lines` > 0 scrolls toward older output.
    fn scroll(&mut self, delta_lines: f64, column: usize, row: usize) {
        self.scroll_remainder += delta_lines;
        let lines = self.scroll_remainder.trunc();
        self.scroll_remainder -= lines;
        if lines == 0.0 {
            return;
        }
        let lines = lines as isize;
        let (alt, mouse, sgr, app_cursor, history) = {
            let term = self.pane.term.lock().unwrap();
            let m = term.modes();
            (term.is_alt_screen(), m.mouse, m.mouse_sgr, m.app_cursor, term.scrollback().len())
        };

        if alt {
            // Full-screen apps get wheel reports if they asked for mouse input,
            // otherwise arrow keys (alternate scroll), like most terminals.
            let up = lines > 0;
            let seq = if mouse != MouseMode::Off {
                keys::mouse_report(if up { 64 } else { 65 }, MouseAction::Press, column, row, Mods::default(), sgr)
            } else {
                let letter = if up { b'A' } else { b'B' };
                vec![0x1b, if app_cursor { b'O' } else { b'[' }, letter]
            };
            let _ = self.pane.write(&seq.repeat(lines.unsigned_abs()));
            return;
        }
        let offset = (self.scroll_offset as isize + lines).clamp(0, history as isize) as usize;
        if offset != self.scroll_offset {
            self.scroll_offset = offset;
            self.render();
        }
    }

    fn set_focused(&mut self, focused: bool) {
        self.renderer.focused = focused;
        if self.pane.term.lock().unwrap().modes().focus_events {
            let _ = self.pane.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
        self.render();
    }

    /// Converts a window-space point to a grid cell.
    fn cell_at(&self, point: CGPoint) -> (usize, usize) {
        // SAFETY: main thread; documented signature.
        let bounds = unsafe { send_rect(self.view, c"bounds") };
        let scale = self.renderer.scale();
        let (cw, ch) = self.renderer.cell_size();
        let pad = self.renderer.padding_px();
        let x = (point.x * scale - pad).max(0.0);
        let y = ((bounds.size.height - point.y) * scale - pad).max(0.0);
        ((x / cw as f64) as usize, (y / ch as f64) as usize)
    }
}

// ---- Objective-C classes ----

unsafe fn register_view_class() -> Id {
    extern "C" fn yes(_: Id, _: Sel) -> Bool {
        YES
    }

    extern "C" fn key_down(view: Id, _: Sel, event: Id) {
        // SAFETY: `event` is an NSEvent; documented signatures.
        unsafe {
            let flags: usize = msg!(event, c"modifierFlags");
            if flags & (1 << 20) != 0 {
                return; // Command shortcuts belong to the menu.
            }
            let mods = modifiers(flags);
            let code: u16 = msg!(event, c"keyCode");
            let chars = string_from_ns(msg!(event, c"characters"));
            let base = string_from_ns(msg!(event, c"charactersIgnoringModifiers"));
            with_window(view, |w| {
                let app_cursor = w.pane.term.lock().unwrap().modes().app_cursor;
                if let Some(bytes) = keys::encode(code, &chars, &base, mods, app_cursor) {
                    w.write(&bytes);
                }
            });
        }
    }

    extern "C" fn scroll_wheel(view: Id, _: Sel, event: Id) {
        // SAFETY: `event` is an NSEvent; documented signatures.
        unsafe {
            let dy: f64 = msg!(event, c"scrollingDeltaY");
            let precise: Bool = msg!(event, c"hasPreciseScrollingDeltas");
            let location: CGPoint = msg!(event, c"locationInWindow");
            with_window(view, |w| {
                // Trackpads report points, wheels report lines.
                let points_per_line = w.renderer.cell_size().1 as f64 / w.renderer.scale();
                let lines = if precise != 0 { dy / points_per_line } else { dy };
                let (col, row) = w.cell_at(location);
                w.scroll(lines, col, row);
            });
        }
    }

    extern "C" fn paste(view: Id, _: Sel, _sender: Id) {
        // SAFETY: AppKit calls with documented signatures.
        unsafe {
            let pasteboard: Id = msg!(class(c"NSPasteboard"), c"generalPasteboard");
            let text = string_from_ns(msg!(pasteboard, c"stringForType:", NSPasteboardTypeString));
            if text.is_empty() {
                return;
            }
            with_window(view, |w| {
                let bracketed = w.pane.term.lock().unwrap().modes().bracketed_paste;
                w.write(&keys::paste(&text, bracketed));
            });
        }
    }

    fn mouse_event(view: Id, event: Id, action: MouseAction) {
        // SAFETY: `event` is an NSEvent; documented signatures.
        unsafe {
            let mods = modifiers(msg!(event, c"modifierFlags"));
            let location: CGPoint = msg!(event, c"locationInWindow");
            let clicks: isize = msg!(event, c"clickCount");
            with_window(view, |w| w.mouse(action, location, clicks, mods));
        }
    }
    extern "C" fn mouse_down(view: Id, _: Sel, event: Id) {
        mouse_event(view, event, MouseAction::Press);
    }
    extern "C" fn mouse_dragged(view: Id, _: Sel, event: Id) {
        mouse_event(view, event, MouseAction::Motion);
    }
    extern "C" fn mouse_up(view: Id, _: Sel, event: Id) {
        mouse_event(view, event, MouseAction::Release);
    }
    extern "C" fn copy(view: Id, _: Sel, _sender: Id) {
        with_window(view, Window::copy);
    }

    let action = |f: extern "C" fn(Id, Sel, Id)| f as *const c_void;
    let predicate = |f: extern "C" fn(Id, Sel) -> Bool| f as *const c_void;
    unsafe {
        register_class(c"MighttyView", c"NSView", &[
            (c"acceptsFirstResponder", predicate(yes), c"c@:"),
            (c"isOpaque", predicate(yes), c"c@:"),
            (c"keyDown:", action(key_down), c"v@:@"),
            (c"scrollWheel:", action(scroll_wheel), c"v@:@"),
            (c"paste:", action(paste), c"v@:@"),
            (c"copy:", action(copy), c"v@:@"),
            (c"mouseDown:", action(mouse_down), c"v@:@"),
            (c"mouseDragged:", action(mouse_dragged), c"v@:@"),
            (c"mouseUp:", action(mouse_up), c"v@:@"),
        ])
    }
}

/// App delegate and window delegate in one.
unsafe fn register_delegate_class() -> Id {
    /// The NSWindow a window notification is about.
    fn window_of(notification: Id) -> Id {
        // SAFETY: an NSNotification.
        unsafe { msg!(notification, c"object") }
    }

    extern "C" fn terminate_after_last_window(_: Id, _: Sel, _: Id) -> Bool {
        YES
    }
    extern "C" fn will_terminate(_: Id, _: Sel, _: Id) {
        with_app(|app| {
            if let Some(socket) = app.mux.socket_path() {
                let _ = fs::remove_file(socket);
            }
        });
    }
    extern "C" fn did_resize(_: Id, _: Sel, notification: Id) {
        with_window(window_of(notification), Window::resize);
    }
    extern "C" fn did_become_key(_: Id, _: Sel, notification: Id) {
        with_window(window_of(notification), |w| w.set_focused(true));
    }
    extern "C" fn did_resign_key(_: Id, _: Sel, notification: Id) {
        with_window(window_of(notification), |w| w.set_focused(false));
    }
    extern "C" fn will_close(_: Id, _: Sel, notification: Id) {
        let window = window_of(notification);
        with_app(|app| app.window_closed(window));
    }
    extern "C" fn new_window(_: Id, _: Sel, _sender: Id) {
        with_app(App::new_shell_window);
    }

    let action = |f: extern "C" fn(Id, Sel, Id)| f as *const c_void;
    unsafe {
        register_class(c"MighttyDelegate", c"NSObject", &[
            (
                c"applicationShouldTerminateAfterLastWindowClosed:",
                terminate_after_last_window as extern "C" fn(Id, Sel, Id) -> Bool as *const c_void,
                c"c@:@",
            ),
            (c"applicationWillTerminate:", action(will_terminate), c"v@:@"),
            (c"windowDidResize:", action(did_resize), c"v@:@"),
            (c"windowDidChangeBackingProperties:", action(did_resize), c"v@:@"),
            (c"windowDidBecomeKey:", action(did_become_key), c"v@:@"),
            (c"windowDidResignKey:", action(did_resign_key), c"v@:@"),
            (c"windowWillClose:", action(will_close), c"v@:@"),
            (c"newWindow:", action(new_window), c"v@:@"),
        ])
    }
}

unsafe fn install_menu(ns_app: Id) {
    unsafe fn add_item(menu: Id, title: &str, action: &CStr, key: &str) {
        unsafe {
            let item: Id = msg!(
                msg!(class(c"NSMenuItem"), c"alloc"),
                c"initWithTitle:action:keyEquivalent:",
                nsstring(title),
                sel(action),
                nsstring(key)
            );
            call!(menu, c"addItem:", item);
        }
    }

    unsafe fn submenu(bar: Id, title: &str) -> Id {
        unsafe {
            let item = new_object(c"NSMenuItem");
            call!(bar, c"addItem:", item);
            let menu: Id = msg!(msg!(class(c"NSMenu"), c"alloc"), c"initWithTitle:", nsstring(title));
            call!(item, c"setSubmenu:", menu);
            menu
        }
    }

    unsafe {
        let bar = new_object(c"NSMenu");
        let app_menu = submenu(bar, "mightty");
        add_item(app_menu, "Quit mightty", c"terminate:", "q");
        let shell_menu = submenu(bar, "Shell");
        add_item(shell_menu, "New Window", c"newWindow:", "n");
        add_item(shell_menu, "Close Window", c"performClose:", "w");
        let edit_menu = submenu(bar, "Edit");
        add_item(edit_menu, "Copy", c"copy:", "c");
        add_item(edit_menu, "Paste", c"paste:", "v");
        call!(ns_app, c"setMainMenu:", bar);
    }
}
