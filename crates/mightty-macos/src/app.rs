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
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mightty_mux::{Host, Mux, Pane, PaneId, SpawnOptions};
use mightty_term::{MouseMode, Term};

use crate::config::{Config, OptionAsAlt};
use crate::ffi::*;
use crate::findbar::FindBar;
use crate::keys::{self, Mods, MouseAction};
use crate::latency;
use crate::open;
use crate::linkhint::{self, LinkHint};
use crate::renderer::{self, Highlights, HoveredLink, Renderer, Snapshot};

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
    /// After a key press, the next output redraw comes this early instead,
    /// so the echo doesn't wait out the refresh.
    echo_frame: Option<Instant>,
    /// Where the pane's shell started, for new tabs until it reports one (OSC 7).
    start_dir: Option<PathBuf>,
    /// Text an input method is composing (marked text), drawn at the
    /// cursor until it's committed.
    preedit: String,
    /// The input method used the key being handled (inserted or composed text).
    ime_used_key: bool,
    search: Option<Search>,
    /// The link under the pointer while Cmd is held, and where it goes.
    hover: Option<(HoveredLink, String)>,
    /// Shows `hover`'s target; made on first use.
    hint: Option<LinkHint>,
}

/// An open search bar and what it found.
struct Search {
    bar: FindBar,
    query: String,
    /// Oldest first, from `Term::search`; kept up to date as output arrives.
    matches: Vec<renderer::Selection>,
    /// The selected match. By value, so it stays put as matches before it
    /// come and go.
    current: Option<renderer::Selection>,
    /// `lines_scrolled()` and alternate screen at the last search: lines
    /// before that were in scrollback, which doesn't change.
    searched_to: u64,
    alt: bool,
}

impl Search {
    /// Brings `matches` up to date: everything for a new `query`, else
    /// only what output may have changed since the last time.
    fn refresh(&mut self, term: &Term, query: Option<String>) {
        let mut full = term.is_alt_screen() != self.alt;
        if let Some(query) = query.filter(|q| *q != self.query) {
            self.query = query;
            full = true;
        }
        let (start, found) = term.search_from(&self.query, if full { 0 } else { self.searched_to });
        // Rows that left scrollback take their matches with them.
        let first = if term.is_alt_screen() { term.lines_scrolled() } else { term.first_line() };
        self.matches.retain(|m| !full && m.0.0 < start && m.0.0 >= first);
        self.matches.extend(found);
        (self.searched_to, self.alt) = (term.lines_scrolled(), term.is_alt_screen());
        self.current = if full {
            self.matches.last().copied()
        } else {
            // Still there, or else the next one that is.
            self.current.and_then(|c| {
                let at = self.matches.partition_point(|m| *m < c);
                self.matches.get(at).or(self.matches.last()).copied()
            })
        };
    }

    /// Selects the match `step` away from the current one, wrapping around.
    fn step(&mut self, step: isize) {
        let n = self.matches.len() as isize;
        let Some(current) = self.current.filter(|_| n > 0) else { return };
        let at = self.matches.partition_point(|m| *m < current) as isize;
        self.current = Some(self.matches[(at + step).rem_euclid(n) as usize]);
    }

    /// Shows "3 of 17", counting from the oldest.
    fn update_count(&mut self) {
        let position = self.current.map(|c| (self.matches.partition_point(|m| *m < c) + 1, self.matches.len()));
        self.bar.show_count(position, !self.query.is_empty());
    }
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

/// The active keyboard layout is US or ABC, where Option's characters are
/// rarely needed (Ghostty's default for `macos-option-as-alt`).
fn us_keyboard_layout() -> bool {
    // SAFETY: Carbon calls; the copied source is released, the property is borrowed from it.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return true;
        }
        let id = string_from_ns(TISGetInputSourceProperty(source, kTISPropertyInputSourceID) as Id);
        CFRelease(source);
        matches!(id.as_str(), "com.apple.keylayout.US" | "com.apple.keylayout.ABC")
    }
}

/// NSEventModifierFlagCommand and NSEventModifierFlagShift.
const CMD: usize = 1 << 20;
const SHIFT: usize = 1 << 17;

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

/// How long after a key press output is gathered before drawing it: enough
/// for a shell's echo (under a millisecond), far less than a refresh.
const ECHO_WAIT: Duration = Duration::from_millis(2);

/// Draws panes with new output, at most once per display refresh each.
///
/// Waiting for the refresh here, rather than in `nextDrawable`, keeps the
/// main thread free for input while output streams in. Output after a pause
/// is drawn at once, and so is a typed key's echo (see `ECHO_WAIT`). Invariant: while `pending` isn't empty, a call to this
/// is scheduled.
extern "C" fn on_redraw(_: *mut c_void) {
    let Some(host) = HOST.get() else { return };
    let ids = mem::take(&mut *host.pending.lock().unwrap());
    let now = Instant::now();
    let mut wait: Option<(Duration, Vec<PaneId>)> = None;
    with_app(|app| {
        for window in app.windows.iter_mut().filter(|w| ids.contains(&w.pane.id)) {
            let due = window.echo_frame.map_or(window.next_frame, |at| at.min(window.next_frame));
            if due <= now {
                window.render();
            } else {
                let (delay, deferred) = wait.get_or_insert((Duration::MAX, Vec::new()));
                *delay = (*delay).min(due - now);
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
        self.open_window_in(pane, hold, NIL);
    }

    /// Opens a window for `pane`, as a new tab of `tab_of` unless that's nil.
    fn open_window_in(&mut self, pane: Arc<Pane>, hold: bool, tab_of: Id) {
        // SAFETY: main thread; AppKit calls with documented signatures.
        unsafe {
            let screen: Id = msg!(class(c"NSScreen"), c"mainScreen");
            let scale: f64 = if screen.is_null() { 2.0 } else { msg!(screen, c"backingScaleFactor") };
            let layer: Id = msg!(msg!(class(c"CAMetalLayer"), c"layer"), c"retain");
            let renderer = Renderer::new(layer, &self.config.font, self.config.font_size, scale, self.config.theme.clone(), self.config.vsync);

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
            // Native tabs: windows with one identifier can be tabs of each other.
            call!(window, c"setTabbingIdentifier:", nsstring("mightty"));

            let view: Id = msg!(msg!(self.view_class, c"alloc"), c"initWithFrame:", content);
            // Layer-hosting view: set the layer before enabling it.
            call!(view, c"setLayer:", layer);
            call!(view, c"setWantsLayer:", YES);
            // The terminal fills a container, which also holds the search bar above it.
            let container: Id = msg!(msg!(class(c"NSView"), c"alloc"), c"initWithFrame:", content);
            call!(container, c"setWantsLayer:", YES);
            call!(view, c"setAutoresizingMask:", 18usize); // width and height sizable
            // Mouse moves (for Cmd-hovering links) while the window is key.
            let options = 0x01usize | 0x02 | 0x20 | 0x200; // entered/exited, moved, active in key window, visible rect
            let area: Id = msg!(
                msg!(class(c"NSTrackingArea"), c"alloc"),
                c"initWithRect:options:owner:userInfo:",
                CGRect::default(),
                options,
                view,
                NIL
            );
            call!(view, c"addTrackingArea:", area);
            call!(area, c"release");
            call!(container, c"addSubview:", view);
            call!(window, c"setContentView:", container);
            call!(container, c"release");
            call!(view, c"release");
            call!(window, c"makeFirstResponder:", view);
            if self.windows.is_empty() {
                call!(window, c"center");
                latency::start(window);
            }
            if tab_of.is_null() {
                // With (0,0) this leaves the window in place and returns the next spot.
                self.cascade = msg!(window, c"cascadeTopLeftFromPoint:", self.cascade);
            } else {
                call!(tab_of, c"addTabbedWindow:ordered:", window, 1isize); // above: just after it
            }
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
                echo_frame: None,
                start_dir: None,
                preedit: String::new(),
                ime_used_key: false,
                search: None,
                hover: None,
                hint: None,
            };
            window.resize();
            self.windows.push(window);
        }
    }

    /// A shell in a new window, or with `tab` in a new tab of the key window.
    /// It starts where the key window's shell is (OSC 7), as in Ghostty.
    fn new_shell_window(&mut self, tab: bool) {
        // SAFETY: main thread; documented signature.
        let key: Id = unsafe { msg!(msg!(class(c"NSApplication"), c"sharedApplication"), c"keyWindow") };
        let current = self.windows.iter().find(|w| w.window == key);
        // Any program can report a directory; use it only if it is one.
        let cwd = current
            .and_then(|w| w.pane.term.lock().unwrap().cwd().map(PathBuf::from).or_else(|| w.start_dir.clone()))
            .filter(|dir| dir.is_dir());
        let tab_of = if tab { current.map_or(NIL, |w| w.window) } else { NIL };
        let opts = SpawnOptions { cols: self.config.cols, rows: self.config.rows, cwd: cwd.clone(), ..Default::default() };
        match self.mux.spawn(opts) {
            Ok(pane) => {
                self.open_window_in(pane, false, tab_of);
                if let Some(window) = self.windows.last_mut() {
                    window.start_dir = cwd;
                }
            }
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
                    // Output may have added or changed matches.
                    if let Some(search) = self.search.as_mut().filter(|s| !s.query.is_empty()) {
                        search.refresh(&term, None);
                    }
                    self.snapshot.capture(&term, self.scroll_offset);
                    if !self.preedit.is_empty() {
                        self.snapshot.overlay(&self.preedit, term.cursor());
                    }
                    Some(selection_range(self.selection, &term))
                }
            };
            // The terminal is unlocked again: build and draw the frame.
            if let Some(selection) = frame {
                if let Some(search) = &mut self.search {
                    search.update_count();
                }
                let link = self.hover.as_ref().map(|h| h.0);
                let highlights = match &self.search {
                    Some(search) => Highlights { selection, matches: &search.matches, current: search.current, link },
                    None => Highlights { selection, link, ..Highlights::default() },
                };
                let frame = self.renderer.prepare(&self.snapshot, highlights);
                let presented = latency::frame(self.window, &self.snapshot);
                self.renderer.present(frame, presented);
                let screen: Id = msg!(self.window, c"screen");
                let fps: isize = if screen.is_null() { 0 } else { msg!(screen, c"maximumFramesPerSecond") };
                self.next_frame = Instant::now() + Duration::from_secs(1) / fps.clamp(30, 240) as u32;
                self.echo_frame = None;
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

    /// Sends typed input, drawing its echo early (see `ECHO_WAIT`).
    fn type_bytes(&mut self, bytes: &[u8]) {
        self.write(bytes);
        if self.echo_frame.is_none() {
            self.echo_frame = Some(Instant::now() + ECHO_WAIT);
            // A redraw already waiting for the refresh would come too late.
            run_on_main_after(ECHO_WAIT, on_redraw);
        }
    }

    /// The cursor's cell in screen coordinates (points, origin bottom-left).
    fn cursor_rect(&self) -> CGRect {
        let (row, col) = self.pane.term.lock().unwrap().cursor();
        let scale = self.renderer.scale();
        let (cw, ch) = self.renderer.cell_size();
        let pad = self.renderer.padding_px();
        // SAFETY: main thread; documented signatures.
        unsafe {
            let bounds = send_rect(self.view, c"bounds");
            let cell = CGRect {
                origin: CGPoint {
                    x: (pad + col as f64 * cw as f64) / scale,
                    y: bounds.size.height - (pad + (row + 1) as f64 * ch as f64) / scale,
                },
                size: CGSize { width: cw as f64 / scale, height: ch as f64 / scale },
            };
            let in_window = send_rect_with(self.view, c"convertRect:toView:", (cell, NIL));
            send_rect_with(self.window, c"convertRectToScreen:", (in_window,))
        }
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

    /// Opens the find bar, or focuses it with its text selected.
    fn open_search(&mut self) {
        // SAFETY: main thread; documented signatures.
        unsafe {
            if let Some(search) = &self.search {
                call!(self.window, c"makeFirstResponder:", search.bar.field);
                call!(search.bar.field, c"selectText:", NIL);
                return;
            }
            let bar = FindBar::open(msg!(self.window, c"contentView"), self.view, self.dark());
            call!(self.window, c"makeFirstResponder:", bar.field);
            self.search = Some(Search { bar, query: String::new(), matches: Vec::new(), current: None, searched_to: 0, alt: false });
        }
    }

    fn close_search(&mut self) {
        if self.search.take().is_none() {
            return;
        }
        // SAFETY: main thread; documented signature.
        unsafe { call!(self.window, c"makeFirstResponder:", self.view) };
        self.render();
    }

    /// Searches for the bar's text: from the newest match when it changed,
    /// else moving `step` matches from the selected one (-1: older).
    fn search(&mut self, step: Option<isize>) {
        let Some(search) = &mut self.search else { return };
        // SAFETY: main thread; documented signature.
        let query = unsafe { string_from_ns(msg!(search.bar.field, c"stringValue")) };
        let changed = query != search.query;
        search.refresh(&self.pane.term.lock().unwrap(), Some(query));
        if let Some(step) = step.filter(|_| !changed) {
            if search.matches.is_empty() {
                // SAFETY: plain AppKit call.
                unsafe { NSBeep() };
            }
            search.step(step);
        }
        if let Some(current) = search.current {
            self.reveal(current.0.0);
        }
        self.render();
    }

    /// Scrolls so `line` (absolute) is on screen, centred if it wasn't.
    fn reveal(&mut self, line: u64) {
        let term = self.pane.term.lock().unwrap();
        if term.is_alt_screen() {
            return;
        }
        let (scrolled, rows, history) = (term.lines_scrolled(), term.rows() as u64, term.scrollback().len());
        let top = scrolled - self.scroll_offset as u64;
        if !(top..top + rows).contains(&line) {
            self.scroll_offset = ((scrolled + rows / 2).saturating_sub(line) as usize).min(history);
        }
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

    /// A dark terminal theme, for the overlays to match.
    fn dark(&self) -> bool {
        let (r, g, b) = self.renderer.theme().background;
        let luma = 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
        luma < 128.0
    }

    /// Finds the link under the pointer while Cmd is held (`cmd`): an OSC 8
    /// hyperlink or a URL in the text. Underlines it, shows where it goes
    /// and turns the pointer into a hand.
    fn update_hover(&mut self, cmd: bool) {
        // SAFETY: main thread; documented signatures.
        let location: CGPoint = unsafe { msg!(self.window, c"mouseLocationOutsideOfEventStream") };
        let bounds = unsafe { send_rect(self.view, c"bounds") };
        let inside = (0.0..bounds.size.width).contains(&location.x) && (0.0..bounds.size.height).contains(&location.y);
        let hover = (cmd && inside)
            .then(|| {
                let (col, row) = self.cell_at(location);
                let term = self.pane.term.lock().unwrap();
                let line = term.lines_scrolled() - self.scroll_offset as u64 + row as u64;
                match term.link_at(line, col) {
                    Some(id) => Some((HoveredLink::Id(id), term.link(id)?.to_owned())),
                    None => term.url_at(line, col).map(|(range, url)| (HoveredLink::Range(range), url)),
                }
            })
            .flatten();
        if hover == self.hover {
            return;
        }
        // SAFETY: main thread; documented signatures.
        unsafe {
            let cursor: Id = msg!(class(c"NSCursor"), if hover.is_some() { c"pointingHandCursor" } else { c"arrowCursor" });
            call!(cursor, c"set");
            if hover.is_some() && self.hint.is_none() {
                self.hint = Some(LinkHint::new(msg!(self.window, c"contentView"), self.dark()));
            }
        }
        if let Some(hint) = &self.hint {
            hint.show(hover.as_ref().map(|h| linkhint::describe(&h.1)).as_deref());
        }
        self.hover = hover;
        self.render();
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
            // Option not acting as Alt types what the layout gives it (å, @,
            // dead keys); keys it types nothing for, like arrows, still get Alt.
            let alt = mods.alt && {
                let setting = with_app(|app| app.config.option_as_alt).flatten();
                OptionAsAlt::applies(setting, flags, us_keyboard_layout)
            };
            // Text keys go through the input method (IME, dead keys), which
            // calls back with text to insert or to show as composing. Ctrl
            // and Alt chords skip it unless it's composing; keys it leaves
            // alone (arrows, Return, ...) are encoded below.
            let composing = with_window(view, |w| {
                w.ime_used_key = false;
                !w.preedit.is_empty()
            });
            if composing == Some(true) || !(mods.ctrl || alt) {
                let context: Id = msg!(view, c"inputContext");
                if !context.is_null() {
                    // Not inside `with_window`: the callbacks borrow the window.
                    let _: Bool = msg!(context, c"handleEvent:", event);
                }
                if with_window(view, |w| w.ime_used_key) == Some(true) {
                    return;
                }
            }
            let code: u16 = msg!(event, c"keyCode");
            let chars = string_from_ns(msg!(event, c"characters"));
            let base = string_from_ns(msg!(event, c"charactersIgnoringModifiers"));
            with_window(view, |w| {
                let app_cursor = w.pane.term.lock().unwrap().modes().app_cursor;
                if let Some(bytes) = keys::encode(code, &chars, &base, mods, app_cursor) {
                    w.type_bytes(&bytes);
                }
            });
        }
    }

    // ---- Search ----

    extern "C" fn find(view: Id, _: Sel, _sender: Id) {
        with_window(view, Window::open_search);
    }

    fn find_step(view: Id, step: isize) {
        with_window(view, |w| if w.search.is_some() { w.search(Some(step)) } else { w.open_search() });
    }
    extern "C" fn find_next(view: Id, _: Sel, _sender: Id) {
        find_step(view, -1);
    }
    extern "C" fn find_previous(view: Id, _: Sel, _sender: Id) {
        find_step(view, 1);
    }
    extern "C" fn close_search(view: Id, _: Sel, _sender: Id) {
        with_window(view, Window::close_search);
    }

    /// The search bar's text changed (the view is its delegate).
    extern "C" fn search_changed(view: Id, _: Sel, _notification: Id) {
        with_window(view, |w| w.search(None));
    }

    /// Keys in the search bar: Return steps to older matches (with Shift,
    /// newer), Escape closes it.
    extern "C" fn search_command(view: Id, _: Sel, _control: Id, _text_view: Id, command: Sel) -> Bool {
        if command == sel(c"insertNewline:") {
            // SAFETY: documented signatures.
            let shift = unsafe {
                let app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
                let event: Id = msg!(app, c"currentEvent");
                let flags: usize = if event.is_null() { 0 } else { msg!(event, c"modifierFlags") };
                flags & (1 << 17) != 0
            };
            find_step(view, if shift { 1 } else { -1 });
        } else if command == sel(c"cancelOperation:") {
            with_window(view, Window::close_search);
        } else {
            return NO;
        }
        YES
    }

    // ---- NSTextInputClient: what input methods call ----

    /// `string` is an NSString or NSAttributedString.
    unsafe fn text_of(string: Id) -> String {
        unsafe {
            let attributed: Bool = msg!(string, c"isKindOfClass:", class(c"NSAttributedString"));
            string_from_ns(if attributed != 0 { msg!(string, c"string") } else { string })
        }
    }

    extern "C" fn insert_text(view: Id, _: Sel, string: Id, _replace: NSRange) {
        // SAFETY: AppKit passes a string object.
        let text = unsafe { text_of(string) };
        with_window(view, |w| {
            w.ime_used_key = true;
            let had_preedit = !mem::take(&mut w.preedit).is_empty();
            w.type_bytes(text.as_bytes());
            if had_preedit {
                w.render();
            }
        });
    }

    extern "C" fn set_marked_text(view: Id, _: Sel, string: Id, _selected: NSRange, _replace: NSRange) {
        // SAFETY: AppKit passes a string object.
        let text = unsafe { text_of(string) };
        with_window(view, |w| {
            w.ime_used_key = true;
            w.preedit = text;
            w.render();
        });
    }

    extern "C" fn unmark_text(view: Id, _: Sel) {
        with_window(view, |w| {
            if !mem::take(&mut w.preedit).is_empty() {
                w.render();
            }
        });
    }

    extern "C" fn has_marked_text(view: Id, _: Sel) -> Bool {
        if with_window(view, |w| !w.preedit.is_empty()) == Some(true) { YES } else { NO }
    }

    extern "C" fn marked_range(view: Id, _: Sel) -> NSRange {
        let length = with_window(view, |w| w.preedit.encode_utf16().count()).unwrap_or(0);
        if length == 0 { NSRange { location: NS_NOT_FOUND, length: 0 } } else { NSRange { location: 0, length } }
    }

    extern "C" fn selected_range(_: Id, _: Sel) -> NSRange {
        NSRange { location: NS_NOT_FOUND, length: 0 }
    }

    extern "C" fn valid_attributes(_: Id, _: Sel) -> Id {
        // SAFETY: documented signature.
        unsafe { msg!(class(c"NSArray"), c"array") }
    }

    extern "C" fn attributed_substring(_: Id, _: Sel, _range: NSRange, _actual: *mut NSRange) -> Id {
        NIL
    }

    extern "C" fn character_index(_: Id, _: Sel, _point: CGPoint) -> usize {
        NS_NOT_FOUND
    }

    /// Where the input method puts its candidate window: the cursor's cell, in screen coordinates.
    extern "C" fn first_rect(view: Id, _: Sel, _range: NSRange, _actual: *mut NSRange) -> CGRect {
        with_window(view, |w| w.cursor_rect()).unwrap_or_default()
    }

    extern "C" fn do_command(_: Id, _: Sel, _command: Sel) {
        // Left to `key_down`, which encodes the key itself.
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
        // SAFETY: `event` is an NSEvent.
        let flags: usize = unsafe { msg!(event, c"modifierFlags") };
        if flags & CMD != 0 {
            // Cmd-click opens the link under the pointer; it never starts a
            // selection. Only the target the hint was showing opens: if output
            // changed what's under the pointer, the hint updates instead.
            let link = with_window(view, |w| {
                let shown = w.hover.clone();
                w.update_hover(true);
                (w.hover == shown).then(|| shown.map(|h| h.1)).flatten()
            });
            if let Some(uri) = link.flatten() {
                open::open(&uri);
            }
            return;
        }
        mouse_event(view, event, MouseAction::Press);
    }
    extern "C" fn mouse_moved(view: Id, _: Sel, event: Id) {
        // SAFETY: `event` is an NSEvent.
        let flags: usize = unsafe { msg!(event, c"modifierFlags") };
        with_window(view, |w| w.update_hover(flags & CMD != 0));
    }
    extern "C" fn mouse_exited(view: Id, _: Sel, _event: Id) {
        with_window(view, |w| w.update_hover(false));
    }
    /// Cmd pressed or released without the mouse moving.
    extern "C" fn flags_changed(view: Id, _: Sel, event: Id) {
        // SAFETY: `event` is an NSEvent.
        let flags: usize = unsafe { msg!(event, c"modifierFlags") };
        with_window(view, |w| w.update_hover(flags & CMD != 0));
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
        register_class_with(c"MighttyView", c"NSView", &[c"NSTextInputClient"], &[
            (c"acceptsFirstResponder", predicate(yes), c"c@:"),
            (c"isOpaque", predicate(yes), c"c@:"),
            (c"keyDown:", action(key_down), c"v@:@"),
            (c"scrollWheel:", action(scroll_wheel), c"v@:@"),
            (c"paste:", action(paste), c"v@:@"),
            (c"copy:", action(copy), c"v@:@"),
            (c"mouseDown:", action(mouse_down), c"v@:@"),
            (c"mouseDragged:", action(mouse_dragged), c"v@:@"),
            (c"mouseUp:", action(mouse_up), c"v@:@"),
            (c"mouseMoved:", action(mouse_moved), c"v@:@"),
            (c"mouseExited:", action(mouse_exited), c"v@:@"),
            (c"flagsChanged:", action(flags_changed), c"v@:@"),
            (c"insertText:replacementRange:", insert_text as *const c_void, c"v@:@{_NSRange=QQ}"),
            (
                c"setMarkedText:selectedRange:replacementRange:",
                set_marked_text as *const c_void,
                c"v@:@{_NSRange=QQ}{_NSRange=QQ}",
            ),
            (c"unmarkText", unmark_text as *const c_void, c"v@:"),
            (c"hasMarkedText", predicate(has_marked_text), c"c@:"),
            (c"markedRange", marked_range as *const c_void, c"{_NSRange=QQ}@:"),
            (c"selectedRange", selected_range as *const c_void, c"{_NSRange=QQ}@:"),
            (c"validAttributesForMarkedText", valid_attributes as *const c_void, c"@@:"),
            (
                c"attributedSubstringForProposedRange:actualRange:",
                attributed_substring as *const c_void,
                c"@@:{_NSRange=QQ}^{_NSRange=QQ}",
            ),
            (c"characterIndexForPoint:", character_index as *const c_void, c"Q@:{CGPoint=dd}"),
            (
                c"firstRectForCharacterRange:actualRange:",
                first_rect as *const c_void,
                c"{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}",
            ),
            (c"doCommandBySelector:", do_command as *const c_void, c"v@::"),
            (c"performFind:", action(find), c"v@:@"),
            (c"findNext:", action(find_next), c"v@:@"),
            (c"findPrevious:", action(find_previous), c"v@:@"),
            (c"searchOlder:", action(find_next), c"v@:@"),
            (c"searchNewer:", action(find_previous), c"v@:@"),
            (c"closeSearch:", action(close_search), c"v@:@"),
            (c"controlTextDidChange:", action(search_changed), c"v@:@"),
            (c"control:textView:doCommandBySelector:", search_command as *const c_void, c"c@:@@:"),
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
        with_app(|app| app.new_shell_window(false));
    }
    /// ⌘T, and the tab bar's + button (`newWindowForTab:`).
    extern "C" fn new_tab(_: Id, _: Sel, _sender: Id) {
        with_app(|app| app.new_shell_window(true));
    }
    /// ⌘1…⌘8 pick that tab, ⌘9 the last (the menu item's tag is the digit).
    extern "C" fn select_tab(_: Id, _: Sel, sender: Id) {
        // SAFETY: main thread; `sender` is the menu item; documented signatures.
        unsafe {
            let digit: isize = msg!(sender, c"tag");
            let key: Id = msg!(msg!(class(c"NSApplication"), c"sharedApplication"), c"keyWindow");
            if key.is_null() {
                return;
            }
            let tabs: Id = msg!(key, c"tabbedWindows");
            let count: usize = if tabs.is_null() { 1 } else { msg!(tabs, c"count") };
            let index = if digit == 9 { count - 1 } else { digit as usize - 1 };
            if !tabs.is_null() && index < count {
                let window: Id = msg!(tabs, c"objectAtIndex:", index);
                call!(window, c"makeKeyAndOrderFront:", NIL);
            }
        }
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
            (c"newTab:", action(new_tab), c"v@:@"),
            (c"newWindowForTab:", action(new_tab), c"v@:@"),
            (c"selectTabByDigit:", action(select_tab), c"v@:@"),
        ])
    }
}

unsafe fn install_menu(ns_app: Id) {
    unsafe fn add_item(menu: Id, title: &str, action: &CStr, key: &str) -> Id {
        unsafe {
            let item: Id = msg!(
                msg!(class(c"NSMenuItem"), c"alloc"),
                c"initWithTitle:action:keyEquivalent:",
                nsstring(title),
                sel(action),
                nsstring(key)
            );
            call!(menu, c"addItem:", item);
            item
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
        add_item(shell_menu, "New Tab", c"newTab:", "t");
        add_item(shell_menu, "Close Window", c"performClose:", "w");
        let edit_menu = submenu(bar, "Edit");
        add_item(edit_menu, "Copy", c"copy:", "c");
        add_item(edit_menu, "Paste", c"paste:", "v");
        add_item(edit_menu, "Find…", c"performFind:", "f");
        add_item(edit_menu, "Find Next", c"findNext:", "g");
        add_item(edit_menu, "Find Previous", c"findPrevious:", "G");
        // AppKit adds the tab items (Show Tab Bar, Move Tab to New Window, …) here.
        let window_menu = submenu(bar, "Window");
        add_item(window_menu, "Minimize", c"performMiniaturize:", "m");
        // ⌘⇧[ and ⌘⇧]: the unshifted key with Shift in the mask, since with
        // Cmd held the key may report "[" rather than "{".
        for (title, action, key) in [("Show Previous Tab", c"selectPreviousTab:", "["), ("Show Next Tab", c"selectNextTab:", "]")] {
            let item = add_item(window_menu, title, action, key);
            call!(item, c"setKeyEquivalentModifierMask:", CMD | SHIFT);
        }
        for digit in 1..=9isize {
            let title = if digit == 9 { "Select Last Tab".to_owned() } else { format!("Select Tab {digit}") };
            let item = add_item(window_menu, &title, c"selectTabByDigit:", &digit.to_string());
            call!(item, c"setTag:", digit);
        }
        call!(ns_app, c"setWindowsMenu:", window_menu);
        call!(ns_app, c"setMainMenu:", bar);
    }
}
