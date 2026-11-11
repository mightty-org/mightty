//! Cocoa application: windows showing panes from a [`Mux`].
//!
//! Threads: the main thread owns every Objective-C object and renderer. Pane
//! reader threads (inside the mux) parse output and notify [`GuiHost`],
//! which hands redraws and window opening/closing to the main thread through
//! libdispatch. The mux also serves the control socket, so `mightty ctl` and
//! AI agents can open, drive and read panes; panes they open get windows.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_void};
use std::fs;
use std::io;
use std::mem;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mightty_mux::{Host, Location, Mux, Pane, PaneId, Placement, SpawnOptions, Status};
use mightty_term::{ClipboardRequest, KEY_EVENT_TYPES, MouseMode, PointerShape, Term, clipboard_reply};

use crate::bindings::{self, Action, Bindings, Trigger};
use crate::config::{self, Behaviour, Colorspace, Config, ConfirmClose, MAX_FONT_SIZE, MIN_FONT_SIZE, OptionAsAlt, Permission, SaveState};
use crate::ffi::*;
use crate::findbar::FindBar;
use crate::keys::{self, Key, KeyAction, Mods, MouseAction};
use crate::latency;
use crate::open;
use crate::linkhint::{self, LinkHint};
use crate::notify;
use crate::session::{self, Layout, Session};
use crate::renderer::{self, Highlights, HoveredLink, Renderer, Snapshot};
use crate::split::{self, Axis, Direction, Divider, Node, Rect};

struct App {
    mux: Arc<Mux>,
    windows: Vec<Window>,
    surfaces: Vec<Surface>,
    /// App delegate and delegate of every window.
    delegate: Id,
    view_class: Id,
    split_class: Id,
    config: Config,
    /// Where the next window's top-left corner goes.
    cascade: CGPoint,
    /// Panes needing the user at the last count, to bounce the Dock icon for new ones.
    attention_count: usize,
    /// Panes with a banner up, and since when they needed the user.
    banners: Vec<(PaneId, Instant)>,
    /// The session last written (see `window-save-state`).
    saved: String,
}

/// Points between split panes, drawn as a divider.
const GAP: f64 = 1.0;
/// How far either side of a divider grabs it.
const GRAB: f64 = 3.0;
/// How far ⌘⌃ and an arrow moves a divider, in points (as in Ghostty).
const RESIZE_STEP: f64 = 10.0;
/// Panes are never made smaller than this by dragging, in points.
const MIN_PANE: f64 = 60.0;
/// NSWindowOcclusionStateVisible.
const NS_WINDOW_OCCLUSION_STATE_VISIBLE: usize = 1 << 1;
/// Around a pane that needs the user, in points.
const ATTENTION_BORDER: f64 = 2.0;
/// How long a blinking cursor stays on, and off.
const BLINK_INTERVAL: Duration = Duration::from_millis(600);
/// How often a selection dragged past the top or bottom edge scrolls.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// An NSWindow (or tab) and how its panes share it.
struct Window {
    window: Id,
    /// The content view, a `MighttySplitView`: holds each pane's container,
    /// showing through between them as dividers.
    root: Id,
    /// The panes, by their views.
    tree: Node<Id>,
    /// The view of the pane keys go to.
    active: Id,
    /// The active pane alone fills the window (⌘⇧↩).
    zoomed: bool,
    /// The divider being dragged.
    dragging: Option<Divider>,
}

/// One pane shown in a window: its view, drawing and what's on screen.
struct Surface {
    window: Id,
    /// Holds the view, and the find bar and link hint above it.
    container: Id,
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
    /// When the user last denied a program here the clipboard.
    clipboard_refused: Option<Instant>,
    /// The mouse button that started `selection` is still down.
    dragging: bool,
    /// While dragging past the top (> 0) or bottom edge: lines to scroll
    /// per `AUTOSCROLL_INTERVAL`.
    autoscroll: isize,
    /// Output redraws wait until then: one frame per display refresh.
    next_frame: Instant,
    /// After a key press, the next output redraw comes this early instead,
    /// so the echo doesn't wait out the refresh.
    echo_frame: Option<Instant>,
    /// When an animated image's next frame is due, to draw it then.
    animation_due: Option<Instant>,
    /// Where the pane's shell started, for new tabs until it reports one (OSC 7).
    start_dir: Option<PathBuf>,
    /// Text an input method is composing (marked text), drawn at the
    /// cursor until it's committed.
    preedit: String,
    /// The input method used the key being handled (inserted or composed text).
    ime_used_key: bool,
    /// While handling a key for a program using the kitty keyboard
    /// protocol: text the input method types is kept in `typed` to be sent
    /// as that key, not as plain text.
    capture: bool,
    typed: Option<String>,
    /// Keys whose presses the program was told of, so it hears their releases.
    keys_down: Vec<u16>,
    search: Option<Search>,
    /// The link under the pointer while Cmd is held, and where it goes.
    hover: Option<(HoveredLink, String)>,
    /// The pointer's shape a program asked for (OSC 22), shown over the
    /// terminal except over a link.
    pointer: PointerShape,
    /// Shows `hover`'s target; made on first use.
    hint: Option<LinkHint>,
    /// The pane keys go to in its window (it shows the window's title).
    active: bool,
    /// Some pane in its window needs the user (see `App::refresh_attention`).
    window_attention: bool,
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

/// NSEventModifierFlagCommand, …Shift, …Control and …Option.
const CMD: usize = 1 << 20;
const SHIFT: usize = 1 << 17;
const CTRL: usize = 1 << 18;
const ALT: usize = 1 << 19;
/// NSEventModifierFlagCapsLock.
const CAPS: usize = 1 << 16;

fn modifiers(flags: usize) -> Mods {
    Mods {
        shift: flags & SHIFT != 0,
        ctrl: flags & CTRL != 0,
        alt: flags & ALT != 0,
        cmd: flags & CMD != 0,
        caps_lock: flags & CAPS != 0,
    }
}

/// The device-dependent bit in `modifierFlags` that is set while each
/// modifier key (by key code) is down; Caps Lock's is its lock state.
const MODIFIER_KEYS: [(u16, usize); 9] =
    [(56, 0x2), (60, 0x4), (59, 0x1), (62, 0x2000), (58, 0x20), (61, 0x40), (55, 0x8), (54, 0x10), (57, CAPS)];

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

/// Runs `f` on the surface whose view is `view`.
fn with_surface<R>(view: Id, f: impl FnOnce(&mut Surface) -> R) -> Option<R> {
    with_app(|app| app.surfaces.iter_mut().find(|w| w.view == view).map(f)).flatten()
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

    fn show_pane(&self, id: PaneId, placement: Placement) -> bool {
        run_on_main_sync(|| with_app(|app| app.show_pane(id, placement)).unwrap_or(false))
    }

    fn locations(&self) -> Vec<(PaneId, Location)> {
        run_on_main_sync(|| with_app(|app| app.locations()).unwrap_or_default())
    }

    fn focus_pane(&self, id: PaneId) -> bool {
        run_on_main_sync(|| with_app(|app| app.focus_pane(id)).unwrap_or(false))
    }

    fn pane_closed(&self, id: PaneId) {
        run_on_main_with(id as usize, on_pane_closed);
    }

    fn pane_status(&self, id: PaneId) {
        run_on_main_with(id as usize, on_pane_status);
    }
}

/// How often the session is saved, when it changed: directories change
/// without any event mightty watches.
const SAVE_INTERVAL: Duration = Duration::from_secs(5);

extern "C" fn on_save_tick(_: *mut c_void) {
    with_app(App::save_session);
    run_on_main_after(SAVE_INTERVAL, on_save_tick);
}

thread_local! {
    /// `config.behaviour`, for panes, which can't reach the app while it's
    /// borrowed for them; kept up to date on reload.
    static BEHAVIOUR: Cell<Behaviour> = Cell::new(Behaviour::default());
    /// A clipboard request waiting on a sheet's answer, and its pane.
    static ASKING: RefCell<Option<(Id, ClipboardRequest)>> = const { RefCell::new(None) };
    /// A paste waiting on a sheet's answer: its pane and bytes.
    static PASTING: RefCell<Option<(Id, Vec<u8>)>> = const { RefCell::new(None) };
}

thread_local! {
    /// Blinking cursors are in their off phase; and the last key typed,
    /// which keeps them on for a moment.
    static BLINK: Cell<(bool, Instant)> = Cell::new((false, Instant::now()));
}

/// Blinks the cursors that blink, then waits for the next phase.
extern "C" fn on_blink_tick(_: *mut c_void) {
    let (off, typed) = BLINK.get();
    BLINK.set((!off && typed.elapsed() >= BLINK_INTERVAL, typed));
    with_app(|app| {
        for surface in app.surfaces.iter_mut().filter(|s| s.renderer.cursor_blinks(&s.snapshot)) {
            surface.render();
        }
    });
    run_on_main_after(BLINK_INTERVAL, on_blink_tick);
}

thread_local! {
    /// When `on_animation_tick` is next scheduled to run.
    static ANIMATION_TICK: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Has `on_animation_tick` run by `due`, unless it already will sooner.
fn schedule_animation(due: Instant) {
    if ANIMATION_TICK.get().is_some_and(|at| at <= due) {
        return;
    }
    ANIMATION_TICK.set(Some(due));
    run_on_main_after(due.saturating_duration_since(Instant::now()), on_animation_tick);
}

/// Draws the panes whose animated images have a frame due, then waits for the next.
extern "C" fn on_animation_tick(_: *mut c_void) {
    let now = Instant::now();
    // Superseded by a sooner tick, which scheduled this one's successor.
    if ANIMATION_TICK.get().is_none_or(|at| at > now + Duration::from_millis(1)) {
        return;
    }
    ANIMATION_TICK.set(None);
    with_app(|app| {
        let soon = now + Duration::from_millis(1);
        for surface in app.surfaces.iter_mut().filter(|s| s.animation_due.is_some_and(|due| due <= soon)) {
            surface.render();
        }
        if let Some(next) = app.surfaces.iter().filter_map(|s| s.animation_due).min() {
            schedule_animation(next);
        }
    });
}

thread_local! {
    /// `on_autoscroll_tick` is scheduled.
    static AUTOSCROLLING: Cell<bool> = const { Cell::new(false) };
}

/// Scrolls the selections dragged past an edge, extending them to the
/// pointer, while any are.
extern "C" fn on_autoscroll_tick(_: *mut c_void) {
    let more = with_app(|app| {
        let mut more = false;
        for surface in app.surfaces.iter_mut().filter(|s| s.autoscroll != 0) {
            let (alt, history) = {
                let term = surface.pane.term.lock().unwrap();
                (term.is_alt_screen(), term.scrollback().len())
            };
            if !alt {
                surface.scroll_offset = surface.scroll_offset.saturating_add_signed(surface.autoscroll).min(history);
            }
            // SAFETY: main thread; documented signature.
            let location = unsafe { msg!(surface.window, c"mouseLocationOutsideOfEventStream") };
            surface.drag_selection(location);
            more |= surface.autoscroll != 0;
        }
        more
    });
    // Busy: try again next tick.
    let more = more.unwrap_or(true);
    AUTOSCROLLING.set(more);
    if more {
        run_on_main_after(AUTOSCROLL_INTERVAL, on_autoscroll_tick);
    }
}

/// After macOS changed between light and dark mode (with a moment for the
/// preference to be written): the settings again, if a theme follows it.
extern "C" fn on_appearance_changed(_: *mut c_void) {
    with_app(|app| {
        if app.config.follows_appearance {
            app.reload_config();
        }
    });
}

extern "C" fn on_reload_signal(_: *mut c_void) {
    with_app(App::reload_config);
}

extern "C" fn on_pane_status(_: *mut c_void) {
    with_app(App::refresh_attention);
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
        for window in app.surfaces.iter_mut().filter(|w| ids.contains(&w.pane.id)) {
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
    let views = with_app(|app| {
        let mut views = Vec::new();
        for surface in app.surfaces.iter_mut().filter(|w| w.pane.id == id) {
            if surface.hold {
                surface.render(); // the title now says it exited
            } else {
                views.push(surface.view);
            }
        }
        views
    });
    close_surfaces(views.unwrap_or_default());
}

extern "C" fn on_pane_closed(context: *mut c_void) {
    let id = context as PaneId;
    let views = with_app(|app| app.surfaces.iter().filter(|w| w.pane.id == id).map(|w| w.view).collect());
    close_surfaces(views.unwrap_or_default());
}

/// Closes these panes, and windows they were the last of: those outside
/// `with_app`, as closing re-enters through windowWillClose:.
fn close_surfaces(views: Vec<Id>) {
    for view in views {
        if let Some(window) = with_app(|app| app.close_surface(view)).flatten() {
            // SAFETY: main thread; a live NSWindow.
            unsafe { call!(window, c"close") };
        }
    }
}

/// Opens the first window and runs the Cocoa event loop. Returns only on setup errors.
pub fn run(config: Config) -> io::Result<()> {
    // SAFETY: AppKit calls on the main thread with their documented signatures.
    unsafe {
        objc_autoreleasePoolPush(); // outermost pool; the event loop adds its own per event

        let ns_app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
        call!(ns_app, c"setActivationPolicy:", 0isize); // regular app: Dock icon, menu bar
        install_menu(ns_app, &config.keybinds);
        let delegate: Id = msg!(register_delegate_class(), c"new");
        call!(ns_app, c"setDelegate:", delegate);
        notify::set_delegate(delegate);

        let host = Arc::new(GuiHost::default());
        let _ = HOST.set(host.clone());
        let mux = Mux::with_options(host, config.term_options());
        if let Err(e) = mightty_mux::listen(mux.clone()) {
            eprintln!("mightty: control socket unavailable: {e}");
        }
        // The first pane starts before its window: give it the cell size now.
        let screen: Id = msg!(class(c"NSScreen"), c"mainScreen");
        let scale: f64 = if screen.is_null() { 2.0 } else { msg!(screen, c"backingScaleFactor") };
        let (cw, ch) = renderer::cell_size_for(&config, scale);
        mightty_mux::set_cell_pixels(cw, ch);
        let first = SpawnOptions {
            command: config.command.clone(),
            // A command runs under plain sh, not the user's login shell and profile.
            shell: config.command.as_ref().map(|_| "/bin/sh".into()),
            cwd: config.cwd.clone(),
            cols: config.cols,
            rows: config.rows,
            ..Default::default()
        };
        // A window running a command (-e) is opened alone; otherwise the last session's come back.
        let restore = config.command.is_none();

        BEHAVIOUR.set(config.behaviour);
        let app = App {
            mux,
            windows: Vec::new(),
            surfaces: Vec::new(),
            delegate,
            view_class: register_view_class(),
            split_class: register_split_class(),
            config,
            cascade: CGPoint::default(),
            attention_count: 0,
            banners: Vec::new(),
            saved: String::new(),
        };
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        if !(restore && with_app(App::restore) == Some(true)) {
            let pane = with_app(|app| app.mux.spawn(first)).expect("the app was just set")?;
            with_app(|app| app.open_window(pane, false));
        }
        run_on_main_after(SAVE_INTERVAL, on_save_tick);
        // As in Ghostty: `kill -USR2` reloads the configuration.
        const SIGUSR2: i32 = 31;
        on_signal_run_on_main(SIGUSR2, on_reload_signal);
        // Light and dark mode changing, for `theme = light:…,dark:…`.
        let center: Id = msg!(class(c"NSDistributedNotificationCenter"), c"defaultCenter");
        let name = nsstring("AppleInterfaceThemeChangedNotification");
        call!(center, c"addObserver:selector:name:object:", delegate, sel(c"appearanceChanged:"), name, NIL);
        run_on_main_after(BLINK_INTERVAL, on_blink_tick);
        call!(ns_app, c"activateIgnoringOtherApps:", YES);
        call!(ns_app, c"run");
    }
    Ok(())
}

impl App {
    fn open_window(&mut self, pane: Arc<Pane>, hold: bool) {
        self.open_window_in(pane, hold, NIL, true);
    }

    /// Opens a window for `pane`, as a new tab of `tab_of` unless that's nil;
    /// it becomes the key window with `focus` (a tab is otherwise added behind).
    fn open_window_in(&mut self, pane: Arc<Pane>, hold: bool, tab_of: Id, focus: bool) {
        // SAFETY: main thread; AppKit calls with documented signatures.
        unsafe {
            let screen: Id = msg!(class(c"NSScreen"), c"mainScreen");
            let scale: f64 = if screen.is_null() { 2.0 } else { msg!(screen, c"backingScaleFactor") };
            let renderer = self.new_renderer(scale);
            let (cols, rows) = {
                let term = pane.term.lock().unwrap();
                (term.cols(), term.rows())
            };
            let (cw, ch) = renderer.cell_size();
            let (px, py) = renderer.padding_px();
            let content = CGRect {
                origin: CGPoint::default(),
                size: CGSize {
                    width: (cols as f64 * cw as f64 + 2.0 * px) / scale,
                    height: (rows as f64 * ch as f64 + 2.0 * py) / scale,
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

            let root: Id = msg!(msg!(self.split_class, c"alloc"), c"initWithFrame:", content);
            call!(root, c"setWantsLayer:", YES);
            call!(window, c"setContentView:", root);
            call!(root, c"release");
            let view = self.add_surface(window, content, renderer, pane, hold, true);
            self.windows.push(Window { window, root, tree: Node::Leaf(view), active: view, zoomed: false, dragging: None });
            call!(window, c"makeFirstResponder:", view);
            if self.windows.len() == 1 {
                call!(window, c"center");
                latency::start(window);
            }
            if tab_of.is_null() {
                // With (0,0) this leaves the window in place and returns the next spot.
                self.cascade = msg!(window, c"cascadeTopLeftFromPoint:", self.cascade);
            } else {
                call!(tab_of, c"addTabbedWindow:ordered:", window, 1isize); // above: just after it
            }
            if focus || tab_of.is_null() {
                call!(window, c"makeKeyAndOrderFront:", NIL);
            }
            // After it's on screen: the blur needs the window server's window.
            style_window(window, root, &self.config);
            self.layout(window, true);
            self.sync_key_focus();
        }
    }

    fn new_renderer(&self, scale: f64) -> Renderer {
        // SAFETY: main thread; documented signatures.
        let layer: Id = unsafe { msg!(msg!(class(c"CAMetalLayer"), c"layer"), c"retain") };
        Renderer::new(layer, scale, &self.config)
    }

    /// Adds a view for `pane` to `window`, at `frame` of its content view
    /// until the next layout; `active` if keys are to go to it. Returns the view.
    fn add_surface(&mut self, window: Id, frame: CGRect, mut renderer: Renderer, pane: Arc<Pane>, hold: bool, active: bool) -> Id {
        // SAFETY: main thread; AppKit calls with documented signatures.
        unsafe {
            let layer = renderer.layer();
            let bounds = CGRect { origin: CGPoint::default(), size: frame.size };
            let view: Id = msg!(msg!(self.view_class, c"alloc"), c"initWithFrame:", bounds);
            // Layer-hosting view: set the layer before enabling it.
            call!(view, c"setLayer:", layer);
            call!(view, c"setWantsLayer:", YES);
            // The terminal fills a container, which also holds the search bar above it.
            // Its background shows through a dimmed terminal.
            let container: Id = msg!(msg!(class(c"NSView"), c"alloc"), c"initWithFrame:", frame);
            call!(container, c"setWantsLayer:", YES);
            call!(msg!(container, c"layer"), c"setBackgroundColor:", pane_background(&self.config));
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
            call!(msg!(window, c"contentView"), c"addSubview:", container);
            call!(container, c"release");
            call!(view, c"release");

            pane.set_visible(true);
            renderer.focused = active;
            self.surfaces.push(Surface {
                window,
                container,
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
                clipboard_refused: None,
                dragging: false,
                autoscroll: 0,
                next_frame: Instant::now(),
                echo_frame: None,
                animation_due: None,
                start_dir: None,
                preedit: String::new(),
                ime_used_key: false,
                capture: false,
                typed: None,
                keys_down: Vec::new(),
                search: None,
                hover: None,
                pointer: PointerShape::Default,
                hint: None,
                active,
                window_attention: false,
            });
            view
        }
    }

    /// The key window and its active pane.
    fn key_window(&self) -> Option<(Id, Id)> {
        // SAFETY: main thread; documented signature.
        let key: Id = unsafe { msg!(msg!(class(c"NSApplication"), c"sharedApplication"), c"keyWindow") };
        self.windows.iter().find(|w| w.window == key).map(|w| (w.window, w.active))
    }

    /// Where a shell started from `surface` should start: where its shell
    /// is (OSC 7), as in Ghostty, else where it started.
    fn cwd_of(&self, surface: &Surface) -> Option<PathBuf> {
        // Any program can report a directory: use it only if it is on this
        // machine (not from a shell over ssh) and is one.
        let reported = {
            let term = surface.pane.term.lock().unwrap();
            term.cwd().filter(|_| open::is_local(term.cwd_host(), &open::hostname())).map(PathBuf::from)
        };
        reported.or_else(|| surface.start_dir.clone()).filter(|dir| dir.is_dir())
    }

    /// A shell in a new window, or with `tab` in a new tab of the key window.
    fn new_shell_window(&mut self, tab: bool) {
        let key = self.key_window();
        let current = key.and_then(|(_, view)| self.surfaces.iter().find(|s| s.view == view));
        let cwd = current.and_then(|s| self.cwd_of(s));
        let tab_of = if tab { key.map_or(NIL, |k| k.0) } else { NIL };
        let opts = SpawnOptions { cols: self.config.cols, rows: self.config.rows, cwd: cwd.clone(), ..Default::default() };
        match self.mux.spawn(opts) {
            Ok(pane) => {
                self.open_window_in(pane, false, tab_of, true);
                if let Some(surface) = self.surfaces.last_mut() {
                    surface.start_dir = cwd;
                }
            }
            Err(e) => eprintln!("mightty: could not start a shell: {e}"),
        }
    }

    /// Splits the key window's active pane, a new shell going right of it
    /// (`Axis::Row`) or below it.
    fn split(&mut self, axis: Axis) {
        let Some((window, active)) = self.key_window() else { return };
        let Some(current) = self.surfaces.iter().find(|s| s.view == active) else { return };
        let cwd = self.cwd_of(current);
        // SAFETY: main thread; documented signatures.
        let (frame, scale): (CGRect, f64) = unsafe { (send_rect(current.container, c"frame"), msg!(window, c"backingScaleFactor")) };
        // The new pane gets about half: start the shell at that size.
        let mut size = frame.size;
        match axis {
            Axis::Row => size.width = (size.width - GAP) / 2.0,
            Axis::Column => size.height = (size.height - GAP) / 2.0,
        }
        let (cols, rows) = current.renderer.grid_size(size.width * scale, size.height * scale);
        let opts = SpawnOptions { cols: cols as u16, rows: rows as u16, cwd: cwd.clone(), ..Default::default() };
        let pane = match self.mux.spawn(opts) {
            Ok(pane) => pane,
            Err(e) => return eprintln!("mightty: could not start a shell: {e}"),
        };
        let view = self.insert_split(active, pane, axis, false, true);
        if let Some(surface) = self.surfaces.iter_mut().find(|s| Some(s.view) == view) {
            surface.start_dir = cwd;
        }
    }

    /// Splits `at`'s space with a view for `pane` (right of or below it),
    /// which keys go to with `focus`. Returns the view.
    fn insert_split(&mut self, at: Id, pane: Arc<Pane>, axis: Axis, hold: bool, focus: bool) -> Option<Id> {
        let current = self.surfaces.iter().find(|s| s.view == at)?;
        let window = current.window;
        // SAFETY: main thread; documented signatures.
        let (frame, scale): (CGRect, f64) = unsafe { (send_rect(current.container, c"frame"), msg!(window, c"backingScaleFactor")) };
        let renderer = self.new_renderer(scale);
        let view = self.add_surface(window, frame, renderer, pane, hold, focus);
        let w = self.windows.iter_mut().find(|w| w.window == window)?;
        w.tree.split(at, view, axis);
        w.zoomed = false;
        if focus {
            self.focus(view);
        }
        self.layout(window, false);
        Some(view)
    }

    /// Shows pane `id` for an agent, kept open after it exits so its output
    /// stays readable. Splits and tabs leave the keyboard where it is: the
    /// user may be typing to the agent.
    fn show_pane(&mut self, id: PaneId, placement: Placement) -> bool {
        let Some(pane) = self.mux.pane(id) else { return false };
        // Next to `beside`, else the pane the user is in.
        let near = |app: &Self, beside: Option<PaneId>| {
            let shown = beside.and_then(|id| app.surfaces.iter().find(|s| s.pane.id == id).map(|s| s.view));
            shown.or_else(|| app.key_window().map(|k| k.1)).or_else(|| app.windows.first().map(|w| w.active))
        };
        let near = match placement {
            Placement::Window => None,
            Placement::Tab { beside } | Placement::Split { beside, .. } => near(self, beside),
        };
        let window = near.and_then(|view| self.surfaces.iter().find(|s| s.view == view)).map(|s| s.window);
        match (placement, near, window) {
            (Placement::Split { down, .. }, Some(view), _) => {
                let axis = if down { Axis::Column } else { Axis::Row };
                self.insert_split(view, pane, axis, true, false);
            }
            (Placement::Tab { .. }, _, Some(window)) => self.open_window_in(pane, true, window, false),
            _ => self.open_window(pane, true),
        }
        true
    }

    /// Where each shown pane is, for `pane.list`.
    fn locations(&self) -> Vec<(PaneId, Location)> {
        let mut out = Vec::new();
        for w in &self.windows {
            // SAFETY: main thread; documented signatures.
            let (number, tab, key) = unsafe {
                let number: isize = msg!(w.window, c"windowNumber");
                let tabs: Id = msg!(w.window, c"tabbedWindows");
                let tab: usize = if tabs.is_null() { 0 } else { msg!(tabs, c"indexOfObject:", w.window) };
                let key: Bool = msg!(w.window, c"isKeyWindow");
                (number, if tab == NS_NOT_FOUND { 0 } else { tab }, key != NO)
            };
            for s in self.surfaces.iter().filter(|s| s.window == w.window) {
                out.push((s.pane.id, Location { window: number as u64, tab, focused: key && s.active }));
            }
        }
        out
    }

    /// Shows which panes need the user: a border on the pane, a mark on its
    /// window's title, a count on the Dock icon. A new one while mightty is
    /// in the background bounces the Dock icon once.
    fn refresh_attention(&mut self) {
        let mut windows = Vec::new();
        for surface in &mut self.surfaces {
            let attention = surface.pane.attention().is_some();
            // SAFETY: main thread; documented signatures.
            unsafe {
                let layer: Id = msg!(surface.container, c"layer");
                call!(layer, c"setBorderWidth:", if attention { ATTENTION_BORDER } else { 0.0 });
                if attention {
                    let orange: Id = msg!(class(c"NSColor"), c"systemOrangeColor");
                    let orange: Id = msg!(orange, c"CGColor");
                    call!(layer, c"setBorderColor:", orange);
                }
            }
            if attention {
                windows.push(surface.window);
            }
        }
        for surface in &mut self.surfaces {
            let marked = windows.contains(&surface.window);
            if surface.window_attention != marked {
                surface.window_attention = marked;
                surface.title.clear();
                surface.render();
            }
        }
        let queue = self.mux.attention_queue();
        let count = queue.len();
        // SAFETY: main thread; documented signatures.
        let active = unsafe {
            let ns_app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
            let tile: Id = msg!(ns_app, c"dockTile");
            call!(tile, c"setBadgeLabel:", if count == 0 { NIL } else { nsstring(&count.to_string()) });
            let active: Bool = msg!(ns_app, c"isActive");
            if count > self.attention_count && active == NO {
                let _: isize = msg!(ns_app, c"requestUserAttention:", 10usize); // informational: one bounce
            }
            active != NO
        };
        self.attention_count = count;
        self.update_banners(&queue, active);
    }

    /// A banner for each pane that started needing the user while they were
    /// in another app (in mightty, the marks above are enough), taken back
    /// once it no longer does.
    fn update_banners(&mut self, queue: &[Arc<Pane>], active: bool) {
        let current: Vec<(PaneId, Instant)> = queue.iter().filter_map(|p| Some((p.id, p.attention()?.since))).collect();
        self.banners.retain(|banner| {
            let kept = current.contains(banner);
            if !kept {
                notify::remove(banner.0);
            }
            kept
        });
        if active {
            return;
        }
        for pane in queue {
            let Some(attention) = pane.attention() else { continue };
            if self.banners.contains(&(pane.id, attention.since)) {
                continue;
            }
            let title = {
                let term = pane.term.lock().unwrap();
                let title = term.title().trim();
                if title.is_empty() { pane.command.clone() } else { title.to_owned() }
            };
            notify::post(pane.id, &title, &attention.reason.message());
            self.banners.push((pane.id, attention.since));
        }
    }

    /// ⌘⇧A: the pane that has needed the user longest.
    fn jump_to_attention(&mut self) {
        let shown = self.mux.attention_queue().into_iter().find(|p| self.surfaces.iter().any(|s| s.pane.id == p.id));
        match shown {
            Some(pane) => {
                self.focus_pane(pane.id);
            }
            // SAFETY: plain AppKit call.
            None => unsafe { NSBeep() },
        }
    }

    /// Brings pane `id`'s window (or tab) to the front, keys going to it.
    fn focus_pane(&mut self, id: PaneId) -> bool {
        let Some((window, view)) = self.surfaces.iter().find(|s| s.pane.id == id).map(|s| (s.window, s.view)) else {
            return false;
        };
        // SAFETY: main thread; documented signature.
        unsafe { call!(window, c"makeKeyAndOrderFront:", NIL) };
        self.focus(view);
        self.sync_key_focus();
        true
    }

    /// Updates which panes show as focused after key-window changes made
    /// inside `with_app`, which the window delegate couldn't see.
    fn sync_key_focus(&mut self) {
        for w in &self.windows {
            // SAFETY: main thread; documented signature.
            let key: Bool = unsafe { msg!(w.window, c"isKeyWindow") };
            let key = key != NO;
            for s in self.surfaces.iter_mut().filter(|s| s.window == w.window) {
                let focused = key && s.view == w.active;
                if s.renderer.focused != focused && s.view == w.active {
                    s.set_focused(focused);
                } else if s.pane.set_focused(focused) {
                    run_on_main_with(0, on_pane_status);
                }
            }
        }
    }

    /// Closes `view`'s pane. If it was its window's last, returns the window
    /// for the caller to close outside `with_app`.
    fn close_surface(&mut self, view: Id) -> Option<Id> {
        let i = self.surfaces.iter().position(|s| s.view == view)?;
        let window = self.surfaces[i].window;
        let w = self.windows.iter_mut().find(|w| w.window == window)?;
        let order = w.tree.leaves();
        if !w.tree.remove(view) {
            return Some(window);
        }
        w.zoomed = false;
        let was_active = w.active == view;
        let surface = self.surfaces.remove(i);
        // SAFETY: main thread; the window's content view holds the container.
        unsafe { call!(surface.container, c"removeFromSuperview") };
        self.release_pane(&surface);
        if was_active {
            // The pane before it, as in Ghostty.
            let at = order.iter().position(|&v| v == view).unwrap_or(0);
            self.focus(order[if at == 0 { 1 } else { at - 1 }]);
        }
        self.layout(window, false);
        None
    }

    /// Whether windows are kept for the next launch (`window-save-state`).
    fn save_enabled(&self) -> bool {
        match self.config.save_state {
            SaveState::Always => true,
            SaveState::Never => false,
            // SAFETY: documented signatures. The key is set (true) when
            // "Close windows when quitting an application" is off.
            SaveState::Default => unsafe {
                let defaults: Id = msg!(class(c"NSUserDefaults"), c"standardUserDefaults");
                let keep: Bool = msg!(defaults, c"boolForKey:", nsstring("NSQuitAlwaysKeepsWindows"));
                keep != NO
            },
        }
    }

    /// Writes the session if it changed; forgets it when saving is off.
    fn save_session(&mut self) {
        if !self.save_enabled() {
            if !self.saved.is_empty() || session::path().is_some_and(|p| p.exists()) {
                session::remove();
                self.saved.clear();
            }
            return;
        }
        let text = self.capture().to_json();
        if text != self.saved {
            match session::save(&text) {
                Ok(()) => self.saved = text,
                Err(e) => eprintln!("mightty: could not save windows: {e}"),
            }
        }
    }

    /// The windows as they are: each window with its tabs, in the order
    /// they were stacked, the front one last.
    fn capture(&self) -> Session {
        let mut groups = Vec::new();
        let mut seen = Vec::new();
        let mut front = None;
        for w in &self.windows {
            if seen.contains(&w.window) {
                continue;
            }
            // SAFETY: main thread; documented signatures.
            let (tabs, selected, frame, key) = unsafe {
                let group: Id = msg!(w.window, c"tabbedWindows");
                let count: usize = if group.is_null() { 0 } else { msg!(group, c"count") };
                let mut tabs: Vec<Id> = (0..count).map(|i| msg!(group, c"objectAtIndex:", i)).collect();
                if tabs.is_empty() {
                    tabs.push(w.window);
                }
                tabs.retain(|t| self.windows.iter().any(|w| w.window == *t));
                let tab_group: Id = msg!(w.window, c"tabGroup");
                let selected: Id = if tab_group.is_null() { w.window } else { msg!(tab_group, c"selectedWindow") };
                let frame = send_rect(selected, c"frame");
                let key = tabs.iter().any(|&t| {
                    let key: Bool = msg!(t, c"isKeyWindow");
                    key != NO
                });
                (tabs, selected, frame, key)
            };
            seen.extend(&tabs);
            let index = tabs.iter().position(|&t| t == selected).unwrap_or(0);
            let tabs: Vec<session::Tab> = tabs.iter().filter_map(|&t| self.capture_tab(t)).collect();
            if tabs.is_empty() {
                continue;
            }
            if key {
                front = Some(groups.len());
            }
            let f = frame;
            groups.push(session::Group { frame: [f.origin.x, f.origin.y, f.size.width, f.size.height], tabs, selected: index });
        }
        if let Some(i) = front {
            let group = groups.remove(i);
            groups.push(group);
        }
        Session { groups }
    }

    fn capture_tab(&self, window: Id) -> Option<session::Tab> {
        let w = self.windows.iter().find(|w| w.window == window)?;
        let layout = self.capture_layout(&w.tree);
        let focused = w.tree.leaves().iter().position(|&v| v == w.active).unwrap_or(0);
        Some(session::Tab { layout, focused, zoomed: w.zoomed })
    }

    fn capture_layout(&self, node: &Node<Id>) -> Layout {
        match node {
            Node::Leaf(view) => {
                let cwd = self.surfaces.iter().find(|s| s.view == *view).and_then(|s| self.cwd_of(s));
                Layout::Pane { cwd: cwd.map(|d| d.display().to_string()) }
            }
            Node::Split { axis, ratio, first, second } => Layout::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(self.capture_layout(first)),
                second: Box::new(self.capture_layout(second)),
            },
        }
    }

    /// Reopens the last session's windows, each pane a new shell in its old
    /// directory. Returns whether any window opened.
    fn restore(&mut self) -> bool {
        if !self.save_enabled() {
            return false;
        }
        let Some(saved) = session::load() else { return false };
        let mut any = false;
        for group in &saved.groups {
            let mut windows: Vec<Id> = Vec::new();
            for tab in &group.tabs {
                let tab_of = windows.last().copied().unwrap_or(NIL);
                if let Some(window) = self.restore_tab(tab, tab_of) {
                    if windows.is_empty() {
                        place(window, group.frame);
                    }
                    windows.push(window);
                }
            }
            if let Some(&window) = windows.get(group.selected).or(windows.last()) {
                // SAFETY: main thread; a live NSWindow.
                unsafe { call!(window, c"makeKeyAndOrderFront:", NIL) };
            }
            any |= !windows.is_empty();
        }
        self.sync_key_focus();
        any
    }

    /// A window (a tab of `tab_of` unless nil) laid out as `tab`. Returns it.
    fn restore_tab(&mut self, tab: &session::Tab, tab_of: Id) -> Option<Id> {
        let pane = self.spawn_shell(tab.layout.first_cwd())?;
        self.open_window_in(pane, false, tab_of, tab_of.is_null());
        let (window, first) = self.windows.last().map(|w| (w.window, w.active))?;
        self.started_in(first, tab.layout.first_cwd());
        self.restore_split(&tab.layout, first);
        let w = self.windows.iter_mut().find(|w| w.window == window)?;
        copy_ratios(&mut w.tree, &tab.layout);
        let order = w.tree.leaves();
        let focused = order[tab.focused.min(order.len() - 1)];
        w.zoomed = tab.zoomed && order.len() > 1;
        self.focus(focused);
        self.layout(window, false);
        Some(window)
    }

    /// Splits `view`'s space as `layout` says, a new shell in each new pane.
    fn restore_split(&mut self, layout: &Layout, view: Id) {
        let Layout::Split { axis, first, second, .. } = layout else { return };
        let Some(pane) = self.spawn_shell(second.first_cwd()) else { return };
        let Some(new) = self.insert_split(view, pane, *axis, false, false) else { return };
        self.started_in(new, second.first_cwd());
        self.restore_split(first, view);
        self.restore_split(second, new);
    }

    /// A login shell in `cwd` if it still exists, else home.
    fn spawn_shell(&mut self, cwd: Option<&str>) -> Option<Arc<Pane>> {
        let cwd = cwd.map(PathBuf::from).filter(|d| d.is_dir());
        let opts = SpawnOptions { cols: self.config.cols, rows: self.config.rows, cwd, ..Default::default() };
        match self.mux.spawn(opts) {
            Ok(pane) => Some(pane),
            Err(e) => {
                eprintln!("mightty: could not start a shell: {e}");
                None
            }
        }
    }

    /// Remembers where `view`'s shell started, for new tabs and splits
    /// until it reports its directory itself.
    fn started_in(&mut self, view: Id, cwd: Option<&str>) {
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.view == view) {
            surface.start_dir = cwd.map(PathBuf::from).filter(|d| d.is_dir());
        }
    }

    /// What to tell the user before closing `views`' panes, or `None` to
    /// close without asking (see `confirm-close-surface`).
    fn close_question(&self, views: &[Id]) -> Option<String> {
        let panes: Vec<&Arc<Pane>> = self.surfaces.iter().filter(|s| views.contains(&s.view)).map(|s| &s.pane).collect();
        let running: Vec<String> = panes
            .iter()
            .filter(|p| p.is_alive() && !matches!(p.status(), Status::Idle | Status::Exited))
            .map(|p| program(p))
            .collect();
        match (self.config.confirm_close, running.as_slice()) {
            (ConfirmClose::Never, _) | (ConfirmClose::Running, []) => None,
            (ConfirmClose::Always, []) => Some(String::new()),
            (_, [one]) => Some(format!("“{one}” is still running. Closing ends it.")),
            (_, [first, rest @ ..]) => {
                let others = if rest.len() == 1 { "1 other".to_owned() } else { format!("{} others", rest.len()) };
                Some(format!("“{first}” and {others} are still running. Closing ends them."))
            }
        }
    }

    /// Ends `closed`'s pane unless another surface shows it.
    fn release_pane(&self, closed: &Surface) {
        if !self.surfaces.iter().any(|s| s.pane.id == closed.pane.id) {
            closed.pane.set_visible(false);
            self.mux.close(closed.pane.id);
        }
    }

    /// Forgets a closed window, ending its panes.
    fn window_closed(&mut self, window: Id) {
        self.windows.retain(|w| w.window != window);
        let (closed, kept) = mem::take(&mut self.surfaces).into_iter().partition(|s| s.window == window);
        self.surfaces = kept;
        for surface in closed {
            self.release_pane(&surface);
        }
    }

    /// Makes `view` the pane keys go to: the first responder of its window.
    fn focus(&mut self, view: Id) {
        self.activate(view);
        let Some(window) = self.surfaces.iter().find(|s| s.view == view).map(|s| s.window) else { return };
        // SAFETY: main thread; documented signature. Its becomeFirstResponder
        // finds the app busy and leaves the work done above.
        unsafe { call!(window, c"makeFirstResponder:", view) };
    }

    /// Marks `view` as its window's active pane, the others as not.
    fn activate(&mut self, view: Id) {
        let Some(window) = self.surfaces.iter().find(|s| s.view == view).map(|s| s.window) else { return };
        let Some(w) = self.windows.iter_mut().find(|w| w.window == window) else { return };
        let zoomed_away = w.zoomed && w.active != view;
        w.active = view;
        if zoomed_away {
            w.zoomed = false;
        }
        // SAFETY: main thread; documented signature.
        let key: Bool = unsafe { msg!(window, c"isKeyWindow") };
        for surface in self.surfaces.iter_mut().filter(|s| s.window == window) {
            let active = surface.view == view;
            if surface.active != active {
                surface.active = active;
                surface.title.clear(); // the window shows the new one's
                surface.set_focused(active && key != NO);
            }
        }
        if zoomed_away {
            self.layout(window, false);
        } else {
            self.dim(window);
        }
    }

    /// Places `window`'s panes, resizing those whose size changed (all of
    /// them with `force`, for a new scale).
    fn layout(&mut self, window: Id, force: bool) {
        let Some(w) = self.windows.iter().find(|w| w.window == window) else { return };
        // SAFETY: main thread; documented signatures.
        unsafe {
            let bounds = to_rect(send_rect(w.root, c"bounds"));
            let frames = if w.zoomed { vec![(w.active, bounds)] } else { w.tree.frames(bounds, GAP) };
            for surface in self.surfaces.iter_mut().filter(|s| s.window == window) {
                let Some(&(_, frame)) = frames.iter().find(|f| f.0 == surface.view) else {
                    call!(surface.container, c"setHidden:", YES);
                    continue;
                };
                let frame = from_rect(frame);
                let old = send_rect(surface.container, c"frame");
                call!(surface.container, c"setHidden:", NO);
                call!(surface.container, c"setFrame:", frame);
                if force || old.size.width != frame.size.width || old.size.height != frame.size.height {
                    surface.resize();
                }
            }
            call!(window, c"invalidateCursorRectsForView:", w.root);
        }
        self.dim(window);
    }

    /// Loads the config files again (⌘⇧,) and applies them to what's open:
    /// colours, fonts, `window-vsync` and the terminals' scrollback.
    /// Window sizes and commands are for new windows; other settings are
    /// read when they're used.
    fn reload_config(&mut self) {
        let (config, warnings) = match self.config.reload() {
            Ok(loaded) => loaded,
            Err(e) => return show_problems("The configuration wasn't reloaded", &[e]),
        };
        self.mux.set_options(config.term_options());
        // Sizes changed with ⌘+ and ⌘- stay unless the font settings changed.
        let font = |c: &Config| (c.fonts.clone(), c.font_size, c.font_backend, c.ligatures);
        let font_changed = font(&config) != font(&self.config);
        set_menu_keys(&config.keybinds);
        for w in &self.windows {
            style_window(w.window, w.root, &config);
        }
        for surface in &mut self.surfaces {
            let size = if font_changed { config.font_size } else { surface.renderer.font_size() };
            surface.renderer.configure(&config, size);
            // SAFETY: main thread; documented signature.
            unsafe { call!(msg!(surface.container, c"layer"), c"setBackgroundColor:", pane_background(&config)) };
            // The cell size may have changed, so the grid too; hidden panes as well.
            surface.resize();
        }
        BEHAVIOUR.set(config.behaviour);
        self.config = config;
        let windows: Vec<Id> = self.windows.iter().map(|w| w.window).collect();
        for window in windows {
            self.dim(window);
        }
        if !warnings.is_empty() {
            show_problems("Some settings weren't applied", &warnings);
        }
    }

    /// ⌘+ and ⌘- (`delta` points), ⌘0 (`None`: back to `font-size`): the
    /// font size of the pane `view`.
    fn change_font_size(&mut self, view: Id, delta: Option<f64>) {
        let Some(surface) = self.surfaces.iter_mut().find(|s| s.view == view) else { return };
        let size = delta.map_or(self.config.font_size, |d| (surface.renderer.font_size() + d).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
        if surface.renderer.configure(&self.config, size) {
            surface.resize();
        }
    }

    /// Opens the config file in the default text editor (⌘,), first
    /// creating it, with a few lines on how it works, if there is none.
    fn open_config(&self) {
        let Some(path) = self.config.file().map(PathBuf::from).or_else(|| Some(config::config_dir()?.join("config"))) else {
            return show_problems("There's no config file", &["HOME is unset".into()]);
        };
        if !path.exists() {
            let created = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path, CONFIG_TEMPLATE));
            if let Err(e) = created {
                return show_problems("The config file couldn't be created", &[format!("{}: {e}", path.display())]);
            }
        }
        if let Err(e) = Command::new("/usr/bin/open").arg("-t").arg(&path).spawn() {
            show_problems("The config file couldn't be opened", &[format!("{}: {e}", path.display())]);
        }
    }

    /// Draws all of `window`'s panes again, at its current size.
    fn redraw_window(&mut self, window: Id) {
        self.layout(window, false);
        for surface in self.surfaces.iter_mut().filter(|s| s.window == window) {
            surface.render();
        }
    }

    /// Dims the panes keys aren't going to, when there are several.
    fn dim(&self, window: Id) {
        let Some(w) = self.windows.iter().find(|w| w.window == window) else { return };
        let several = !w.zoomed && matches!(w.tree, Node::Split { .. });
        for surface in self.surfaces.iter().filter(|s| s.window == window) {
            let opacity = if several && !surface.active { self.config.unfocused_split_opacity as f32 } else { 1.0 };
            // SAFETY: main thread; documented signature.
            unsafe { call!(surface.layer, c"setOpacity:", opacity) };
        }
    }

    /// Moves to the pane `step` places after the active one, in reading
    /// order, wrapping around.
    fn select_split(&mut self, step: isize) {
        let Some((window, active)) = self.key_window() else { return };
        let Some(w) = self.windows.iter().find(|w| w.window == window) else { return };
        let order = w.tree.leaves();
        let at = order.iter().position(|&v| v == active).unwrap_or(0) as isize;
        let next = order[(at + step).rem_euclid(order.len() as isize) as usize];
        self.focus(next);
    }

    fn select_split_toward(&mut self, direction: Direction) {
        let Some((window, active)) = self.key_window() else { return };
        let Some(w) = self.windows.iter().find(|w| w.window == window) else { return };
        // SAFETY: main thread; documented signature.
        let bounds = to_rect(unsafe { send_rect(w.root, c"bounds") });
        if let Some(next) = split::neighbor(&w.tree.frames(bounds, GAP), active, direction) {
            self.focus(next);
        }
    }

    /// ⌘⌃ and an arrow: moves the nearest divider `step` points that way
    /// (as Ghostty's `resize_split`).
    fn move_divider(&mut self, direction: Direction, step: f64) {
        let Some((window, active)) = self.key_window() else { return };
        let Some(w) = self.windows.iter_mut().find(|w| w.window == window).filter(|w| !w.zoomed) else { return };
        let (axis, points) = match direction {
            Direction::Left => (Axis::Row, -step),
            Direction::Right => (Axis::Row, step),
            Direction::Up => (Axis::Column, -step),
            Direction::Down => (Axis::Column, step),
        };
        // SAFETY: main thread; documented signature.
        let bounds = to_rect(unsafe { send_rect(w.root, c"bounds") });
        if w.tree.move_divider(active, axis, points, bounds, GAP, MIN_PANE) {
            self.layout(window, false);
        }
    }

    fn toggle_zoom(&mut self) {
        let Some((window, _)) = self.key_window() else { return };
        let Some(w) = self.windows.iter_mut().find(|w| w.window == window) else { return };
        if matches!(w.tree, Node::Split { .. }) {
            w.zoomed = !w.zoomed;
            self.layout(window, false);
        }
    }

    fn equalize(&mut self) {
        let Some((window, _)) = self.key_window() else { return };
        let Some(w) = self.windows.iter_mut().find(|w| w.window == window) else { return };
        w.tree.equalize();
        self.layout(window, false);
    }

    /// The divider of `root`'s window under `point` (in `root`'s coordinates).
    fn divider_at(&self, root: Id, point: CGPoint) -> Option<Divider> {
        let w = self.windows.iter().find(|w| w.root == root).filter(|w| !w.zoomed)?;
        // SAFETY: main thread; documented signature.
        let bounds = to_rect(unsafe { send_rect(root, c"bounds") });
        w.tree.dividers(bounds, GAP).into_iter().find(|d| grab_area(d).contains(point.x, point.y))
    }

    /// Drags the divider grabbed in `root` (see `divider_at`) to `point`.
    fn drag_divider(&mut self, root: Id, point: CGPoint) {
        let Some(w) = self.windows.iter_mut().find(|w| w.root == root) else { return };
        let Some(divider) = &w.dragging else { return };
        let ratio = split::ratio_at(divider, point.x, point.y, GAP, MIN_PANE);
        w.tree.set_ratio(&divider.path, ratio);
        let window = w.window;
        self.layout(window, false);
    }
}

/// Moves `window` to `frame` (x, y, width, height), if that's still
/// mostly on a screen: displays come and go between launches.
fn place(window: Id, [x, y, width, height]: [f64; 4]) {
    let frame = CGRect { origin: CGPoint { x, y }, size: CGSize { width, height } };
    // SAFETY: main thread; documented signatures.
    unsafe {
        let screens: Id = msg!(class(c"NSScreen"), c"screens");
        let count: usize = msg!(screens, c"count");
        let visible = (0..count).any(|i| {
            let screen: Id = msg!(screens, c"objectAtIndex:", i);
            let area = send_rect(screen, c"visibleFrame");
            let overlap_x = (x + width).min(area.origin.x + area.size.width) - x.max(area.origin.x);
            let overlap_y = (y + height).min(area.origin.y + area.size.height) - y.max(area.origin.y);
            overlap_x >= width.min(area.size.width) / 2.0 && overlap_y >= height.min(area.size.height) / 2.0
        });
        if visible && width >= 100.0 && height >= 100.0 {
            call!(window, c"setFrame:display:", frame, YES);
        }
    }
}

/// Gives `tree`'s splits `layout`'s ratios: they have the same shape.
fn copy_ratios(tree: &mut Node<Id>, layout: &Layout) {
    if let (Node::Split { ratio, first, second, .. }, Layout::Split { ratio: saved, first: f, second: s, .. }) = (tree, layout) {
        *ratio = *saved;
        copy_ratios(first, f);
        copy_ratios(second, s);
    }
}

/// What runs in `pane`, by name: the program of its command line.
fn program(pane: &Pane) -> String {
    let line = pane.term.lock().unwrap().running_command().unwrap_or_else(|| pane.command.clone());
    let word = line.split_whitespace().find(|w| !w.contains('=')).unwrap_or(&line);
    word.rsplit('/').next().unwrap_or(word).to_owned()
}

/// What a confirmed close sheet closes.
#[derive(Clone, Copy)]
enum Closing {
    Pane(Id),
    Window(Id),
}

thread_local! {
    /// The close waiting on a sheet's answer: global blocks capture nothing.
    static CLOSING: Cell<Option<Closing>> = const { Cell::new(None) };
}

/// Written to a new config file by Settings… (⌘,).
const CONFIG_TEMPLATE: &str = "\
# mightty settings: one `name = value` per line; `#` starts a comment.
# `mightty --help` lists every setting with its current value.
# Save, then reload with ⌘⇧, (mightty → Reload Configuration).
#
# font-family = JetBrains Mono
# font-size = 14
# theme = Dracula
";

/// Tells the user what went wrong, on a sheet over the key window if
/// there is one; also on stderr.
fn show_problems(title: &str, problems: &[String]) {
    for problem in problems {
        eprintln!("mightty: {problem}");
    }
    // SAFETY: main thread; documented signatures.
    unsafe {
        let alert: Id = msg!(new_object(c"NSAlert"), c"autorelease");
        call!(alert, c"setMessageText:", nsstring(title));
        call!(alert, c"setInformativeText:", nsstring(&problems.join("\n")));
        let key: Id = msg!(msg!(class(c"NSApplication"), c"sharedApplication"), c"keyWindow");
        let sheet: Id = if key.is_null() { NIL } else { msg!(key, c"attachedSheet") };
        if key.is_null() || !sheet.is_null() {
            let _: isize = msg!(alert, c"runModal");
        } else {
            call!(alert, c"beginSheetModalForWindow:completionHandler:", key, NIL);
        }
    }
}

/// NSAlertFirstButtonReturn: "Close".
const ALERT_FIRST_BUTTON: isize = 1000;
const ALERT_SECOND_BUTTON: isize = 1001;
/// After the user denies a program the clipboard, its pane's next requests
/// are refused without asking for this long, so it can't keep asking.
const CLIPBOARD_REFUSED_FOR: Duration = Duration::from_secs(60);

/// An alert asking `title`, explained by `detail`, with Close and Cancel.
unsafe fn close_alert(title: &str, detail: &str) -> Id {
    unsafe {
        let alert: Id = msg!(new_object(c"NSAlert"), c"autorelease");
        call!(alert, c"setMessageText:", nsstring(title));
        call!(alert, c"setInformativeText:", nsstring(detail));
        let close: Id = msg!(alert, c"addButtonWithTitle:", nsstring(if title.starts_with("Quit") { "Quit" } else { "Close" }));
        call!(close, c"setHasDestructiveAction:", YES);
        let _: Id = msg!(alert, c"addButtonWithTitle:", nsstring("Cancel"));
        alert
    }
}

/// Asks on a sheet over `window` before carrying out `closing`.
fn confirm(window: Id, closing: Closing, title: &str, detail: &str) {
    static ANSWERED: Block = Block::with_response(answered);
    unsafe extern "C" fn answered(_: *const Block, response: isize) {
        let Some(closing) = CLOSING.with(Cell::take) else { return };
        if response != ALERT_FIRST_BUTTON {
            return;
        }
        match closing {
            Closing::Pane(view) => close_surfaces(vec![view]),
            // `close` skips windowShouldClose:, which asked already.
            // SAFETY: main thread; a live NSWindow.
            Closing::Window(window) => unsafe { call!(window, c"close") },
        }
    }
    // SAFETY: main thread; documented signatures; the handler is a global block.
    unsafe {
        let sheet: Id = msg!(window, c"attachedSheet");
        if !sheet.is_null() {
            return; // already asking
        }
        CLOSING.with(|c| c.set(Some(closing)));
        let alert = close_alert(title, detail);
        call!(alert, c"beginSheetModalForWindow:completionHandler:", window, &ANSWERED);
    }
}

/// Asks on a sheet over `window` whether to carry out a program's
/// clipboard `request` for the pane `view`. One at a time: while a sheet is
/// up, other requests are refused.
fn ask_clipboard(window: Id, view: Id, request: ClipboardRequest, question: &str, detail: &str) {
    static ANSWERED: Block = Block::with_response(answered);
    unsafe extern "C" fn answered(_: *const Block, response: isize) {
        let Some((view, request)) = ASKING.with_borrow_mut(Option::take) else { return };
        with_surface(view, |s| {
            if response == ALERT_SECOND_BUTTON {
                s.do_clipboard(request);
            } else {
                s.clipboard_refused = Some(Instant::now());
            }
        });
    }
    // SAFETY: main thread; documented signatures; the handler is a global block.
    unsafe {
        let sheet: Id = msg!(window, c"attachedSheet");
        if !sheet.is_null() || ASKING.with_borrow(Option::is_some) {
            return;
        }
        ASKING.with_borrow_mut(|asking| *asking = Some((view, request)));
        let alert: Id = msg!(new_object(c"NSAlert"), c"autorelease");
        call!(alert, c"setMessageText:", nsstring(question));
        call!(alert, c"setInformativeText:", nsstring(detail));
        // Deny first, so Return (typed as the sheet appears) refuses.
        let _: Id = msg!(alert, c"addButtonWithTitle:", nsstring("Deny"));
        let _: Id = msg!(alert, c"addButtonWithTitle:", nsstring("Allow"));
        call!(alert, c"beginSheetModalForWindow:completionHandler:", window, &ANSWERED);
    }
}

/// Asks on a sheet over `window` before pasting `bytes` into the pane
/// `view`, showing `text` (see `keys::paste_is_risky`).
fn ask_paste(window: Id, view: Id, text: &str, bytes: Vec<u8>) {
    static ANSWERED: Block = Block::with_response(answered);
    unsafe extern "C" fn answered(_: *const Block, response: isize) {
        let Some((view, bytes)) = PASTING.with_borrow_mut(Option::take) else { return };
        if response == ALERT_SECOND_BUTTON {
            with_surface(view, |s| s.write(&bytes));
        }
    }
    // SAFETY: main thread; documented signatures; the handler is a global block.
    unsafe {
        let sheet: Id = msg!(window, c"attachedSheet");
        if !sheet.is_null() || PASTING.with_borrow(Option::is_some) {
            return;
        }
        PASTING.with_borrow_mut(|pasting| *pasting = Some((view, bytes)));
        let lines = text.lines().count();
        let question = if lines > 1 { format!("Paste {lines} lines?") } else { "Paste text with control characters?".to_owned() };
        let detail = format!(
            "Pasted into a shell, each line may run as a command, and control characters act as keys.\n\n{}",
            keys::visible_controls(text, 300)
        );
        let alert: Id = msg!(new_object(c"NSAlert"), c"autorelease");
        call!(alert, c"setMessageText:", nsstring(&question));
        call!(alert, c"setInformativeText:", nsstring(&detail));
        // Cancel first, so Return doesn't paste.
        let _: Id = msg!(alert, c"addButtonWithTitle:", nsstring("Cancel"));
        let _: Id = msg!(alert, c"addButtonWithTitle:", nsstring("Paste"));
        call!(alert, c"beginSheetModalForWindow:completionHandler:", window, &ANSWERED);
    }
}

/// Where a divider can be grabbed: wider than it's drawn.
fn grab_area(divider: &Divider) -> Rect {
    let r = divider.rect;
    match divider.axis {
        Axis::Row => Rect { x: r.x - GRAB, width: r.width + 2.0 * GRAB, ..r },
        Axis::Column => Rect { y: r.y - GRAB, height: r.height + 2.0 * GRAB, ..r },
    }
}

fn to_rect(r: CGRect) -> Rect {
    Rect { x: r.origin.x, y: r.origin.y, width: r.size.width, height: r.size.height }
}

fn from_rect(r: Rect) -> CGRect {
    CGRect { origin: CGPoint { x: r.x, y: r.y }, size: CGSize { width: r.width, height: r.height } }
}

/// Shows `shape` as the mouse pointer.
fn set_pointer(shape: PointerShape) {
    let name = match shape {
        PointerShape::Default => c"arrowCursor",
        PointerShape::Text => c"IBeamCursor",
        PointerShape::VerticalText => c"IBeamCursorForVerticalLayout",
        PointerShape::Pointer => c"pointingHandCursor",
        PointerShape::Crosshair => c"crosshairCursor",
        PointerShape::NotAllowed => c"operationNotAllowedCursor",
        PointerShape::Grab => c"openHandCursor",
        PointerShape::Grabbing => c"closedHandCursor",
        PointerShape::ColResize => c"resizeLeftRightCursor",
        PointerShape::RowResize => c"resizeUpDownCursor",
        PointerShape::Copy => c"dragCopyCursor",
        PointerShape::Alias => c"dragLinkCursor",
        PointerShape::ContextMenu => c"contextualMenuCursor",
    };
    // SAFETY: main thread; NSCursor class methods with no arguments.
    unsafe {
        let cursor: Id = msg!(class(c"NSCursor"), name);
        call!(cursor, c"set");
    }
}

/// A `CGColor` in `space` (autoreleased with its `NSColor`).
fn color_alpha((r, g, b): (u8, u8, u8), alpha: f64, space: Colorspace) -> Id {
    let c = |v: u8| v as f64 / 255.0;
    let make = match space {
        Colorspace::Srgb => c"colorWithSRGBRed:green:blue:alpha:",
        Colorspace::DisplayP3 => c"colorWithDisplayP3Red:green:blue:alpha:",
    };
    // SAFETY: documented signatures.
    unsafe {
        let color: Id = msg!(class(c"NSColor"), make, c(r), c(g), c(b), alpha);
        msg!(color, c"CGColor")
    }
}

/// Behind each pane's terminal: the default background, at
/// `background-opacity`. It also shows through a dimmed pane.
fn pane_background(config: &Config) -> Id {
    color_alpha(config.theme.background, config.look.background_opacity, config.colorspace)
}

/// `background-opacity` and `background-blur` for a window, whose split
/// view `root` shows through between panes as dividers.
fn style_window(window: Id, root: Id, config: &Config) {
    let translucent = config.look.background_opacity < 1.0;
    // SAFETY: main thread; documented signatures (the blur is private API).
    unsafe {
        call!(window, c"setOpaque:", if translucent { NO } else { YES });
        let background: Id = if translucent { msg!(class(c"NSColor"), c"clearColor") } else { msg!(class(c"NSColor"), c"windowBackgroundColor") };
        call!(window, c"setBackgroundColor:", background);
        // Translucent dividers would show the pane backgrounds through twice: clear instead.
        let divider = renderer::mix(config.theme.background, config.theme.foreground, 0.25);
        call!(msg!(root, c"layer"), c"setBackgroundColor:", color_alpha(divider, if translucent { 0.0 } else { 1.0 }, config.colorspace));
        let number: isize = msg!(window, c"windowNumber");
        let radius = if translucent { config.background_blur } else { 0 };
        CGSSetWindowBackgroundBlurRadius(CGSMainConnectionID(), number as i32, radius);
    }
}

impl Surface {
    fn render(&mut self) {
        // SAFETY: main thread; AppKit calls with documented signatures.
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let (frame, clipboard, pointer) = {
                let mut term = self.pane.term.lock().unwrap();
                // Animated images' next frames, or a stale progress bar to take down.
                self.animation_due = match (term.animate(Instant::now()), term.progress_expiry()) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                if let Some(due) = self.animation_due {
                    schedule_animation(due);
                }
                if term.take_bell() {
                    NSBeep();
                }
                let clipboard = term.take_clipboard_requests();
                let pointer = term.pointer_shape();
                // A program can't set the attention marker itself.
                let own = term.title().trim_start_matches(['●', ' ']);
                let mut title = if own.is_empty() { self.pane.command.clone() } else { own.to_owned() };
                if let Some(code) = self.pane.exit_status() {
                    title += &code.map_or(" — exited".into(), |c| format!(" — exited ({c})"));
                }
                // The window shows its active pane's title, marked if any of its panes needs the user.
                if self.window_attention {
                    title.insert_str(0, "● ");
                }
                if self.active && title != self.title {
                    call!(self.window, c"setTitle:", nsstring(&title));
                    self.title = title;
                }
                // Synchronized output (mode 2026): hold the frame until the app finishes.
                let frame = if term.modes().sync_output {
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
                };
                (frame, clipboard, pointer)
            };
            for request in clipboard {
                self.clipboard_request(request);
            }
            if pointer != self.pointer {
                self.pointer = pointer;
                if self.hover.is_none() && self.mouse_inside() {
                    set_pointer(pointer);
                }
            }
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
                self.renderer.blink_off = BLINK.get().0;
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
            let (cw, ch) = self.renderer.cell_size();
            self.pane.resize(cols as u16, rows as u16, (cols as u32 * cw) as u16, (rows as u32 * ch) as u16);
        }
        self.render();
    }

    /// Sends typed input, drawing its echo early (see `ECHO_WAIT`).
    fn type_bytes(&mut self, bytes: &[u8]) {
        // Typing shows a blinking cursor until it pauses.
        BLINK.set((false, Instant::now()));
        if BEHAVIOUR.get().hide_mouse {
            // SAFETY: main thread; documented class method.
            unsafe { call!(class(c"NSCursor"), c"setHiddenUntilMouseMoves:", YES) };
        }
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
        let (px, py) = self.renderer.padding_px();
        // SAFETY: main thread; documented signatures.
        unsafe {
            let bounds = send_rect(self.view, c"bounds");
            let cell = CGRect {
                origin: CGPoint {
                    x: (px + col as f64 * cw as f64) / scale,
                    y: bounds.size.height - (py + (row + 1) as f64 * ch as f64) / scale,
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

    /// `location` is in the window's coordinates.
    fn mouse(&mut self, action: MouseAction, location: CGPoint, clicks: isize, mods: Mods) {
        let (col, row) = self.cell_at(self.local(location));
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

        if action == MouseAction::Motion {
            self.drag_selection(location);
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
                self.dragging = true;
            }
            MouseAction::Motion => {}
            MouseAction::Release => {
                self.dragging = false;
                self.autoscroll = 0;
                // A drag, or a word or line picked by clicking.
                let picked = self.selection.is_some_and(|s| s.dragged || s.granularity != Granularity::Cell);
                if picked && BEHAVIOUR.get().copy_on_select {
                    self.copy();
                }
                return;
            }
        }
        self.render();
    }

    /// While selecting, moves the selection's end to the pointer
    /// (`location`, in the window's coordinates). Past the top or bottom
    /// edge, history scrolls, faster the further out the pointer is.
    fn drag_selection(&mut self, location: CGPoint) {
        if !self.dragging || self.selection.is_none() {
            self.autoscroll = 0;
            return;
        }
        let local = self.local(location);
        let (col, row) = self.cell_at(local);
        let head = {
            let term = self.pane.term.lock().unwrap();
            let (col, row) = (col.min(term.cols() - 1), row.min(term.rows() - 1));
            (term.lines_scrolled() - self.scroll_offset as u64 + row as u64, col)
        };
        if let Some(selection) = &mut self.selection {
            selection.head = head;
            selection.dragged = true;
        }

        // SAFETY: main thread; documented signature.
        let height = unsafe { send_rect(self.view, c"bounds") }.size.height;
        let line = self.renderer.cell_size().1 as f64 / self.renderer.scale();
        // Within a point of the edge counts, for a window against the screen's.
        let beyond = if local.y >= height - 1.0 {
            local.y - (height - 1.0)
        } else if local.y <= 1.0 {
            local.y - 1.0
        } else {
            0.0
        };
        self.autoscroll = if beyond == 0.0 { 0 } else { (1 + (beyond.abs() / line) as isize).min(10) * beyond.signum() as isize };
        if self.autoscroll != 0 && !AUTOSCROLLING.get() {
            AUTOSCROLLING.set(true);
            run_on_main_after(AUTOSCROLL_INTERVAL, on_autoscroll_tick);
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
            let bar = FindBar::open(self.container, self.view, self.dark());
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

    /// A program's OSC 52 request, as `clipboard-read` and
    /// `clipboard-write` allow: done, refused, or asked about on a sheet.
    fn clipboard_request(&mut self, request: ClipboardRequest) {
        let behaviour = BEHAVIOUR.get();
        let (permission, question, detail) = match &request {
            ClipboardRequest::Read { .. } => (
                behaviour.clipboard_read,
                "Let this program read the clipboard?",
                "A program in this pane is asking for what you last copied.".to_owned(),
            ),
            ClipboardRequest::Write(text) => {
                let preview = keys::visible_controls(text, 200);
                (behaviour.clipboard_write, "Let this program copy to the clipboard?", format!("It would copy:\n\n{preview}"))
            }
        };
        let refused = self.clipboard_refused.is_some_and(|at| at.elapsed() < CLIPBOARD_REFUSED_FOR);
        match permission {
            Permission::Ask if refused => {}
            Permission::Allow => self.do_clipboard(request),
            Permission::Deny => {}
            Permission::Ask => ask_clipboard(self.window, self.view, request, question, &detail),
        }
    }

    fn do_clipboard(&mut self, request: ClipboardRequest) {
        // SAFETY: main thread; documented signatures.
        unsafe {
            let pasteboard: Id = msg!(class(c"NSPasteboard"), c"generalPasteboard");
            match request {
                ClipboardRequest::Write(text) => {
                    let _: isize = msg!(pasteboard, c"clearContents");
                    let _: Bool = msg!(pasteboard, c"setString:forType:", nsstring(&text), NSPasteboardTypeString);
                }
                ClipboardRequest::Read { bell_terminated } => {
                    let text = string_from_ns(msg!(pasteboard, c"stringForType:", NSPasteboardTypeString));
                    let _ = self.pane.write(&clipboard_reply(&text, bell_terminated));
                }
            }
        }
    }

    /// ⌘K: history and the screen above the prompt.
    fn clear_screen(&mut self) {
        self.pane.term.lock().unwrap().clear_above_cursor();
        self.scroll_offset = 0;
        self.render();
    }

    /// `scroll_to_top` and the like: `to` picks the new offset from the
    /// current one, the history length and the rows on screen. Not on the
    /// alternate screen, which has no history.
    fn scroll_to(&mut self, to: impl FnOnce(usize, usize, usize) -> usize) {
        let (alt, history, rows) = {
            let term = self.pane.term.lock().unwrap();
            (term.is_alt_screen(), term.scrollback().len(), term.rows())
        };
        let offset = to(self.scroll_offset, history, rows).min(history);
        if !alt && offset != self.scroll_offset {
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
        let inside = self.mouse_inside();
        let hover = (cmd && inside)
            .then(|| {
                // SAFETY: main thread; documented signature.
                let (col, row) = self.cell_at(self.local(unsafe { msg!(self.window, c"mouseLocationOutsideOfEventStream") }));
                let term = self.pane.term.lock().unwrap();
                let line = term.lines_scrolled() - self.scroll_offset as u64 + row as u64;
                match term.link_at(line, col) {
                    Some(id) => Some((HoveredLink::Id(id), term.link(id)?.to_owned())),
                    None => term.url_at(line, col).map(|(range, url)| (HoveredLink::Range(range), url)),
                }
            })
            .flatten();
        if hover == self.hover {
            // Entering the view, AppKit puts the arrow back.
            if inside && hover.is_none() && self.pointer != PointerShape::Default {
                set_pointer(self.pointer);
            }
            return;
        }
        set_pointer(if hover.is_some() { PointerShape::Pointer } else { self.pointer });
        // SAFETY: main thread; documented signatures.
        unsafe {
            if hover.is_some() && self.hint.is_none() {
                self.hint = Some(LinkHint::new(self.container, self.dark()));
            }
        }
        if let Some(hint) = &self.hint {
            hint.show(hover.as_ref().map(|h| linkhint::describe(&h.1)).as_deref());
        }
        self.hover = hover;
        self.render();
    }

    /// Whether the mouse is over the terminal.
    fn mouse_inside(&self) -> bool {
        // SAFETY: main thread; documented signatures.
        let location = self.local(unsafe { msg!(self.window, c"mouseLocationOutsideOfEventStream") });
        let bounds = unsafe { send_rect(self.view, c"bounds") };
        (0.0..bounds.size.width).contains(&location.x) && (0.0..bounds.size.height).contains(&location.y)
    }

    fn set_focused(&mut self, focused: bool) {
        self.renderer.focused = focused;
        // Looking at a pane answers whatever it needed the user for.
        if self.pane.set_focused(focused) {
            run_on_main_with(self.pane.id as usize, on_pane_status);
        }
        if self.pane.term.lock().unwrap().modes().focus_events {
            let _ = self.pane.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
        self.render();
    }

    /// A point in the window's coordinates in the view's.
    fn local(&self, point: CGPoint) -> CGPoint {
        // SAFETY: main thread; documented signature.
        unsafe { msg!(self.view, c"convertPoint:fromView:", point, NIL) }
    }

    /// Converts a view-space point to a grid cell.
    fn cell_at(&self, point: CGPoint) -> (usize, usize) {
        // SAFETY: main thread; documented signature.
        let bounds = unsafe { send_rect(self.view, c"bounds") };
        let scale = self.renderer.scale();
        let (cw, ch) = self.renderer.cell_size();
        let (px, py) = self.renderer.padding_px();
        let x = (point.x * scale - px).max(0.0);
        let y = ((bounds.size.height - point.y) * scale - py).max(0.0);
        ((x / cw as f64) as usize, (y / ch as f64) as usize)
    }
}

// ---- Objective-C classes ----

unsafe fn register_view_class() -> Id {
    extern "C" fn yes(_: Id, _: Sel) -> Bool {
        YES
    }

    /// Key presses reach the key window's views before the menu: bound
    /// keys are handled here, for the pane keys go to.
    extern "C" fn perform_key_equivalent(view: Id, _: Sel, event: Id) -> Bool {
        const KEY_DOWN: usize = 10;
        // SAFETY: `event` is an NSEvent; documented signatures.
        unsafe {
            let kind: usize = msg!(event, c"type");
            let window: Id = msg!(view, c"window");
            let responder: Id = if window.is_null() { NIL } else { msg!(window, c"firstResponder") };
            if kind != KEY_DOWN || responder != view {
                return NO;
            }
            match binding_for(event) {
                Some(action) => {
                    perform(view, &action);
                    YES
                }
                None => NO,
            }
        }
    }

    extern "C" fn key_down(view: Id, _: Sel, event: Id) {
        // SAFETY: `event` is an NSEvent; documented signatures.
        unsafe {
            // Bound keys without Command can arrive here instead; not while composing.
            if with_surface(view, |w| w.preedit.is_empty()) == Some(true)
                && let Some(action) = binding_for(event)
            {
                return perform(view, &action);
            }
            let flags: usize = msg!(event, c"modifierFlags");
            let key_flags = with_surface(view, |w| w.pane.term.lock().unwrap().key_flags()).unwrap_or(0);
            // Command shortcuts belong to the menu; ones it hasn't go to
            // programs that can tell them apart (kitty's Super).
            if flags & CMD != 0 && key_flags == 0 {
                return;
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
            let composing = with_surface(view, |w| {
                w.ime_used_key = false;
                !w.preedit.is_empty()
            });
            let code: u16 = msg!(event, c"keyCode");
            let chars = string_from_ns(msg!(event, c"characters"));
            let shifted = string_from_ns(msg!(event, c"charactersIgnoringModifiers"));
            let unshifted = if key_flags == 0 { String::new() } else { string_from_ns(msg!(event, c"charactersByApplyingModifiers:", 0usize)) };
            let repeat: Bool = msg!(event, c"isARepeat");
            let action = if repeat != NO { KeyAction::Repeat } else { KeyAction::Press };
            let key = Key { code, text: &chars, shifted: &shifted, unshifted: &unshifted, mods, action };
            if composing == Some(true) || !(mods.ctrl || alt || mods.cmd) {
                // What the input method types for this key alone is still this key.
                with_surface(view, |w| w.capture = key_flags != 0 && w.preedit.is_empty());
                let context: Id = msg!(view, c"inputContext");
                if !context.is_null() {
                    // Not inside `with_surface`: the callbacks borrow the window.
                    let _: Bool = msg!(context, c"handleEvent:", event);
                }
                let (used, typed) = with_surface(view, |w| {
                    w.capture = false;
                    (w.ime_used_key, w.typed.take())
                })
                .unwrap_or_default();
                if let Some(text) = typed {
                    // Option typed it: it isn't Alt.
                    let mods = Mods { alt: false, ..mods };
                    send_key(view, &Key { text: &text, mods, ..key }, key_flags);
                }
                if used {
                    return;
                }
            }
            send_key(view, &key, key_flags);
        }
    }

    fn send_key(view: Id, key: &Key, key_flags: u8) {
        with_surface(view, |w| {
            let app_cursor = w.pane.term.lock().unwrap().modes().app_cursor;
            if let Some(bytes) = keys::kitty(key, key_flags, app_cursor) {
                w.type_bytes(&bytes);
                if key_flags & KEY_EVENT_TYPES != 0 && !w.keys_down.contains(&key.code) {
                    w.keys_down.push(key.code);
                }
            }
        });
    }

    /// Releases, for programs that asked for them, of keys they heard pressed.
    extern "C" fn key_up(view: Id, _: Sel, event: Id) {
        // SAFETY: `event` is an NSEvent; documented signatures.
        unsafe {
            let code: u16 = msg!(event, c"keyCode");
            if with_surface(view, |w| w.keys_down.contains(&code)) != Some(true) {
                return;
            }
            let flags: usize = msg!(event, c"modifierFlags");
            let chars = string_from_ns(msg!(event, c"characters"));
            let shifted = string_from_ns(msg!(event, c"charactersIgnoringModifiers"));
            let unshifted = string_from_ns(msg!(event, c"charactersByApplyingModifiers:", 0usize));
            let key = Key { code, text: &chars, shifted: &shifted, unshifted: &unshifted, mods: modifiers(flags), action: KeyAction::Release };
            with_surface(view, |w| {
                w.keys_down.retain(|&c| c != code);
                let (key_flags, app_cursor) = {
                    let term = w.pane.term.lock().unwrap();
                    (term.key_flags(), term.modes().app_cursor)
                };
                if let Some(bytes) = keys::kitty(&key, key_flags, app_cursor) {
                    w.write(&bytes);
                }
            });
        }
    }

    // ---- Search ----

    extern "C" fn find(view: Id, _: Sel, _sender: Id) {
        with_surface(view, Surface::open_search);
    }

    fn find_step(view: Id, step: isize) {
        with_surface(view, |w| if w.search.is_some() { w.search(Some(step)) } else { w.open_search() });
    }
    extern "C" fn find_next(view: Id, _: Sel, _sender: Id) {
        find_step(view, -1);
    }
    extern "C" fn find_previous(view: Id, _: Sel, _sender: Id) {
        find_step(view, 1);
    }
    extern "C" fn close_search(view: Id, _: Sel, _sender: Id) {
        with_surface(view, Surface::close_search);
    }

    /// The search bar's text changed (the view is its delegate).
    extern "C" fn search_changed(view: Id, _: Sel, _notification: Id) {
        with_surface(view, |w| w.search(None));
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
            with_surface(view, Surface::close_search);
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
        with_surface(view, |w| {
            w.ime_used_key = true;
            let had_preedit = !mem::take(&mut w.preedit).is_empty();
            if w.capture && !had_preedit {
                w.typed = Some(text);
                return;
            }
            w.type_bytes(text.as_bytes());
            if had_preedit {
                w.render();
            }
        });
    }

    extern "C" fn set_marked_text(view: Id, _: Sel, string: Id, _selected: NSRange, _replace: NSRange) {
        // SAFETY: AppKit passes a string object.
        let text = unsafe { text_of(string) };
        with_surface(view, |w| {
            w.ime_used_key = true;
            w.preedit = text;
            w.render();
        });
    }

    extern "C" fn unmark_text(view: Id, _: Sel) {
        with_surface(view, |w| {
            if !mem::take(&mut w.preedit).is_empty() {
                w.render();
            }
        });
    }

    extern "C" fn has_marked_text(view: Id, _: Sel) -> Bool {
        if with_surface(view, |w| !w.preedit.is_empty()) == Some(true) { YES } else { NO }
    }

    extern "C" fn marked_range(view: Id, _: Sel) -> NSRange {
        let length = with_surface(view, |w| w.preedit.encode_utf16().count()).unwrap_or(0);
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
        with_surface(view, |w| w.cursor_rect()).unwrap_or_default()
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
            with_surface(view, |w| {
                // Trackpads report points, wheels report lines.
                let points_per_line = w.renderer.cell_size().1 as f64 / w.renderer.scale();
                let lines = if precise != 0 { dy / points_per_line } else { dy } * BEHAVIOUR.get().scroll_multiplier;
                let (col, row) = w.cell_at(w.local(location));
                w.scroll(lines, col, row);
                // Scrolling while selecting moves the selection's end too.
                if w.dragging {
                    w.drag_selection(location);
                }
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
            with_surface(view, |w| {
                let bracketed = w.pane.term.lock().unwrap().modes().bracketed_paste;
                let bytes = keys::paste(&text, bracketed);
                if BEHAVIOUR.get().paste_protection && keys::paste_is_risky(&text, bracketed) {
                    ask_paste(w.window, view, &text, bytes);
                } else {
                    w.write(&bytes);
                }
            });
        }
    }

    fn mouse_event(view: Id, event: Id, action: MouseAction) {
        // SAFETY: `event` is an NSEvent; documented signatures.
        unsafe {
            let mods = modifiers(msg!(event, c"modifierFlags"));
            let location: CGPoint = msg!(event, c"locationInWindow");
            let clicks: isize = msg!(event, c"clickCount");
            with_surface(view, |w| w.mouse(action, location, clicks, mods));
        }
    }
    extern "C" fn mouse_down(view: Id, _: Sel, event: Id) {
        // SAFETY: `event` is an NSEvent.
        let flags: usize = unsafe { msg!(event, c"modifierFlags") };
        if flags & CMD != 0 {
            // Cmd-click opens the link under the pointer; it never starts a
            // selection. Only the target the hint was showing opens: if output
            // changed what's under the pointer, the hint updates instead.
            let link = with_surface(view, |w| {
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
        with_surface(view, |w| w.update_hover(flags & CMD != 0));
    }
    extern "C" fn mouse_exited(view: Id, _: Sel, _event: Id) {
        with_surface(view, |w| w.update_hover(false));
    }
    /// Cmd pressed or released without the mouse moving.
    extern "C" fn flags_changed(view: Id, _: Sel, event: Id) {
        // SAFETY: `event` is an NSEvent.
        let (flags, code): (usize, u16) = unsafe { (msg!(event, c"modifierFlags"), msg!(event, c"keyCode")) };
        with_surface(view, |w| {
            w.update_hover(flags & CMD != 0);
            // Modifier keys themselves, for programs that want every key.
            let Some(&(_, bit)) = MODIFIER_KEYS.iter().find(|m| m.0 == code) else { return };
            let (key_flags, app_cursor) = {
                let term = w.pane.term.lock().unwrap();
                (term.key_flags(), term.modes().app_cursor)
            };
            let action = if flags & bit != 0 { KeyAction::Press } else { KeyAction::Release };
            let key = Key { code, mods: modifiers(flags), action, ..Key::default() };
            if let Some(bytes) = keys::kitty(&key, key_flags, app_cursor) {
                w.write(&bytes);
            }
        });
    }
    extern "C" fn mouse_dragged(view: Id, _: Sel, event: Id) {
        mouse_event(view, event, MouseAction::Motion);
    }
    extern "C" fn mouse_up(view: Id, _: Sel, event: Id) {
        mouse_event(view, event, MouseAction::Release);
    }
    extern "C" fn copy(view: Id, _: Sel, _sender: Id) {
        with_surface(view, Surface::copy);
    }
    /// Clicked, or focused by AppKit: its pane becomes the active one. When
    /// the app is busy, the caller did that already (`App::focus`).
    extern "C" fn become_first_responder(view: Id, _: Sel) -> Bool {
        with_app(|app| app.activate(view));
        // SAFETY: `view` is a MighttyView, an NSView.
        unsafe { send_super(view, class(c"NSView"), c"becomeFirstResponder", ()) }
    }

    let action = |f: extern "C" fn(Id, Sel, Id)| f as *const c_void;
    let predicate = |f: extern "C" fn(Id, Sel) -> Bool| f as *const c_void;
    unsafe {
        register_class_with(c"MighttyView", c"NSView", &[c"NSTextInputClient"], &[
            (c"acceptsFirstResponder", predicate(yes), c"c@:"),
            (c"becomeFirstResponder", predicate(become_first_responder), c"c@:"),
            (c"isOpaque", predicate(yes), c"c@:"),
            (c"keyDown:", action(key_down), c"v@:@"),
            (c"keyUp:", action(key_up), c"v@:@"),
            (
                c"performKeyEquivalent:",
                perform_key_equivalent as extern "C" fn(Id, Sel, Id) -> Bool as *const c_void,
                c"c@:@",
            ),
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

/// A window's content view: lays out its panes, and lets their dividers be dragged.
unsafe fn register_split_class() -> Id {
    /// Near a divider, events come here rather than to the pane under them.
    extern "C" fn hit_test(view: Id, _: Sel, point: CGPoint) -> Id {
        // SAFETY: `view` is a MighttySplitView; documented signatures.
        unsafe {
            let superview: Id = msg!(view, c"superview");
            let local: CGPoint = msg!(view, c"convertPoint:fromView:", point, superview);
            if with_app(|app| app.divider_at(view, local).is_some()) == Some(true) {
                return view;
            }
            send_super(view, class(c"NSView"), c"hitTest:", (point,))
        }
    }
    fn location(view: Id, event: Id) -> CGPoint {
        // SAFETY: `event` is an NSEvent; documented signatures.
        unsafe {
            let point: CGPoint = msg!(event, c"locationInWindow");
            msg!(view, c"convertPoint:fromView:", point, NIL)
        }
    }
    extern "C" fn mouse_down(view: Id, _: Sel, event: Id) {
        let point = location(view, event);
        // SAFETY: `event` is an NSEvent.
        let clicks: isize = unsafe { msg!(event, c"clickCount") };
        with_app(|app| {
            let divider = app.divider_at(view, point);
            let w = app.windows.iter_mut().find(|w| w.root == view)?;
            // Double-clicking a divider shares the space evenly.
            if clicks == 2 && divider.is_some() {
                w.tree.equalize();
                let window = w.window;
                app.layout(window, false);
            } else {
                w.dragging = divider;
            }
            Some(())
        });
    }
    extern "C" fn mouse_dragged(view: Id, _: Sel, event: Id) {
        let point = location(view, event);
        with_app(|app| app.drag_divider(view, point));
    }
    extern "C" fn mouse_up(view: Id, _: Sel, _event: Id) {
        with_app(|app| app.windows.iter_mut().find(|w| w.root == view).map(|w| w.dragging = None));
    }
    /// Resize pointers over the dividers.
    extern "C" fn reset_cursor_rects(view: Id, _: Sel) {
        let dividers = with_app(|app| {
            let w = app.windows.iter().find(|w| w.root == view).filter(|w| !w.zoomed)?;
            // SAFETY: main thread; documented signature.
            let bounds = to_rect(unsafe { send_rect(view, c"bounds") });
            Some(w.tree.dividers(bounds, GAP))
        });
        for divider in dividers.flatten().unwrap_or_default() {
            let cursor = if divider.axis == Axis::Row { c"resizeLeftRightCursor" } else { c"resizeUpDownCursor" };
            // SAFETY: main thread; documented signatures.
            unsafe {
                let cursor: Id = msg!(class(c"NSCursor"), cursor);
                call!(view, c"addCursorRect:cursor:", from_rect(grab_area(&divider)), cursor);
            }
        }
    }

    let action = |f: extern "C" fn(Id, Sel, Id)| f as *const c_void;
    unsafe {
        register_class(c"MighttySplitView", c"NSView", &[
            (c"hitTest:", hit_test as *const c_void, c"@@:{CGPoint=dd}"),
            (c"mouseDown:", action(mouse_down), c"v@:@"),
            (c"mouseDragged:", action(mouse_dragged), c"v@:@"),
            (c"mouseUp:", action(mouse_up), c"v@:@"),
            (c"resetCursorRects", reset_cursor_rects as *const c_void, c"v@:"),
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
            app.save_session();
            if let Some(socket) = app.mux.socket_path() {
                let _ = fs::remove_file(socket);
            }
        });
    }
    extern "C" fn did_resize(_: Id, _: Sel, notification: Id) {
        with_app(|app| app.layout(window_of(notification), true));
    }
    fn set_key(notification: Id, key: bool) {
        let window = window_of(notification);
        with_app(|app| {
            let active = app.windows.iter().find(|w| w.window == window)?.active;
            app.surfaces.iter_mut().find(|s| s.view == active).map(|s| s.set_focused(key))
        });
    }
    /// A tab coming to the front, a window uncovered: what it last showed may
    /// be gone (hidden tabs drop it), and it may have been resized meanwhile.
    extern "C" fn did_change_occlusion(_: Id, _: Sel, notification: Id) {
        let window = window_of(notification);
        // SAFETY: an NSWindow; documented signature.
        let state: usize = unsafe { msg!(window, c"occlusionState") };
        if state & NS_WINDOW_OCCLUSION_STATE_VISIBLE != 0 {
            with_app(|app| app.redraw_window(window));
        }
    }
    /// A banner was clicked: show its pane.
    extern "C" fn notification_clicked(_: Id, _: Sel, _center: Id, response: Id, done: Id) {
        if let Some(pane) = notify::pane_of(response) {
            with_app(|app| app.focus_pane(pane));
            // SAFETY: main thread; documented signatures.
            unsafe {
                let ns_app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
                call!(ns_app, c"activateIgnoringOtherApps:", YES);
            }
        }
        // SAFETY: the completion handler AppKit passed, taking no arguments.
        unsafe { call_block(done) };
    }
    extern "C" fn did_become_key(_: Id, _: Sel, notification: Id) {
        set_key(notification, true);
    }
    extern "C" fn did_resign_key(_: Id, _: Sel, notification: Id) {
        set_key(notification, false);
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
    extern "C" fn split_right(_: Id, _: Sel, _sender: Id) {
        with_app(|app| app.split(Axis::Row));
    }
    extern "C" fn split_down(_: Id, _: Sel, _sender: Id) {
        with_app(|app| app.split(Axis::Column));
    }
    /// ⌘W: the active pane, and the window with its last; asking first if
    /// something runs in it (`confirm-close-surface`).
    extern "C" fn close_split(_: Id, _: Sel, _sender: Id) {
        let Some((window, view)) = with_app(|app| app.key_window()).flatten() else { return };
        match with_app(|app| app.close_question(&[view])).flatten() {
            Some(running) => confirm(window, Closing::Pane(view), "Close this pane?", &running),
            None => close_surfaces(vec![view]),
        }
    }
    /// The close button, ⌘⇧W and a tab's ×.
    extern "C" fn should_close(_: Id, _: Sel, window: Id) -> Bool {
        let question = with_app(|app| {
            let views: Vec<Id> = app.surfaces.iter().filter(|s| s.window == window).map(|s| s.view).collect();
            app.close_question(&views)
        });
        match question.flatten() {
            Some(running) => {
                confirm(window, Closing::Window(window), "Close this window?", &running);
                NO
            }
            None => YES,
        }
    }
    /// ⌘Q.
    extern "C" fn should_terminate(_: Id, _: Sel, _: Id) -> usize {
        const TERMINATE_CANCEL: usize = 0;
        const TERMINATE_NOW: usize = 1;
        let question = with_app(|app| {
            let views: Vec<Id> = app.surfaces.iter().map(|s| s.view).collect();
            app.close_question(&views)
        });
        let Some(running) = question.flatten() else { return TERMINATE_NOW };
        // SAFETY: main thread; documented signatures.
        unsafe {
            let alert = close_alert("Quit mightty?", &running.replace("Closing", "Quitting"));
            let response: isize = msg!(alert, c"runModal");
            if response == ALERT_FIRST_BUTTON { TERMINATE_NOW } else { TERMINATE_CANCEL }
        }
    }
    /// ⌘[ / ⌘]: the menu item's tag is the step.
    extern "C" fn select_split(_: Id, _: Sel, sender: Id) {
        // SAFETY: `sender` is the menu item.
        let step: isize = unsafe { msg!(sender, c"tag") };
        with_app(|app| app.select_split(step));
    }
    /// ⌘⌥ and an arrow: the tag is the `Direction`.
    extern "C" fn select_split_toward(_: Id, _: Sel, sender: Id) {
        // SAFETY: `sender` is the menu item.
        let tag: isize = unsafe { msg!(sender, c"tag") };
        let direction = [Direction::Left, Direction::Right, Direction::Up, Direction::Down][tag as usize % 4];
        with_app(|app| app.select_split_toward(direction));
    }
    extern "C" fn jump_to_attention(_: Id, _: Sel, _sender: Id) {
        with_app(App::jump_to_attention);
    }
    extern "C" fn appearance_changed(_: Id, _: Sel, _notification: Id) {
        run_on_main_after(Duration::from_millis(100), on_appearance_changed);
    }
    extern "C" fn open_config(_: Id, _: Sel, _sender: Id) {
        with_app(|app| app.open_config());
    }
    extern "C" fn reload_config(_: Id, _: Sel, _sender: Id) {
        with_app(App::reload_config);
    }
    /// ⌘⌃ and an arrow: the tag is the `Direction`.
    extern "C" fn move_divider(_: Id, _: Sel, sender: Id) {
        // SAFETY: `sender` is the menu item.
        let tag: isize = unsafe { msg!(sender, c"tag") };
        let direction = [Direction::Left, Direction::Right, Direction::Up, Direction::Down][tag as usize % 4];
        with_app(|app| app.move_divider(direction, RESIZE_STEP));
    }
    extern "C" fn toggle_zoom(_: Id, _: Sel, _sender: Id) {
        with_app(App::toggle_zoom);
    }
    extern "C" fn equalize(_: Id, _: Sel, _sender: Id) {
        with_app(App::equalize);
    }
    /// ⌘1…⌘8 pick that tab, ⌘9 the last (the menu item's tag is the digit).
    extern "C" fn select_tab_by_digit(_: Id, _: Sel, sender: Id) {
        // SAFETY: `sender` is the menu item.
        let digit: isize = unsafe { msg!(sender, c"tag") };
        select_tab(if digit == 9 { None } else { Some(digit as usize - 1) });
    }
    /// Menu items for actions without a selector: the tag says which.
    extern "C" fn menu_action(_: Id, _: Sel, sender: Id) {
        // SAFETY: `sender` is the menu item.
        let index: isize = unsafe { msg!(sender, c"tag") };
        let Some(action) = MENU.with_borrow(|items| items.get(index as usize).map(|i| i.1.clone())) else { return };
        let view = with_app(|app| app.key_window().map(|k| k.1)).flatten().unwrap_or(NIL);
        perform(view, &action);
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
            (c"windowDidChangeOcclusionState:", action(did_change_occlusion), c"v@:@"),
            (
                c"userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:",
                notification_clicked as extern "C" fn(Id, Sel, Id, Id, Id) as *const c_void,
                c"v@:@@@?",
            ),
            (c"windowDidResignKey:", action(did_resign_key), c"v@:@"),
            (c"windowWillClose:", action(will_close), c"v@:@"),
            (c"windowShouldClose:", should_close as extern "C" fn(Id, Sel, Id) -> Bool as *const c_void, c"c@:@"),
            (
                c"applicationShouldTerminate:",
                should_terminate as extern "C" fn(Id, Sel, Id) -> usize as *const c_void,
                c"Q@:@",
            ),
            (c"newWindow:", action(new_window), c"v@:@"),
            (c"newTab:", action(new_tab), c"v@:@"),
            (c"newWindowForTab:", action(new_tab), c"v@:@"),
            (c"selectTabByDigit:", action(select_tab_by_digit), c"v@:@"),
            (c"menuAction:", action(menu_action), c"v@:@"),
            (c"splitRight:", action(split_right), c"v@:@"),
            (c"splitDown:", action(split_down), c"v@:@"),
            (c"closeSplit:", action(close_split), c"v@:@"),
            (c"selectSplit:", action(select_split), c"v@:@"),
            (c"selectSplitToward:", action(select_split_toward), c"v@:@"),
            (c"toggleSplitZoom:", action(toggle_zoom), c"v@:@"),
            (c"moveDivider:", action(move_divider), c"v@:@"),
            (c"equalizeSplits:", action(equalize), c"v@:@"),
            (c"jumpToAttention:", action(jump_to_attention), c"v@:@"),
            (c"openConfig:", action(open_config), c"v@:@"),
            (c"appearanceChanged:", action(appearance_changed), c"v@:@"),
            (c"reloadConfig:", action(reload_config), c"v@:@"),
        ])
    }
}

thread_local! {
    /// Menu items and their actions, to show the keys bound to them.
    static MENU: RefCell<Vec<(Id, Action)>> = const { RefCell::new(Vec::new()) };
}

/// Shows on each menu item the first key bound to its action, if any.
fn set_menu_keys(bindings: &Bindings) {
    MENU.with_borrow(|items| {
        for (item, action) in items {
            let (key, mask) = match bindings.trigger_for(action) {
                Some(trigger) => (trigger.menu_key(), ns_modifiers(trigger.mods)),
                None => (String::new(), 0),
            };
            // SAFETY: main thread; a live NSMenuItem; documented signatures.
            unsafe {
                call!(*item, c"setKeyEquivalent:", nsstring(&key));
                call!(*item, c"setKeyEquivalentModifierMask:", mask);
            }
        }
    });
}

/// NSEvent modifier flags for `bindings` modifier bits, and back.
fn ns_modifiers(mods: u8) -> usize {
    [(bindings::SHIFT, SHIFT), (bindings::CTRL, CTRL), (bindings::ALT, ALT), (bindings::CMD, CMD)]
        .iter()
        .filter(|(bit, _)| mods & bit != 0)
        .fold(0, |flags, (_, flag)| flags | flag)
}

fn binding_modifiers(flags: usize) -> u8 {
    [(bindings::SHIFT, SHIFT), (bindings::CTRL, CTRL), (bindings::ALT, ALT), (bindings::CMD, CMD)]
        .iter()
        .filter(|(_, flag)| flags & flag != 0)
        .fold(0, |mods, (bit, _)| mods | bit)
}

/// The action bound to a key press, if any.
unsafe fn binding_for(event: Id) -> Option<Action> {
    unsafe {
        let flags: usize = msg!(event, c"modifierFlags");
        let code: u16 = msg!(event, c"keyCode");
        // What the key types on the current layout with no modifiers held.
        let unmodified = string_from_ns(msg!(event, c"charactersByApplyingModifiers:", 0usize));
        let trigger = Trigger::from_event(code, &unmodified, binding_modifiers(flags))?;
        with_app(|app| app.config.keybinds.find(trigger).cloned()).flatten()
    }
}

/// Carries out a bound action for the pane `view` (nil when there's none):
/// menu commands go through the responder chain as if chosen, the rest are
/// done here.
fn perform(view: Id, action: &Action) {
    let send = |selector: &CStr, target: Id| {
        // SAFETY: main thread; documented signature.
        let _: Bool = unsafe { msg!(msg!(class(c"NSApplication"), c"sharedApplication"), c"sendAction:to:from:", sel(selector), target, NIL) };
    };
    match *action {
        Action::NewWindow => send(c"newWindow:", NIL),
        Action::NewTab => send(c"newTab:", NIL),
        Action::NewSplit(Direction::Down) => send(c"splitDown:", NIL),
        Action::NewSplit(_) => send(c"splitRight:", NIL),
        Action::CloseSurface => send(c"closeSplit:", NIL),
        Action::CloseWindow => send(c"performClose:", NIL),
        Action::MinimizeWindow => send(c"performMiniaturize:", NIL),
        Action::Quit => send(c"terminate:", NIL),
        Action::Copy => send(c"copy:", view),
        Action::Paste => send(c"paste:", view),
        Action::StartSearch => send(c"performFind:", view),
        Action::NavigateSearch(step) => send(if step > 0 { c"findNext:" } else { c"findPrevious:" }, view),
        Action::GotoSplitStep(step) => drop(with_app(|app| app.select_split(step))),
        Action::GotoSplit(direction) => drop(with_app(|app| app.select_split_toward(direction))),
        Action::ToggleSplitZoom => drop(with_app(App::toggle_zoom)),
        Action::EqualizeSplits => drop(with_app(App::equalize)),
        Action::ResizeSplit(direction, points) => drop(with_app(|app| app.move_divider(direction, points))),
        Action::GotoTab(n) => select_tab(Some(n - 1)),
        Action::LastTab => select_tab(None),
        Action::PreviousTab => send(c"selectPreviousTab:", NIL),
        Action::NextTab => send(c"selectNextTab:", NIL),
        Action::JumpToAttention => drop(with_app(App::jump_to_attention)),
        Action::OpenConfig => drop(with_app(|app| app.open_config())),
        Action::ReloadConfig => drop(with_app(App::reload_config)),
        Action::ChangeFontSize(points) => drop(with_app(|app| app.change_font_size(view, Some(points)))),
        Action::ResetFontSize => drop(with_app(|app| app.change_font_size(view, None))),
        Action::ClearScreen => drop(with_surface(view, Surface::clear_screen)),
        Action::ScrollToTop => drop(with_surface(view, |s| s.scroll_to(|_, history, _| history))),
        Action::ScrollToBottom => drop(with_surface(view, |s| s.scroll_to(|_, _, _| 0))),
        Action::ScrollPageUp => drop(with_surface(view, |s| s.scroll_to(|at, _, rows| at + rows))),
        Action::ScrollPageDown => drop(with_surface(view, |s| s.scroll_to(|at, _, rows| at.saturating_sub(rows)))),
        Action::Text(ref bytes) => drop(with_surface(view, |s| s.type_bytes(bytes))),
        Action::Ignore => {}
    }
}

/// Shows the key window's tab at `index`, or its last tab.
fn select_tab(index: Option<usize>) {
    // SAFETY: main thread; documented signatures.
    unsafe {
        let key: Id = msg!(msg!(class(c"NSApplication"), c"sharedApplication"), c"keyWindow");
        if key.is_null() {
            return;
        }
        let tabs: Id = msg!(key, c"tabbedWindows");
        let count: usize = if tabs.is_null() { 1 } else { msg!(tabs, c"count") };
        let index = index.unwrap_or(count - 1);
        if !tabs.is_null() && index < count {
            let window: Id = msg!(tabs, c"objectAtIndex:", index);
            call!(window, c"makeKeyAndOrderFront:", NIL);
        }
    }
}

/// The main menu. Items show the keys bound to their actions (see
/// `set_menu_keys`); key presses reach the bindings first, through the
/// terminal view, and the menu when a field such as the search bar has focus.
unsafe fn install_menu(ns_app: Id, bindings: &Bindings) {
    /// An item doing `action` by sending `selector`.
    unsafe fn add_item(menu: Id, title: &str, selector: &CStr, action: Action) -> Id {
        unsafe {
            let item: Id = msg!(
                msg!(class(c"NSMenuItem"), c"alloc"),
                c"initWithTitle:action:keyEquivalent:",
                nsstring(title),
                sel(selector),
                nsstring("")
            );
            call!(menu, c"addItem:", item);
            MENU.with_borrow_mut(|items| items.push((item, action)));
            item
        }
    }

    /// An item for an action without a selector of its own: `menuAction:`
    /// finds the action by the item's tag.
    unsafe fn add_action(menu: Id, title: &str, action: Action) -> Id {
        unsafe {
            let index = MENU.with_borrow(Vec::len);
            let item = add_item(menu, title, c"menuAction:", action);
            call!(item, c"setTag:", index as isize);
            item
        }
    }

    unsafe fn separator(menu: Id) {
        unsafe {
            let item: Id = msg!(class(c"NSMenuItem"), c"separatorItem");
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
        add_item(app_menu, "Settings…", c"openConfig:", Action::OpenConfig);
        add_item(app_menu, "Reload Configuration", c"reloadConfig:", Action::ReloadConfig);
        separator(app_menu);
        add_item(app_menu, "Quit mightty", c"terminate:", Action::Quit);
        let shell_menu = submenu(bar, "Shell");
        add_item(shell_menu, "New Window", c"newWindow:", Action::NewWindow);
        add_item(shell_menu, "New Tab", c"newTab:", Action::NewTab);
        add_item(shell_menu, "Split Right", c"splitRight:", Action::NewSplit(Direction::Right));
        add_item(shell_menu, "Split Down", c"splitDown:", Action::NewSplit(Direction::Down));
        separator(shell_menu);
        add_item(shell_menu, "Close", c"closeSplit:", Action::CloseSurface);
        add_item(shell_menu, "Close Window", c"performClose:", Action::CloseWindow);
        let edit_menu = submenu(bar, "Edit");
        add_item(edit_menu, "Copy", c"copy:", Action::Copy);
        add_item(edit_menu, "Paste", c"paste:", Action::Paste);
        add_action(edit_menu, "Clear Screen", Action::ClearScreen);
        separator(edit_menu);
        add_item(edit_menu, "Find…", c"performFind:", Action::StartSearch);
        add_item(edit_menu, "Find Next", c"findNext:", Action::NavigateSearch(1));
        add_item(edit_menu, "Find Previous", c"findPrevious:", Action::NavigateSearch(-1));
        let view_menu = submenu(bar, "View");
        add_action(view_menu, "Increase Font Size", Action::ChangeFontSize(1.0));
        add_action(view_menu, "Decrease Font Size", Action::ChangeFontSize(-1.0));
        add_action(view_menu, "Reset Font Size", Action::ResetFontSize);
        // AppKit adds the tab items (Show Tab Bar, Move Tab to New Window, …) here.
        let window_menu = submenu(bar, "Window");
        add_item(window_menu, "Minimize", c"performMiniaturize:", Action::MinimizeWindow);
        separator(window_menu);
        add_item(window_menu, "Jump to Pane Needing You", c"jumpToAttention:", Action::JumpToAttention);
        separator(window_menu);
        add_item(window_menu, "Zoom Split", c"toggleSplitZoom:", Action::ToggleSplitZoom);
        add_item(window_menu, "Equalize Splits", c"equalizeSplits:", Action::EqualizeSplits);
        for (title, step) in [("Select Previous Split", -1isize), ("Select Next Split", 1)] {
            let item = add_item(window_menu, title, c"selectSplit:", Action::GotoSplitStep(step));
            call!(item, c"setTag:", step);
        }
        // In `Direction` order, which the tags are.
        let directions = [Direction::Left, Direction::Right, Direction::Up, Direction::Down];
        for (tag, title) in ["Left", "Right", "Above", "Below"].into_iter().enumerate() {
            let item = add_item(window_menu, &format!("Select Split {title}"), c"selectSplitToward:", Action::GotoSplit(directions[tag]));
            call!(item, c"setTag:", tag as isize);
        }
        for (tag, title) in ["Left", "Right", "Up", "Down"].into_iter().enumerate() {
            let action = Action::ResizeSplit(directions[tag], RESIZE_STEP);
            let item = add_item(window_menu, &format!("Move Divider {title}"), c"moveDivider:", action);
            call!(item, c"setTag:", tag as isize);
        }
        separator(window_menu);
        add_item(window_menu, "Show Previous Tab", c"selectPreviousTab:", Action::PreviousTab);
        add_item(window_menu, "Show Next Tab", c"selectNextTab:", Action::NextTab);
        for digit in 1..=9isize {
            let (title, action) =
                if digit == 9 { ("Select Last Tab".to_owned(), Action::LastTab) } else { (format!("Select Tab {digit}"), Action::GotoTab(digit as usize)) };
            let item = add_item(window_menu, &title, c"selectTabByDigit:", action);
            call!(item, c"setTag:", digit);
        }
        set_menu_keys(bindings);
        call!(ns_app, c"setWindowsMenu:", window_menu);
        call!(ns_app, c"setMainMenu:", bar);
    }
}
