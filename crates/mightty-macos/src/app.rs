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
use std::path::{Path, PathBuf};
use std::env;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mightty_mux::{Host, Location, Mux, Pane, PaneId, Placement, SpawnOptions, Status};
use mightty_term::{ClipboardRequest, KEY_EVENT_TYPES, MouseMode, PointerShape, Rgb, Term, clipboard_reply};

use crate::bindings::{self, Action, Bindings, Trigger};
use crate::config::{self, Behaviour, Colorspace, Config, ConfirmClose, Extent, Titlebar, MAX_FONT_SIZE, MIN_FONT_SIZE, OptionAsAlt, Permission, QuickPosition, SaveState};
use crate::ffi::*;
use crate::findbar::FindBar;
use crate::hotkey;
use crate::keys::{self, Key, KeyAction, Mods, MouseAction};
use crate::latency;
use crate::open;
use crate::palette::{self, Palette};
use crate::scrollbar::{self, Scrollbar};
use crate::linkhint::{self, LinkHint};
use crate::notify;
use crate::session::{self, Layout, Session};
use crate::renderer::{self, Highlights, HoveredLink, Renderer, Snapshot};
use crate::split::{self, Axis, Direction, Divider, Node, Place, Rect};
use crate::tabbar::{self, TabBar};

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
    /// Secure Keyboard Entry: asked for from the menu, and whether it's on.
    secure_input: bool,
    secure_input_on: bool,
    /// Tabs, panes and windows closed, oldest first, to undo or reopen.
    closed: Vec<Closed>,
    /// The tab bars of windows with mightty's own tabs, with what they last showed.
    bars: Vec<(Id, TabBar, Option<BarShown>)>,
    /// The quick terminal's window, once opened; and the app (retained)
    /// to give the keys back to when it hides.
    quick: Option<Id>,
    quick_return: Id,
}

/// A tab, pane or window closed, to bring back.
struct Closed {
    /// Its panes still running, hidden, until `until` (see `undo-timeout`),
    /// in the order their layout lists them; none after.
    live: Vec<Arc<Pane>>,
    until: Instant,
    what: Reopen,
}

enum Reopen {
    /// A pane of `window`, at `place` in its splits.
    Pane { window: Id, place: Place<Id>, cwd: Option<String>, title: Option<String> },
    /// A tab or window laid out as `tab`: a tab of `tab_of` if that's still
    /// open, else a window at `frame`.
    Tab { tab: session::Tab, tab_of: Id, frame: CGRect },
}

/// What a tab bar shows: titles, the selected tab, and whether it's shown at all.
type BarShown = (Vec<String>, usize, bool);

/// Closes remembered for reopening at most.
const CLOSED_KEPT: usize = 20;

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
/// A tab: its window and its panes. A window has one, with AppKit's tabs
/// (each tab its own window); with mightty's own (`macos-titlebar-style`
/// tabs or hidden) a window has one per tab, `shown` the one on screen.
struct Window {
    window: Id,
    /// The content view, a `MighttySplitView`: holds each pane's container,
    /// showing through between them as dividers. The window's tabs share it.
    root: Id,
    shown: bool,
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
    /// Characters typed at the password prompt the program shows, drawn
    /// as `*` (see `password-mask`).
    password: usize,
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
    /// The command palette, while open.
    palette: Option<Palette>,
    scrollbar: Scrollbar,
    /// A title the user gave the pane (Change Title…), shown instead of its program's.
    title_override: Option<String>,
    /// What the container's background was last set to (see `Renderer::backing`).
    backing: Option<(Rgb, f64, Colorspace)>,
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
    /// A bell rang here, not yet seen: 🔔 on its title (see `bell-features`).
    bell: bool,
    /// Flashed round the pane's edge by a bell, made on the first.
    flash: Id,
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
    /// Columns between the ends, on every line between (⌥-drag), as in Ghostty.
    block: bool,
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
        // Output may be a password prompt, or a new title for a tab.
        app.update_secure_input();
        let windows: Vec<Id> = app.bars.iter().map(|b| b.0).collect();
        for window in windows {
            app.update_tab_bar(window);
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
        register_global_keys(&config.keybinds);
        scrollbar::set_mouse_handler(on_scrollbar_mouse);
        tabbar::set_click_handler(on_tab_click);
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
        if let Some(problem) = load_bell_sound(config.bell_audio.as_deref()) {
            eprintln!("mightty: {problem}");
        }
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
            secure_input: false,
            secure_input_on: false,
            closed: Vec::new(),
            bars: Vec::new(),
            quick: None,
            quick_return: NIL,
            banners: Vec::new(),
            saved: String::new(),
        };
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        if !(restore && with_app(App::restore) == Some(true)) {
            let pane = with_app(|app| app.mux.spawn(first)).expect("the app was just set")?;
            with_app(|app| app.open_window(pane, false));
        }
        run_on_main_after(SAVE_INTERVAL, on_save_tick);
        // What the config file is now; changes to it are applied as they're saved.
        let path = with_app(|app| app.config.file().map(Path::to_path_buf).or_else(|| Some(config::config_dir()?.join("config")))).flatten();
        CONFIG_SEEN.set(config_stamp(path.as_deref()));
        run_on_main_after(CONFIG_TICK, on_config_tick);
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
        self.open_window_with(pane, hold, tab_of, focus, false);
    }

    /// `open_window_in`, or with `quick` the quick terminal's window:
    /// borderless, never a tab, left hidden for `show_quick` to place.
    fn open_window_with(&mut self, pane: Arc<Pane>, hold: bool, tab_of: Id, focus: bool, quick: bool) -> Id {
        if !quick && !tab_of.is_null() && self.own_tabs() && self.shown(tab_of).is_some() {
            return self.add_tab(tab_of, pane, hold, focus);
        }
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
            // titled | closable | miniaturizable | resizable, or borderless; buffered backing.
            let window: Id = msg!(
                msg!(if quick { quick_window_class() } else { class(c"NSWindow") }, c"alloc"),
                c"initWithContentRect:styleMask:backing:defer:",
                content,
                if quick { 0usize } else { 15usize },
                2usize,
                NO
            );
            call!(window, c"setTitle:", nsstring("Mightty"));
            call!(window, c"setDelegate:", self.delegate);
            call!(window, c"setReleasedWhenClosed:", NO);
            if quick {
                call!(window, c"setTabbingMode:", 2isize); // disallowed
                // Over other windows, on every Space, and over full-screen apps.
                call!(window, c"setLevel:", 3isize); // floating
                call!(window, c"setCollectionBehavior:", 1usize | 256); // canJoinAllSpaces | fullScreenAuxiliary
            } else {
                // Tabs are mightty's own, in its tab bar: none of AppKit's.
                call!(window, c"setTabbingMode:", 2isize); // disallowed
            }

            let root: Id = msg!(msg!(self.split_class, c"alloc"), c"initWithFrame:", content);
            call!(root, c"setWantsLayer:", YES);
            call!(window, c"setContentView:", root);
            call!(root, c"release");
            let view = self.add_surface(window, content, renderer, pane, hold, true);
            self.windows.push(Window { window, root, shown: true, tree: Node::Leaf(view), active: view, zoomed: false, dragging: None });
            call!(window, c"makeFirstResponder:", view);
            if quick {
                style_window(window, root, &self.config);
                return window;
            }
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
            self.update_tab_bar(window);
            self.layout(window, true);
            self.sync_key_focus();
            window
        }
    }

    /// Shows the quick terminal, starting a shell for it the first time,
    /// or hides it when it has the keys.
    fn toggle_quick_terminal(&mut self) {
        // SAFETY: main thread; documented signatures.
        unsafe {
            if let Some(window) = self.quick {
                let key: Bool = msg!(window, c"isKeyWindow");
                if key != NO {
                    return self.hide_quick(true);
                }
            }
            let ns_app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
            let front: Id = msg!(msg!(class(c"NSWorkspace"), c"sharedWorkspace"), c"frontmostApplication");
            let us: Id = msg!(class(c"NSRunningApplication"), c"currentApplication");
            // Where the keys go back to when it hides.
            let pid = |app: Id| -> i32 { if app.is_null() { 0 } else { msg!(app, c"processIdentifier") } };
            if pid(front) != pid(us) {
                if !self.quick_return.is_null() {
                    call!(self.quick_return, c"release");
                }
                self.quick_return = if front.is_null() { NIL } else { msg!(front, c"retain") };
            }
            let window = match self.quick {
                Some(window) => window,
                None => {
                    let opts = SpawnOptions { cols: self.config.cols, rows: self.config.rows, ..Default::default() };
                    let pane = match self.mux.spawn(opts) {
                        Ok(pane) => pane,
                        Err(e) => return eprintln!("mightty: could not start a shell: {e}"),
                    };
                    let window = self.open_window_with(pane, false, NIL, true, true);
                    self.quick = Some(window);
                    window
                }
            };
            // On the screen with the mouse, sliding in from its edge.
            let (frame, start) = quick_frames(self.config.quick_position, self.config.quick_size);
            call!(window, c"setFrame:display:", start, NO);
            call!(window, c"makeKeyAndOrderFront:", NIL);
            call!(ns_app, c"activateIgnoringOtherApps:", YES);
            // AppKit's own slide, over `animationResizeTime:`.
            call!(window, c"setFrame:display:animate:", frame, YES, YES);
            self.layout(window, true);
        }
    }

    /// Hides the quick terminal, sliding it out to its edge; with
    /// `give_back`, the keys go back to the app that had them.
    fn hide_quick(&mut self, give_back: bool) {
        let Some(window) = self.quick else { return };
        // SAFETY: main thread; documented signatures; `quick_return` was retained.
        unsafe {
            // Already gone: giving the keys back makes it resign them, which hides it too.
            let visible: Bool = msg!(window, c"isVisible");
            if visible == NO {
                return;
            }
            // The keys go back first: hidden while mightty is still active,
            // AppKit would bring its next window forward for a moment.
            let back = mem::replace(&mut self.quick_return, NIL);
            if !back.is_null() {
                if give_back {
                    let _: Bool = msg!(back, c"activateWithOptions:", 0usize);
                }
                call!(back, c"release");
            }
            if self.config.quick_position != QuickPosition::Center {
                let (_, out) = quick_frames(self.config.quick_position, self.config.quick_size);
                call!(window, c"setFrame:display:animate:", out, YES, YES);
            }
            call!(window, c"orderOut:", NIL);
        }
    }

    /// Where `window`'s panes go in its content view `root`: all of it, but
    /// for a tab bar along the top.
    fn pane_bounds(&self, window: Id, root: Id) -> Rect {
        // SAFETY: main thread; documented signature.
        let mut bounds = to_rect(unsafe { send_rect(root, c"bounds") });
        if self.bars.iter().any(|b| b.0 == window && b.2.as_ref().is_some_and(|s| s.2)) {
            bounds.height = (bounds.height - tabbar::HEIGHT).max(0.0);
        }
        bounds
    }

    /// The title of the tab at `index` of `windows`: its active pane's.
    fn tab_title(&self, index: usize) -> String {
        let active = self.windows[index].active;
        let Some(s) = self.surfaces.iter().find(|s| s.view == active) else { return String::new() };
        let mark = if s.bell { "🔔 " } else { "" };
        if let Some(title) = &s.title_override {
            return format!("{mark}{title}");
        }
        let title = s.pane.term.lock().unwrap().title().trim_start_matches(['●', ' ']).to_owned();
        format!("{mark}{}", if title.is_empty() { &s.pane.command } else { &title })
    }

    /// Brings `window`'s tab bar up to date: there with mightty's own tabs
    /// (with hidden, only for several), showing their titles.
    fn update_tab_bar(&mut self, window: Id) {
        let tabs = self.tabs_of(window);
        let style = self.config.behaviour.titlebar;
        let wanted = self.own_tabs() && self.quick != Some(window) && !tabs.is_empty();
        if !wanted {
            let had = self.bars.iter().any(|b| b.0 == window);
            self.bars.retain(|b| b.0 != window);
            if had {
                self.layout(window, false);
            }
            return;
        }
        // In the title bar's place it's always there; else, as AppKit's, from two tabs.
        let show = style == Titlebar::Tabs || tabs.len() > 1;
        let titles: Vec<String> = tabs.iter().map(|&i| self.tab_title(i)).collect();
        let selected = tabs.iter().position(|&i| self.windows[i].shown).unwrap_or(0);
        let now = Some((titles.clone(), selected, show));
        if !self.bars.iter().any(|b| b.0 == window) {
            let root = self.windows[tabs[0]].root;
            // SAFETY: main thread; `root` is the window's content view.
            self.bars.push((window, unsafe { TabBar::new(root) }, None));
        }
        let Some(entry) = self.bars.iter_mut().find(|b| b.0 == window) else { return };
        if entry.2 == now {
            return;
        }
        let was_shown = entry.2.as_ref().is_some_and(|s| s.2);
        // Past the window's buttons, when they're over it.
        let left = if style == Titlebar::Tabs { 78.0 } else { 0.0 };
        // A tab came or went: the others slide to their places.
        let animate = entry.2.as_ref().is_some_and(|s| s.0.len() != titles.len() && s.2 && show);
        entry.1.show(&titles, selected, left, self.config.theme.background, self.config.theme.foreground, animate);
        entry.1.set_hidden(!show);
        entry.2 = now;
        if was_shown != show {
            self.layout(window, false);
        }
    }

    /// Moves `window`'s tab `from` to `to`, the others making way.
    fn move_tab(&mut self, window: Id, from: usize, to: usize) {
        let places = self.tabs_of(window);
        if from != to && from < places.len() && to < places.len() {
            // Taken out in order, put back in the same places in the new order.
            let mut tabs: Vec<Window> = places.iter().rev().map(|&i| self.windows.remove(i)).collect();
            tabs.reverse();
            let moved = tabs.remove(from);
            tabs.insert(to, moved);
            for (&i, tab) in places.iter().zip(tabs) {
                self.windows.insert(i, tab);
            }
        }
        // Drawn in place again, also when it went back where it was.
        for bar in self.bars.iter_mut().filter(|b| b.0 == window) {
            bar.2 = None;
        }
        self.update_tab_bar(window);
    }

    /// Takes `window`'s tab `index` out of it, its panes going on running:
    /// its layout and panes, to be put in another window.
    fn take_tab(&mut self, window: Id, index: usize) -> Option<(session::Tab, Vec<Arc<Pane>>)> {
        let places = self.tabs_of(window);
        let ti = *places.get(index)?;
        let tab = self.capture_tab(ti)?;
        let leaves = self.windows[ti].tree.leaves();
        let panes = leaves.iter().filter_map(|v| self.surfaces.iter().find(|s| s.view == *v)).map(|s| s.pane.clone()).collect();
        // Its views go; its panes don't.
        let (gone, kept): (Vec<Surface>, Vec<Surface>) = mem::take(&mut self.surfaces).into_iter().partition(|s| leaves.contains(&s.view));
        self.surfaces = kept;
        for surface in &gone {
            // SAFETY: main thread; the window's content view holds the container.
            unsafe { call!(surface.container, c"removeFromSuperview") };
        }
        drop(gone);
        let was_shown = self.windows[ti].shown;
        self.windows.remove(ti);
        if places.len() > 1 {
            if was_shown {
                self.select_tab(window, index.min(places.len() - 2));
            } else {
                self.update_tab_bar(window);
            }
        }
        Some((tab, panes))
    }

    /// Puts a tab taken out (see `take_tab`) into `into` at `at`, shown.
    fn put_tab(&mut self, (tab, panes): (session::Tab, Vec<Arc<Pane>>), into: Id, at: usize) {
        self.restore_tab_with(&tab, into, &panes);
        let last = self.tabs_of(into).len() - 1;
        self.move_tab(into, last, at.min(last));
        self.select_tab(into, at.min(last));
        // SAFETY: main thread; a live NSWindow.
        unsafe { call!(into, c"makeKeyAndOrderFront:", NIL) };
    }

    /// `window`'s tab `index`, dragged off its bar and let go at `point` on
    /// the screen: a window of its own there. Its panes go on running; a
    /// window's only tab stays.
    fn detach_tab(&mut self, window: Id, index: usize, point: CGPoint) {
        if self.tabs_of(window).len() < 2 {
            return self.move_tab(window, index, index);
        }
        let Some((tab, panes)) = self.take_tab(window, index) else { return };
        let Some(new) = self.restore_tab_with(&tab, NIL, &panes) else { return };
        // Its title bar where the tab was let go.
        // SAFETY: main thread; documented signatures.
        unsafe {
            let mut frame = send_rect(new, c"frame");
            frame.origin = CGPoint { x: point.x - 60.0, y: point.y - frame.size.height + 14.0 };
            call!(new, c"setFrame:display:", frame, YES);
            call!(new, c"makeKeyAndOrderFront:", NIL);
        }
    }

    /// `window`'s tab `index` dragged onto `into`'s tab bar, to go at `at`.
    fn transfer_tab(&mut self, window: Id, index: usize, into: Id, at: usize) {
        if window == into || !self.windows.iter().any(|w| w.window == into) {
            return self.move_tab(window, index, index);
        }
        let alone = self.tabs_of(window).len() == 1;
        let Some(taken) = self.take_tab(window, index) else { return };
        self.put_tab(taken, into, at);
        if alone {
            // Emptied: the window goes, its tab being elsewhere now.
            // SAFETY: main thread; a live NSWindow.
            unsafe { call!(window, c"close") };
        }
    }

    /// `window` dragged by its title bar and let go over `into`'s tab bar:
    /// its tabs go there from `at`, and it closes, as AppKit merges windows.
    fn merge_window(&mut self, window: Id, into: Id, at: usize) {
        let count = self.tabs_of(window).len();
        for k in 0..count {
            let Some(taken) = self.take_tab(window, 0) else { break };
            self.put_tab(taken, into, at + k);
        }
        // SAFETY: main thread; a live NSWindow, empty now.
        unsafe { call!(window, c"close") };
    }

    /// Tabs are mightty's own, in the window, rather than AppKit's: always,
    /// now, but for the quick terminal, which has none.
    fn own_tabs(&self) -> bool {
        true
    }

    /// The tab `window` shows.
    fn shown(&self, window: Id) -> Option<&Window> {
        self.windows.iter().find(|w| w.window == window && w.shown)
    }

    fn shown_mut(&mut self, window: Id) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.window == window && w.shown)
    }

    /// The tab holding the pane `view`, by its index in `windows`.
    fn tab_index(&self, view: Id) -> Option<usize> {
        self.windows.iter().position(|w| w.tree.leaves().contains(&view))
    }

    /// `window`'s tabs, in order, by their index in `windows`.
    fn tabs_of(&self, window: Id) -> Vec<usize> {
        (0..self.windows.len()).filter(|&i| self.windows[i].window == window).collect()
    }

    /// A tab of mightty's own for `pane` in `window`, after its others;
    /// shown with `focus`.
    fn add_tab(&mut self, window: Id, pane: Arc<Pane>, hold: bool, focus: bool) -> Id {
        let Some(root) = self.shown(window).map(|w| w.root) else { return window };
        // SAFETY: main thread; documented signatures.
        let (frame, scale): (CGRect, f64) = unsafe { (send_rect(root, c"bounds"), msg!(window, c"backingScaleFactor")) };
        let renderer = self.new_renderer(scale);
        let view = self.add_surface(window, frame, renderer, pane.clone(), hold, false);
        self.windows.push(Window { window, root, shown: false, tree: Node::Leaf(view), active: view, zoomed: false, dragging: None });
        if focus {
            let index = self.tabs_of(window).len() - 1;
            self.select_tab(window, index);
        } else {
            pane.set_visible(false);
            self.layout(window, false);
            self.update_tab_bar(window);
        }
        window
    }

    /// Shows `window`'s tab `index` (of mightty's own), giving it the keys.
    fn select_tab(&mut self, window: Id, index: usize) {
        let tabs = self.tabs_of(window);
        let Some(&target) = tabs.get(index) else { return };
        for &t in &tabs {
            self.windows[t].shown = t == target;
        }
        let (leaves, active) = (self.windows[target].tree.leaves(), self.windows[target].active);
        for s in self.surfaces.iter_mut().filter(|s| s.window == window) {
            let inside = leaves.contains(&s.view);
            s.pane.set_visible(inside);
            s.active = inside && s.view == active;
            s.title.clear(); // the window shows the new tab's
        }
        self.update_tab_bar(window);
        self.layout(window, true);
        self.focus(active);
        self.sync_key_focus();
        for s in self.surfaces.iter_mut().filter(|s| leaves.contains(&s.view)) {
            s.render();
        }
    }

    /// The key window's tab `index` (the last, for `None`), or `step` from
    /// the one shown, when its tabs are mightty's own. False otherwise.
    fn select_own_tab(&mut self, index: Option<usize>, step: isize) -> bool {
        let Some((window, _)) = self.key_window() else { return false };
        let tabs = self.tabs_of(window);
        if !self.own_tabs() || tabs.is_empty() {
            return false;
        }
        let at = tabs.iter().position(|&t| self.windows[t].shown).unwrap_or(0);
        let n = tabs.len();
        let to = match index {
            _ if step != 0 => (at as isize + step).rem_euclid(n as isize) as usize,
            Some(i) => i.min(n - 1),
            None => n - 1,
        };
        self.select_tab(window, to);
        true
    }

    /// Closes the tab at `index` of `windows`, its window's other tabs
    /// staying; remembered to undo or reopen.
    fn close_tab(&mut self, index: usize) {
        let window = self.windows[index].window;
        if let Some(tab) = self.capture_tab(index) {
            // SAFETY: main thread; documented signature.
            let frame = unsafe { send_rect(window, c"frame") };
            let panes = self.windows[index].tree.leaves().iter().filter_map(|v| self.surfaces.iter().find(|s| s.view == *v)).map(|s| s.pane.clone()).collect();
            let leaves = self.windows[index].tree.leaves();
            let (closed, kept): (Vec<Surface>, Vec<Surface>) = mem::take(&mut self.surfaces).into_iter().partition(|s| leaves.contains(&s.view));
            self.surfaces = kept;
            let at = self.tabs_of(window).iter().position(|&t| t == index).unwrap_or(0);
            let was_shown = self.windows[index].shown;
            self.windows.remove(index);
            self.keep_closed(panes, Reopen::Tab { tab, tab_of: window, frame });
            for surface in closed {
                // SAFETY: main thread; the window's content view holds the container.
                unsafe { call!(surface.container, c"removeFromSuperview") };
                self.release_pane(&surface);
            }
            if was_shown {
                let left = self.tabs_of(window).len();
                self.select_tab(window, at.min(left.saturating_sub(1)));
            } else {
                self.update_tab_bar(window);
            }
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
            let scrollbar = Scrollbar::new(container);
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
                password: 0,
                ime_used_key: false,
                capture: false,
                typed: None,
                keys_down: Vec::new(),
                search: None,
                palette: None,
                scrollbar,
                title_override: None,
                backing: None,
                hover: None,
                pointer: PointerShape::Default,
                hint: None,
                active,
                window_attention: false,
                bell: false,
                flash: NIL,
            });
            view
        }
    }

    /// The key window and its active pane.
    fn key_window(&self) -> Option<(Id, Id)> {
        // SAFETY: main thread; documented signature.
        let key: Id = unsafe { msg!(msg!(class(c"NSApplication"), c"sharedApplication"), c"keyWindow") };
        self.shown(key).map(|w| (w.window, w.active))
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
    /// A new tab of `window`, its shell where the window's is.
    fn new_shell_tab_of(&mut self, window: Id) {
        let current = self.shown(window).and_then(|w| self.surfaces.iter().find(|s| s.view == w.active));
        let cwd = current.and_then(|s| self.cwd_of(s));
        let opts = SpawnOptions { cols: self.config.cols, rows: self.config.rows, cwd: cwd.clone(), ..Default::default() };
        match self.mux.spawn(opts) {
            Ok(pane) => {
                self.open_window_in(pane, false, window, true);
                if let Some(surface) = self.surfaces.last_mut() {
                    surface.start_dir = cwd;
                }
            }
            Err(e) => eprintln!("mightty: could not start a shell: {e}"),
        }
    }

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
        let tab = self.tab_index(at)?;
        let w = &mut self.windows[tab];
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
        for (i, w) in self.windows.iter().enumerate() {
            // SAFETY: main thread; documented signatures.
            let (number, tab, key) = unsafe {
                let number: isize = msg!(w.window, c"windowNumber");
                let tabs: Id = msg!(w.window, c"tabbedWindows");
                let tab: usize = if tabs.is_null() { 0 } else { msg!(tabs, c"indexOfObject:", w.window) };
                let key: Bool = msg!(w.window, c"isKeyWindow");
                // Among the window's own tabs, else its place in AppKit's.
                let own = self.tabs_of(w.window).iter().position(|&t| t == i).unwrap_or(0);
                (number, if tab == NS_NOT_FOUND { 0 } else { tab } + own, key != NO)
            };
            let leaves = w.tree.leaves();
            for s in self.surfaces.iter().filter(|s| leaves.contains(&s.view)) {
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
    /// Turns Secure Keyboard Entry on while mightty is the active app and
    /// it was asked for, or the key pane's program reads a password; off
    /// otherwise, as other apps need it off to take keys from the user.
    fn update_secure_input(&mut self) {
        // SAFETY: main thread; documented signature.
        let active: Bool = unsafe { msg!(msg!(class(c"NSApplication"), c"sharedApplication"), c"isActive") };
        let password = || {
            let Some((_, view)) = self.key_window() else { return false };
            self.surfaces.iter().find(|s| s.view == view).is_some_and(|s| s.pane.hides_input())
        };
        let on = active != NO && (self.secure_input || (self.config.auto_secure_input && password()));
        if on != self.secure_input_on {
            self.secure_input_on = on;
            // SAFETY: Carbon calls taking nothing; each on is matched by an off.
            unsafe {
                if on {
                    EnableSecureEventInput();
                } else {
                    DisableSecureEventInput();
                }
            }
        }
    }

    fn toggle_secure_input(&mut self) {
        self.secure_input = !self.secure_input;
        MENU.with_borrow(|items| {
            for (item, _) in items.iter().filter(|i| i.1 == Action::ToggleSecureInput) {
                // SAFETY: main thread; a menu item; NSControlStateValueOn is 1.
                unsafe { call!(*item, c"setState:", isize::from(self.secure_input)) };
            }
        });
        self.update_secure_input();
    }

    fn sync_key_focus(&mut self) {
        for w in &self.windows {
            // SAFETY: main thread; documented signature.
            let key: Bool = unsafe { msg!(w.window, c"isKeyWindow") };
            let key = key != NO && w.shown;
            let leaves = w.tree.leaves();
            for s in self.surfaces.iter_mut().filter(|s| leaves.contains(&s.view)) {
                let focused = key && s.view == w.active;
                if s.renderer.focused != focused && s.view == w.active {
                    s.set_focused(focused);
                } else if s.pane.set_focused(focused) {
                    run_on_main_with(0, on_pane_status);
                }
            }
        }
        self.update_secure_input();
    }

    /// Closes `view`'s pane. If it was its window's last, returns the window
    /// for the caller to close outside `with_app`.
    fn close_surface(&mut self, view: Id) -> Option<Id> {
        let i = self.surfaces.iter().position(|s| s.view == view)?;
        let window = self.surfaces[i].window;
        let tab = self.tab_index(view)?;
        // A tab's last pane: the tab closes, or the window with its last tab.
        if matches!(self.windows[tab].tree, Node::Leaf(_)) {
            if self.tabs_of(window).len() > 1 {
                self.close_tab(tab);
                return None;
            }
            return Some(window);
        }
        let w = &mut self.windows[tab];
        let order = w.tree.leaves();
        let place = w.tree.place_of(view);
        w.tree.remove(view);
        w.zoomed = false;
        let was_active = w.active == view;
        let surface = self.surfaces.remove(i);
        // SAFETY: main thread; the window's content view holds the container.
        unsafe { call!(surface.container, c"removeFromSuperview") };
        if let Some(place) = place {
            let cwd = self.cwd_of(&surface).map(|d| d.display().to_string());
            let what = Reopen::Pane { window, place, cwd, title: surface.title_override.clone() };
            self.keep_closed(vec![surface.pane.clone()], what);
        }
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
            // The quick terminal isn't a window to restore.
            if seen.contains(&w.window) || self.quick == Some(w.window) {
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
            // Each window's tabs: one with AppKit's, any number with mightty's own.
            let entries: Vec<usize> = tabs.iter().flat_map(|&t| self.tabs_of(t)).collect();
            let index = entries.iter().position(|&i| self.windows[i].window == selected && self.windows[i].shown).unwrap_or(0);
            let tabs: Vec<session::Tab> = entries.iter().filter_map(|&i| self.capture_tab(i)).collect();
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

    /// The tab at `index` of `windows`.
    fn capture_tab(&self, index: usize) -> Option<session::Tab> {
        let w = self.windows.get(index)?;
        let layout = self.capture_layout(&w.tree);
        let focused = w.tree.leaves().iter().position(|&v| v == w.active).unwrap_or(0);
        Some(session::Tab { layout, focused, zoomed: w.zoomed })
    }

    fn capture_layout(&self, node: &Node<Id>) -> Layout {
        match node {
            Node::Leaf(view) => {
                let surface = self.surfaces.iter().find(|s| s.view == *view);
                let cwd = surface.and_then(|s| self.cwd_of(s));
                Layout::Pane { cwd: cwd.map(|d| d.display().to_string()), title: surface.and_then(|s| s.title_override.clone()) }
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
                if self.tabs_of(window).len() > 1 {
                    self.select_tab(window, group.selected);
                }
            }
            any |= !windows.is_empty();
        }
        self.sync_key_focus();
        any
    }

    /// A window (a tab of `tab_of` unless nil) laid out as `tab`. Returns it.
    fn restore_tab(&mut self, tab: &session::Tab, tab_of: Id) -> Option<Id> {
        self.restore_tab_with(tab, tab_of, &[])
    }

    /// `restore_tab`, its panes being `live` (in layout order) as far as
    /// they go, new shells for the rest.
    fn restore_tab_with(&mut self, tab: &session::Tab, tab_of: Id, live: &[Arc<Pane>]) -> Option<Id> {
        let pane = match live.first() {
            Some(pane) => pane.clone(),
            None => self.spawn_shell(tab.layout.first_cwd())?,
        };
        self.open_window_in(pane, false, tab_of, tab_of.is_null());
        let (window, first) = self.windows.last().map(|w| (w.window, w.active))?;
        self.started_in(first, tab.layout.first_cwd());
        self.restore_split(&tab.layout, first, 0, live);
        let w = self.windows.iter_mut().rfind(|w| w.window == window)?;
        copy_ratios(&mut w.tree, &tab.layout);
        let order = w.tree.leaves();
        for (view, title) in order.iter().zip(tab.layout.titles()) {
            if let Some(surface) = self.surfaces.iter_mut().find(|s| s.view == *view) {
                surface.title_override = title.map(str::to_owned);
            }
        }
        let focused = order[tab.focused.min(order.len() - 1)];
        w.zoomed = tab.zoomed && order.len() > 1;
        self.focus(focused);
        self.layout(window, false);
        Some(window)
    }

    /// Splits `view`'s space as `layout` says, a new shell in each new pane
    /// unless `live` has the pane: `layout`'s first is its pane `at`.
    fn restore_split(&mut self, layout: &Layout, view: Id, at: usize, live: &[Arc<Pane>]) {
        let Layout::Split { axis, first, second, .. } = layout else { return };
        let second_at = at + first.titles().len();
        let pane = match live.get(second_at) {
            Some(pane) => pane.clone(),
            None => {
                let Some(pane) = self.spawn_shell(second.first_cwd()) else { return };
                pane
            }
        };
        let Some(new) = self.insert_split(view, pane, *axis, false, false) else { return };
        self.started_in(new, second.first_cwd());
        self.restore_split(first, view, at, live);
        self.restore_split(second, new, second_at, live);
    }

    /// Remembers what was closed. Its panes not shown elsewhere and still
    /// running are kept running, hidden, for `undo-timeout`; those of the
    /// oldest close past `CLOSED_KEPT` end.
    fn keep_closed(&mut self, panes: Vec<Arc<Pane>>, what: Reopen) {
        // Nothing to bring back from a shell that exited.
        if !panes.is_empty() && panes.iter().all(|p| p.exit_status().is_some()) {
            return;
        }
        let timeout = self.config.undo_timeout;
        let live: Vec<Arc<Pane>> = if timeout.is_zero() {
            Vec::new()
        } else {
            panes.into_iter().filter(|p| p.exit_status().is_none() && !self.surfaces.iter().any(|s| s.pane.id == p.id)).collect()
        };
        for pane in &live {
            pane.set_visible(false);
        }
        self.closed.push(Closed { live, until: Instant::now() + timeout, what });
        if self.closed.len() > CLOSED_KEPT {
            let oldest = self.closed.remove(0);
            self.end_panes(oldest.live);
        }
        if !timeout.is_zero() {
            run_on_main_after(timeout + Duration::from_millis(10), on_undo_expiry);
        }
    }

    /// Ends panes kept for undo.
    fn end_panes(&self, panes: Vec<Arc<Pane>>) {
        for pane in panes {
            if !self.surfaces.iter().any(|s| s.pane.id == pane.id) {
                self.mux.close(pane.id);
            }
        }
    }

    /// Brings back the last close: with `live_only` (undo), only while its
    /// panes still run; else (reopen) with new shells for those that don't.
    fn reopen_closed(&mut self, live_only: bool) {
        let ready = self.closed.last().is_some_and(|c| !live_only || !c.live.is_empty());
        let Some(Closed { live, what, .. }) = ready.then(|| self.closed.pop()).flatten() else {
            // SAFETY: plain AppKit call.
            return unsafe { NSBeep() };
        };
        match what {
            Reopen::Pane { window, place, cwd, title } => {
                let pane = match live.into_iter().next() {
                    Some(pane) => pane,
                    None => {
                        let Some(pane) = self.spawn_shell(cwd.as_deref()) else { return };
                        pane
                    }
                };
                let tab = self.windows.iter().position(|w| w.window == window && w.tree.leaves().iter().any(|v| place.sibling.contains(v)));
                let view = if let Some(tab) = tab {
                    // Its tab shown first, if hidden.
                    if !self.windows[tab].shown {
                        let index = self.tabs_of(window).iter().position(|&t| t == tab).unwrap_or(0);
                        self.select_tab(window, index);
                    }
                    // SAFETY: main thread; documented signatures.
                    let (frame, scale): (CGRect, f64) =
                        unsafe { (send_rect(msg!(window, c"contentView"), c"bounds"), msg!(window, c"backingScaleFactor")) };
                    let renderer = self.new_renderer(scale);
                    let view = self.add_surface(window, frame, renderer, pane, false, true);
                    if let Some(tab) = self.tab_index(place.sibling.iter().copied().find(|&v| self.tab_index(v).is_some()).unwrap_or(NIL)) {
                        self.windows[tab].tree.put_back(view, &place);
                        self.windows[tab].zoomed = false;
                    }
                    self.focus(view);
                    // SAFETY: main thread; a live NSWindow.
                    unsafe { call!(window, c"makeKeyAndOrderFront:", NIL) };
                    self.layout(window, false);
                    Some(view)
                } else {
                    // Its window is gone: a window of its own.
                    self.open_window_in(pane, false, NIL, true);
                    self.windows.last().map(|w| w.active)
                };
                if let Some(surface) = view.and_then(|v| self.surfaces.iter_mut().find(|s| s.view == v)) {
                    surface.title_override = title;
                    surface.start_dir = cwd.map(PathBuf::from);
                }
            }
            Reopen::Tab { tab, tab_of, frame } => {
                let tab_of = if self.windows.iter().any(|w| w.window == tab_of) { tab_of } else { NIL };
                let Some(window) = self.restore_tab_with(&tab, tab_of, &live) else { return };
                // SAFETY: main thread; documented signatures.
                unsafe {
                    if tab_of.is_null() {
                        call!(window, c"setFrame:display:", frame, YES);
                    }
                    call!(window, c"makeKeyAndOrderFront:", NIL);
                }
                // Brought back in front, among the window's own tabs too.
                let tabs = self.tabs_of(window).len();
                if tabs > 1 && self.own_tabs() {
                    self.select_tab(window, tabs - 1);
                }
            }
        }
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
        let held = self.closed.iter().any(|c| c.live.iter().any(|p| p.id == closed.pane.id));
        if !held && !self.surfaces.iter().any(|s| s.pane.id == closed.pane.id) {
            closed.pane.set_visible(false);
            self.mux.close(closed.pane.id);
        }
    }

    /// Forgets a closed window, ending its panes.
    fn window_closed(&mut self, window: Id) {
        let mut reopen = Vec::new();
        if self.quick == Some(window) {
            self.quick = None;
        }
        for index in self.tabs_of(window).into_iter().filter(|_| self.quick != Some(window)) {
            let Some(tab) = self.capture_tab(index) else { continue };
            // SAFETY: main thread; documented signatures.
            let (tab_of, frame) = unsafe {
                let group: Id = msg!(window, c"tabbedWindows");
                let count: usize = if group.is_null() { 0 } else { msg!(group, c"count") };
                let others = (0..count).map(|i| -> Id { msg!(group, c"objectAtIndex:", i) }).find(|&w| w != window);
                (others.unwrap_or(NIL), send_rect(window, c"frame"))
            };
            let leaves = self.windows[index].tree.leaves();
            let panes = leaves.iter().filter_map(|v| self.surfaces.iter().find(|s| s.view == *v)).map(|s| s.pane.clone()).collect();
            reopen.push((panes, Reopen::Tab { tab, tab_of, frame }));
        }
        self.windows.retain(|w| w.window != window);
        self.bars.retain(|b| b.0 != window);
        let (closed, kept) = mem::take(&mut self.surfaces).into_iter().partition(|s| s.window == window);
        self.surfaces = kept;
        // Once its panes aren't counted as shown.
        for (panes, what) in reopen {
            self.keep_closed(panes, what);
        }
        for surface in closed {
            self.release_pane(&surface);
        }
    }

    /// Makes `view` the pane keys go to: the first responder of its window.
    fn focus(&mut self, view: Id) {
        self.activate(view);
        if self.tab_index(view).is_some_and(|t| !self.windows[t].shown) {
            return; // remembered for when its tab shows
        }
        let Some(window) = self.surfaces.iter().find(|s| s.view == view).map(|s| s.window) else { return };
        // SAFETY: main thread; documented signature. Its becomeFirstResponder
        // finds the app busy and leaves the work done above.
        unsafe { call!(window, c"makeFirstResponder:", view) };
    }

    /// Marks `view` as its window's active pane, the others as not.
    fn activate(&mut self, view: Id) {
        let Some(window) = self.surfaces.iter().find(|s| s.view == view).map(|s| s.window) else { return };
        let Some(tab) = self.tab_index(view) else { return };
        let w = &mut self.windows[tab];
        let zoomed_away = w.zoomed && w.active != view;
        w.active = view;
        if zoomed_away {
            w.zoomed = false;
        }
        if !w.shown {
            return;
        }
        let leaves = w.tree.leaves();
        // SAFETY: main thread; documented signature.
        let key: Bool = unsafe { msg!(window, c"isKeyWindow") };
        for surface in self.surfaces.iter_mut().filter(|s| leaves.contains(&s.view)) {
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
        let Some(w) = self.windows.iter().find(|w| w.window == window && w.shown) else { return };
        let bounds = self.pane_bounds(window, w.root);
        // SAFETY: main thread; documented signatures.
        unsafe {
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
        register_global_keys(&config.keybinds);
        for w in &self.windows {
            style_window(w.window, w.root, &config);
        }
        for surface in &mut self.surfaces {
            let size = if font_changed { config.font_size } else { surface.renderer.font_size() };
            surface.renderer.configure(&config, size);
            surface.sync_backing();
            // The cell size may have changed, so the grid too; hidden panes as well.
            surface.resize();
        }
        BEHAVIOUR.set(config.behaviour);
        let sound_problem = load_bell_sound(config.bell_audio.as_deref());
        self.config = config;
        let windows: Vec<Id> = self.windows.iter().map(|w| w.window).collect();
        for window in windows {
            // Colours and style may have changed: the bar drawn again.
            for bar in self.bars.iter_mut().filter(|b| b.0 == window) {
                bar.2 = None;
            }
            self.update_tab_bar(window);
            // The title bar may cover the content now, or not.
            self.layout(window, false);
            self.dim(window);
        }
        let warnings: Vec<String> = warnings.into_iter().chain(sound_problem).collect();
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
    fn open_config(&mut self) {
        let Some(path) = self.config.file().map(PathBuf::from).or_else(|| Some(config::config_dir()?.join("config"))) else {
            return show_problems("There's no config file", &["HOME is unset".into()]);
        };
        if !path.exists() {
            let created = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path, CONFIG_TEMPLATE));
            if let Err(e) = created {
                return show_problems("The config file couldn't be created", &[format!("{}: {e}", path.display())]);
            }
        }
        // In the user's editor, in the terminal: a tab of the key window, else a window.
        let command = format!("exec ${{VISUAL:-${{EDITOR:-vi}}}} {}", shell_quote(&path.display().to_string()));
        self.open_command(command, path.parent().map(Path::to_path_buf), "Config", true);
    }

    /// The settings, in a window of their own: `mightty config`.
    fn open_settings(&mut self) {
        let Ok(exe) = env::current_exe() else { return };
        let command = format!("exec {} config", shell_quote(&exe.display().to_string()));
        self.open_command(command, None, "Settings", false);
    }

    /// Runs `command` (through the login shell) in a new tab of the key
    /// window if `tab`, else a window, titled `title`.
    fn open_command(&mut self, command: String, cwd: Option<PathBuf>, title: &str, tab: bool) {
        let opts = SpawnOptions { command: Some(command), cwd, cols: self.config.cols.max(100), rows: self.config.rows.max(32), ..Default::default() };
        let pane = match self.mux.spawn(opts) {
            Ok(pane) => pane,
            Err(e) => return show_problems("It couldn't be started", &[e.to_string()]),
        };
        let tab_of = if tab { self.key_window().map_or(NIL, |k| k.0) } else { NIL };
        self.open_window_in(pane, false, tab_of, true);
        if let Some(surface) = self.surfaces.last_mut() {
            surface.title_override = Some(title.to_owned());
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
        let Some(w) = self.shown(window) else { return };
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
        let Some(w) = self.shown(window) else { return };
        let order = w.tree.leaves();
        let at = order.iter().position(|&v| v == active).unwrap_or(0) as isize;
        let next = order[(at + step).rem_euclid(order.len() as isize) as usize];
        self.focus(next);
    }

    fn select_split_toward(&mut self, direction: Direction) {
        let Some((window, active)) = self.key_window() else { return };
        let Some(w) = self.shown(window) else { return };
        let bounds = self.pane_bounds(window, w.root);
        if let Some(next) = split::neighbor(&w.tree.frames(bounds, GAP), active, direction) {
            self.focus(next);
        }
    }

    /// ⌘⌃ and an arrow: moves the nearest divider `step` points that way
    /// (as Ghostty's `resize_split`).
    fn move_divider(&mut self, direction: Direction, step: f64) {
        let Some((window, active)) = self.key_window() else { return };
        let Some(root) = self.shown(window).map(|w| w.root) else { return };
        let bounds = self.pane_bounds(window, root);
        let Some(w) = self.shown_mut(window).filter(|w| !w.zoomed) else { return };
        let (axis, points) = match direction {
            Direction::Left => (Axis::Row, -step),
            Direction::Right => (Axis::Row, step),
            Direction::Up => (Axis::Column, -step),
            Direction::Down => (Axis::Column, step),
        };
        if w.tree.move_divider(active, axis, points, bounds, GAP, MIN_PANE) {
            self.layout(window, false);
        }
    }

    fn toggle_zoom(&mut self) {
        let Some((window, _)) = self.key_window() else { return };
        let Some(w) = self.shown_mut(window) else { return };
        if matches!(w.tree, Node::Split { .. }) {
            w.zoomed = !w.zoomed;
            self.layout(window, false);
        }
    }

    fn equalize(&mut self) {
        let Some((window, _)) = self.key_window() else { return };
        let Some(w) = self.shown_mut(window) else { return };
        w.tree.equalize();
        self.layout(window, false);
    }

    /// The divider of `root`'s window under `point` (in `root`'s coordinates).
    fn divider_at(&self, root: Id, point: CGPoint) -> Option<Divider> {
        let w = self.windows.iter().find(|w| w.root == root && w.shown).filter(|w| !w.zoomed)?;
        let bounds = self.pane_bounds(w.window, root);
        w.tree.dividers(bounds, GAP).into_iter().find(|d| grab_area(d).contains(point.x, point.y))
    }

    /// Drags the divider grabbed in `root` (see `divider_at`) to `point`.
    fn drag_divider(&mut self, root: Id, point: CGPoint) {
        let Some(w) = self.windows.iter_mut().find(|w| w.root == root && w.shown) else { return };
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

/// Asks on a sheet over `window` for a title for the pane `view`, starting
/// from `current`. Empty goes back to the program's title.
fn ask_title(window: Id, view: Id, current: &str) {
    thread_local! {
        /// The pane being renamed, and the field (retained) its title is typed in.
        static RENAMING: Cell<Option<(Id, Id)>> = const { Cell::new(None) };
    }
    static ANSWERED: Block = Block::with_response(answered);
    unsafe extern "C" fn answered(_: *const Block, response: isize) {
        let Some((view, field)) = RENAMING.take() else { return };
        // SAFETY: main thread; the field was retained when the sheet opened.
        let text = unsafe {
            let text = string_from_ns(msg!(field, c"stringValue"));
            call!(field, c"release");
            text
        };
        if response == ALERT_FIRST_BUTTON {
            let title = text.trim();
            with_app(|app| {
                let window = app.surfaces.iter_mut().find(|s| s.view == view).map(|s| {
                    s.title_override = (!title.is_empty()).then(|| title.to_owned());
                    s.render();
                    s.window
                })?;
                // The tab bar shows it at once, not with the next output.
                app.update_tab_bar(window);
                Some(())
            });
        }
    }
    // SAFETY: main thread; documented signatures; the handler is a global block.
    unsafe {
        let sheet: Id = msg!(window, c"attachedSheet");
        if !sheet.is_null() {
            return;
        }
        let alert: Id = msg!(new_object(c"NSAlert"), c"autorelease");
        call!(alert, c"setMessageText:", nsstring("Change Title"));
        call!(alert, c"setInformativeText:", nsstring("Leave blank to use the title the running program sets."));
        let _: Id = msg!(alert, c"addButtonWithTitle:", nsstring("OK"));
        let _: Id = msg!(alert, c"addButtonWithTitle:", nsstring("Cancel"));
        let frame = CGRect { origin: CGPoint::default(), size: CGSize { width: 300.0, height: 24.0 } };
        let field: Id = msg!(msg!(class(c"NSTextField"), c"alloc"), c"initWithFrame:", frame);
        call!(field, c"setStringValue:", nsstring(current));
        call!(alert, c"setAccessoryView:", field);
        RENAMING.set(Some((view, field)));
        call!(alert, c"beginSheetModalForWindow:completionHandler:", window, &ANSWERED);
        // Typing goes straight into the field, its text selected.
        let sheet: Id = msg!(alert, c"window");
        call!(sheet, c"makeFirstResponder:", field);
        call!(field, c"selectText:", NIL);
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

/// Registers the `global:` keybinds, saying which couldn't be.
fn register_global_keys(bindings: &Bindings) {
    fn perform_global(action: &Action) {
        let view = with_app(|app| app.key_window().map(|k| k.1)).flatten().unwrap_or(NIL);
        perform(view, action);
    }
    let failed = hotkey::register(bindings, perform_global);
    if !failed.is_empty() {
        let keys: Vec<String> = failed.iter().map(|t| format!("global:{t}: taken by another app, or not a key on a US keyboard")).collect();
        show_problems("Some global keys couldn't be set", &keys);
    }
}

/// The quick terminal's window class: borderless, yet it takes keys.
fn quick_window_class() -> Id {
    thread_local! {
        static CLASS: std::cell::OnceCell<Id> = const { std::cell::OnceCell::new() };
    }
    extern "C" fn yes(_: Id, _: Sel) -> Bool {
        YES
    }
    /// The slide's length, whatever the distance (AppKit's grows with it).
    extern "C" fn slide_time(_: Id, _: Sel, _frame: CGRect) -> f64 {
        QUICK_SLIDE
    }
    /// Left where it's put: AppKit would pull it below the menu bar, so it
    /// couldn't start or end its slide off the top of the screen.
    extern "C" fn unconstrained(_: Id, _: Sel, frame: CGRect, _screen: Id) -> CGRect {
        frame
    }
    CLASS.with(|cell| {
        *cell.get_or_init(|| {
            let yes = yes as extern "C" fn(Id, Sel) -> Bool as *const c_void;
            let slide_time = slide_time as extern "C" fn(Id, Sel, CGRect) -> f64 as *const c_void;
            let unconstrained = unconstrained as extern "C" fn(Id, Sel, CGRect, Id) -> CGRect as *const c_void;
            // SAFETY: a new subclass, its methods with their signatures.
            unsafe {
                register_class(c"MighttyQuickWindow", c"NSWindow", &[
                    (c"canBecomeKeyWindow", yes, c"c@:"),
                    (c"canBecomeMainWindow", yes, c"c@:"),
                    (c"animationResizeTime:", slide_time, c"d@:{CGRect={CGPoint=dd}{CGSize=dd}}"),
                    (
                        c"constrainFrameRect:toScreen:",
                        unconstrained,
                        c"{CGRect={CGPoint=dd}{CGSize=dd}}@:{CGRect={CGPoint=dd}{CGSize=dd}}@",
                    ),
                ])
            }
        })
    })
}

/// How long the quick terminal takes to slide in or out.
const QUICK_SLIDE: f64 = 0.2;

/// The quick terminal's frame for `position` and `size` on the screen with
/// the mouse, and where it slides in from: off that edge.
fn quick_frames(position: QuickPosition, (deep, along): (Extent, Option<Extent>)) -> (CGRect, CGRect) {
    // SAFETY: main thread; documented signatures.
    let area = unsafe {
        let mouse: CGPoint = msg!(class(c"NSEvent"), c"mouseLocation");
        let screens: Id = msg!(class(c"NSScreen"), c"screens");
        let count: usize = msg!(screens, c"count");
        let screen = (0..count)
            .map(|i| -> Id { msg!(screens, c"objectAtIndex:", i) })
            .find(|&s| {
                let f = send_rect(s, c"frame");
                (f.origin.x..f.origin.x + f.size.width).contains(&mouse.x) && (f.origin.y..f.origin.y + f.size.height).contains(&mouse.y)
            })
            .unwrap_or_else(|| msg!(class(c"NSScreen"), c"mainScreen"));
        send_rect(screen, c"visibleFrame")
    };
    let (x, y, w, h) = (area.origin.x, area.origin.y, area.size.width, area.size.height);
    let rect = |x, y, w, h| CGRect { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } };
    // Along its edge: all of it unless asked, centred.
    let (across_x, across_y) = (along.map_or(w, |a| a.of(w)), along.map_or(h, |a| a.of(h)));
    let (cx, cy) = (x + (w - across_x) / 2.0, y + (h - across_y) / 2.0);
    match position {
        QuickPosition::Top => {
            let d = deep.of(h);
            (rect(cx, y + h - d, across_x, d), rect(cx, y + h, across_x, d))
        }
        QuickPosition::Bottom => {
            let d = deep.of(h);
            (rect(cx, y, across_x, d), rect(cx, y - d, across_x, d))
        }
        QuickPosition::Left => {
            let d = deep.of(w);
            (rect(x, cy, d, across_y), rect(x - d, cy, d, across_y))
        }
        QuickPosition::Right => {
            let d = deep.of(w);
            (rect(x + w - d, cy, d, across_y), rect(x + w, cy, d, across_y))
        }
        // Width, then height (the same share when left out).
        QuickPosition::Center => {
            let (width, height) = (deep.of(w), along.unwrap_or(deep).of(h));
            let frame = rect(x + (w - width) / 2.0, y + (h - height) / 2.0, width, height);
            (frame, frame)
        }
    }
}

thread_local! {
    /// `on_scrollbar_fade` is scheduled.
    static FADE_SCHEDULED: Cell<bool> = const { Cell::new(false) };
}

/// Ends the panes kept for undo past `undo-timeout`; what was closed can
/// still be reopened, with new shells.
extern "C" fn on_undo_expiry(_: *mut c_void) {
    let now = Instant::now();
    with_app(|app| {
        let due: Vec<Arc<Pane>> = app.closed.iter_mut().filter(|c| c.until <= now).flat_map(|c| mem::take(&mut c.live)).collect();
        app.end_panes(due);
    });
}

/// A click on a window's tab bar.
fn on_tab_click(bar: Id, click: tabbar::Click) {
    let Some(window) = with_app(|app| app.bars.iter().find(|b| b.1.bar == bar).map(|b| b.0)).flatten() else { return };
    let tab = |app: &App, i: usize| app.tabs_of(window).get(i).copied();
    match click {
        tabbar::Click::Select(i) => drop(with_app(|app| app.select_tab(window, i))),
        tabbar::Click::Close(i) => {
            let last = with_app(|app| app.tabs_of(window).len() <= 1).unwrap_or(true);
            if last {
                // SAFETY: main thread; a live NSWindow.
                unsafe { call!(window, c"performClose:", NIL) };
            } else {
                with_app(|app| tab(app, i).map(|t| app.close_tab(t)));
            }
        }
        tabbar::Click::Rename(i) => drop(with_app(|app| {
            app.select_tab(window, i);
            let active = app.windows[tab(app, i)?].active;
            let s = app.surfaces.iter().find(|s| s.view == active)?;
            let text = match &s.title_override {
                Some(title) => title.clone(),
                None => s.pane.term.lock().unwrap().title().trim_start_matches(['●', ' ']).to_owned(),
            };
            app.bars.iter().find(|b| b.0 == window)?.1.edit(i, &text);
            Some(())
        })),
        tabbar::Click::Retitle(i, text) => drop(with_app(|app| {
            let active = app.windows[tab(app, i)?].active;
            if let Some(text) = text {
                let s = app.surfaces.iter_mut().find(|s| s.view == active)?;
                s.title_override = (!text.is_empty()).then_some(text);
                s.title.clear();
                s.render();
                for bar in app.bars.iter_mut().filter(|b| b.0 == window) {
                    bar.2 = None;
                }
                app.update_tab_bar(window);
            }
            // The keys back to the terminal.
            app.focus(active);
            Some(())
        })),
        tabbar::Click::CloseOthers(i) => drop(with_app(|app| {
            let kept = app.windows[tab(app, i)?].active;
            // One at a time, the indices moving as tabs go; each undoable.
            while let Some(&other) = app.tabs_of(window).iter().find(|&&t| !app.windows[t].tree.leaves().contains(&kept)) {
                app.close_tab(other);
            }
            Some(())
        })),
        tabbar::Click::NewWindow(i) => drop(with_app(|app| {
            // SAFETY: main thread; a live NSWindow.
            let frame = unsafe { send_rect(window, c"frame") };
            // Cascaded from this one, as a new window would be.
            app.detach_tab(window, i, CGPoint { x: frame.origin.x + 90.0, y: frame.origin.y + frame.size.height - 44.0 });
        })),
        tabbar::Click::New => drop(with_app(|app| app.new_shell_tab_of(window))),
        tabbar::Click::Move(from, to) => drop(with_app(|app| app.move_tab(window, from, to))),
        tabbar::Click::Detach(i, (x, y)) => drop(with_app(|app| app.detach_tab(window, i, CGPoint { x, y }))),
        tabbar::Click::Transfer(i, bar, at) => drop(with_app(|app| {
            let into = app.bars.iter().find(|b| b.1.bar == bar).map(|b| b.0)?;
            app.transfer_tab(window, i, into, at);
            Some(())
        })),
    }
}

thread_local! {
    /// `bell-audio-path`'s sound, loaded (ours, +1), or nil.
    static BELL_SOUND: Cell<Id> = const { Cell::new(NIL) };
}

/// Loads `path` as the bell's sound (see `bell-audio-path`), saying why not.
fn load_bell_sound(path: Option<&Path>) -> Option<String> {
    // SAFETY: main thread; documented signatures; the old sound was ours.
    unsafe {
        let old = BELL_SOUND.replace(NIL);
        if !old.is_null() {
            call!(old, c"release");
        }
        let path = path?;
        let sound: Id = msg!(msg!(class(c"NSSound"), c"alloc"), c"initWithContentsOfFile:byReference:", nsstring(&path.display().to_string()), YES);
        if sound.is_null() {
            return Some(format!("bell-audio-path: {} isn't a sound macOS can play", path.display()));
        }
        BELL_SOUND.set(sound);
        None
    }
}

thread_local! {
    /// The window being dragged by its title bar, while the button's down.
    static WINDOW_DRAG: Cell<Option<Id>> = const { Cell::new(None) };
}

/// How often a window dragged by its title bar is checked for being over a tab bar.
const WINDOW_DRAG_TICK: Duration = Duration::from_millis(30);

/// A window dragged by its title bar: over another window's tab bar, a gap
/// opens there; let go there, its tabs join that window. Checked on a timer
/// while the button's down, as AppKit moves the window itself.
extern "C" fn on_window_drag_tick(_: *mut c_void) {
    let Some(window) = WINDOW_DRAG.get() else { return };
    // SAFETY: main thread; documented class methods.
    let (buttons, mouse): (usize, CGPoint) = unsafe { (msg!(class(c"NSEvent"), c"pressedMouseButtons"), msg!(class(c"NSEvent"), c"mouseLocation")) };
    // All its tabs, in order: they'd all go.
    let titles = with_app(|app| app.tabs_of(window).iter().map(|&t| app.tab_title(t)).collect::<Vec<_>>()).filter(|t| !t.is_empty());
    let Some(titles) = titles else {
        WINDOW_DRAG.set(None);
        tabbar::hover(None, NIL, &[]);
        // SAFETY: main thread; the window may be closing, and is shown as it was.
        unsafe { call!(window, c"setAlphaValue:", 1.0f64) };
        return;
    };
    let target = tabbar::hover(Some(mouse), window, &titles);
    // Over a tab bar the window gives way to the tab it would become, as
    // AppKit's does, so the gap it would go into shows.
    // SAFETY: main thread; a live NSWindow.
    unsafe { call!(window, c"setAlphaValue:", if target.is_some() { 0.0f64 } else { 1.0f64 }) };
    if buttons & 1 != 0 {
        run_on_main_after(WINDOW_DRAG_TICK, on_window_drag_tick);
        return;
    }
    WINDOW_DRAG.set(None);
    tabbar::hover(None, NIL, &[]);
    // SAFETY: main thread; a live NSWindow.
    unsafe { call!(window, c"setAlphaValue:", 1.0f64) };
    if let Some((bar, at)) = target {
        with_app(|app| {
            let into = app.bars.iter().find(|b| b.1.bar == bar).map(|b| b.0)?;
            app.merge_window(window, into, at);
            Some(())
        });
    }
}

/// `text` as one word for the shell, quoted.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The config file's modification time and size, to see it change.
fn config_stamp(path: Option<&Path>) -> Option<(std::time::SystemTime, u64)> {
    let meta = fs::metadata(path?).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

thread_local! {
    /// The config file as it was last seen (see `on_config_tick`).
    static CONFIG_SEEN: Cell<Option<(std::time::SystemTime, u64)>> = const { Cell::new(None) };
}

/// How often the config file is looked at for changes.
const CONFIG_TICK: Duration = Duration::from_secs(1);

/// The config file changed (saved by an editor, or by `mightty config`):
/// it's applied, as by Reload Configuration.
extern "C" fn on_config_tick(_: *mut c_void) {
    with_app(|app| {
        let path = app.config.file().map(Path::to_path_buf).or_else(|| Some(config::config_dir()?.join("config")));
        let now = config_stamp(path.as_deref());
        if CONFIG_SEEN.replace(now) != now {
            app.reload_config();
        }
    });
    run_on_main_after(CONFIG_TICK, on_config_tick);
}

/// The mouse on a pane's scrollbar.
fn on_scrollbar_mouse(bar: Id, drag: scrollbar::Drag) {
    with_app(|app| app.surfaces.iter_mut().find(|s| s.scrollbar.bar == bar).map(|s| s.scrollbar_mouse(drag)));
}

/// Hides scrollbars once they've lingered, unless already scheduled to.
fn schedule_scrollbar_fade() {
    if !FADE_SCHEDULED.replace(true) {
        run_on_main_after(scrollbar::LINGER, on_scrollbar_fade);
    }
}

/// Fades the scrollbars due to, and waits for the next.
extern "C" fn on_scrollbar_fade(_: *mut c_void) {
    FADE_SCHEDULED.set(false);
    let now = Instant::now();
    let next = with_app(|app| app.surfaces.iter_mut().filter_map(|s| s.scrollbar.fade(now)).min()).flatten();
    if let Some(next) = next {
        FADE_SCHEDULED.set(true);
        run_on_main_after(next.saturating_duration_since(now), on_scrollbar_fade);
    }
}

/// `macos-titlebar-style` for `window`, unless it has no title bar (the
/// quick terminal).
unsafe fn style_titlebar(window: Id, config: &Config, translucent: bool) {
    const TITLED: usize = 1;
    const FULL_SIZE_CONTENT: usize = 1 << 15;
    unsafe {
        let mask: usize = msg!(window, c"styleMask");
        if mask & TITLED == 0 {
            return;
        }
        let style = config.behaviour.titlebar;
        let native = style == Titlebar::Native;
        // Native tabs sit under the title bar: a window that has some keeps
        // one, transparent, rather than the tab bar covering the terminal.
        let group: Id = msg!(window, c"tabbedWindows");
        let tabs: usize = if group.is_null() { 0 } else { msg!(group, c"count") };
        let hidden = style == Titlebar::Hidden && tabs <= 1;
        // Tabs in the title bar's place: no title, the buttons over the bar.
        let tabbed = style == Titlebar::Tabs;
        let mask = if hidden || tabbed { mask | FULL_SIZE_CONTENT } else { mask & !FULL_SIZE_CONTENT };
        call!(window, c"setStyleMask:", mask);
        call!(window, c"setTitlebarAppearsTransparent:", if native { NO } else { YES });
        call!(window, c"setTitleVisibility:", if hidden || tabbed { 1isize } else { 0isize });
        for button in 0..3usize {
            // Close, minimize, zoom.
            let button: Id = msg!(window, c"standardWindowButton:", button);
            if !button.is_null() {
                call!(button, c"setHidden:", if hidden { YES } else { NO });
            }
        }
        if !native && !translucent {
            // The title bar shows the window's background: the terminal's.
            call!(window, c"setBackgroundColor:", ns_color(config.theme.background, 1.0, config.colorspace));
        }
        // Its title and buttons drawn to suit that background.
        let (r, g, b) = config.theme.background;
        let dark = (0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64) < 128.0;
        let name = if dark { "NSAppearanceNameDarkAqua" } else { "NSAppearanceNameAqua" };
        let appearance: Id = if native { NIL } else { msg!(class(c"NSAppearance"), c"appearanceNamed:", nsstring(name)) };
        call!(window, c"setAppearance:", appearance);
    }
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

/// An `NSColor` in `space` (autoreleased).
fn ns_color((r, g, b): (u8, u8, u8), alpha: f64, space: Colorspace) -> Id {
    let c = |v: u8| v as f64 / 255.0;
    let make = match space {
        Colorspace::Srgb => c"colorWithSRGBRed:green:blue:alpha:",
        Colorspace::DisplayP3 => c"colorWithDisplayP3Red:green:blue:alpha:",
    };
    // SAFETY: documented signature.
    unsafe { msg!(class(c"NSColor"), make, c(r), c(g), c(b), alpha) }
}

/// A `CGColor` in `space`, for layers (autoreleased with its `NSColor`).
fn color_alpha(rgb: (u8, u8, u8), alpha: f64, space: Colorspace) -> Id {
    // SAFETY: documented signature.
    unsafe { msg!(ns_color(rgb, alpha, space), c"CGColor") }
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
        style_titlebar(window, config, translucent);
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
            let (frame, clipboard, pointer, bar, rang, font) = {
                let mut term = self.pane.term.lock().unwrap();
                // Animated images' next frames, or a stale progress bar to take down.
                self.animation_due = match (term.animate(Instant::now()), term.progress_expiry()) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                if let Some(due) = self.animation_due {
                    schedule_animation(due);
                }
                let rang = term.take_bell();
                if rang && BEHAVIOUR.get().bell.title {
                    self.bell = true;
                }
                let clipboard = term.take_clipboard_requests();
                let pointer = term.pointer_shape();
                let font = term.take_font_request();
                let bar = (term.scrollback().len(), term.rows(), term.is_alt_screen());
                // A program can't set the attention marker itself.
                let own = match &self.title_override {
                    Some(title) => title.as_str(),
                    None => term.title().trim_start_matches(['●', ' ']),
                };
                let mut title = if own.is_empty() { self.pane.command.clone() } else { own.to_owned() };
                if let Some(code) = self.pane.exit_status() {
                    title += &code.map_or(" — exited".into(), |c| format!(" — exited ({c})"));
                }
                // The window shows its active pane's title, marked if any of its panes needs the user.
                if self.bell {
                    title.insert_str(0, "🔔 ");
                }
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
                    } else if self.password > 0 {
                        // Gone once the program reads the line, echoing again.
                        if self.pane.hides_input() {
                            self.snapshot.overlay(&"*".repeat(self.password), term.cursor());
                        } else {
                            self.password = 0;
                        }
                    }
                    Some(selection_range(self.selection, &term))
                };
                (frame, clipboard, pointer, bar, rang, font)
            };
            for request in clipboard {
                self.clipboard_request(request);
            }
            if rang {
                self.ring();
            }
            // Another font for this pane (OSC 50): its cells, so its grid,
            // may change; drawn once it's fitted to them, below.
            let refit = font.is_some_and(|family| self.renderer.set_family(family));
            let (history, rows, alt) = bar;
            let dark = self.dark();
            self.scrollbar.update(history, rows, self.scroll_offset, BEHAVIOUR.get().scrollbar && !alt, dark);
            if self.scrollbar.fade(Instant::now()).is_some() {
                schedule_scrollbar_fade();
            }
            if pointer != self.pointer {
                self.pointer = pointer;
                if self.hover.is_none() && self.mouse_inside() {
                    set_pointer(pointer);
                }
            }
            // The terminal is unlocked again: build and draw the frame.
            if let Some(selection) = frame.filter(|_| !refit) {
                if let Some(search) = &mut self.search {
                    search.update_count();
                }
                let link = self.hover.as_ref().map(|h| h.0);
                let block = self.selection.is_some_and(|s| s.block);
                let highlights = match &self.search {
                    Some(search) => Highlights { selection, block, matches: &search.matches, current: search.current, link },
                    None => Highlights { selection, block, link, ..Highlights::default() },
                };
                self.renderer.blink_off = BLINK.get().0;
                let frame = self.renderer.prepare(&self.snapshot, highlights);
                self.sync_backing();
                let presented = latency::frame(self.window, &self.snapshot);
                self.renderer.present(frame, presented);
                let screen: Id = msg!(self.window, c"screen");
                let fps: isize = if screen.is_null() { 0 } else { msg!(screen, c"maximumFramesPerSecond") };
                self.next_frame = Instant::now() + Duration::from_secs(1) / fps.clamp(30, 240) as u32;
                self.echo_frame = None;
            }
            objc_autoreleasePoolPop(pool);
            if refit {
                self.resize();
            }
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

    /// A bell rang: what `bell-features` asks (the title's mark is set
    /// as the frame is drawn).
    fn ring(&mut self) {
        let bell = BEHAVIOUR.get().bell;
        // SAFETY: main thread; documented signatures.
        unsafe {
            if bell.system {
                NSBeep();
            }
            let sound = BELL_SOUND.get();
            if bell.audio && !sound.is_null() {
                let _: Bool = msg!(sound, c"stop");
                let _: Bool = msg!(sound, c"play");
            }
            let ns_app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
            let active: Bool = msg!(ns_app, c"isActive");
            if bell.attention && active == NO {
                let _: isize = msg!(ns_app, c"requestUserAttention:", 10usize); // informational: one bounce
            }
            if bell.border {
                // A ring round the pane in the accent colour, fading as it thins.
                let container: Id = msg!(self.container, c"layer");
                if self.flash.is_null() {
                    self.flash = msg!(class(c"CALayer"), c"layer");
                    let _: Id = msg!(self.flash, c"retain");
                    call!(container, c"addSublayer:", self.flash);
                }
                call!(self.flash, c"setFrame:", send_rect(self.container, c"bounds"));
                let accent: Id = msg!(class(c"NSColor"), c"controlAccentColor");
                let accent: Id = msg!(accent, c"CGColor");
                call!(self.flash, c"setBorderColor:", accent);
                call!(self.flash, c"setBorderWidth:", 0.0f64);
                let fade: Id = msg!(class(c"CABasicAnimation"), c"animationWithKeyPath:", nsstring("borderWidth"));
                let number = |v: f64| -> Id { msg!(class(c"NSNumber"), c"numberWithDouble:", v) };
                call!(fade, c"setFromValue:", number(4.0));
                call!(fade, c"setToValue:", number(0.0));
                call!(fade, c"setDuration:", 0.35f64);
                call!(self.flash, c"addAnimation:forKey:", fade, nsstring("bell"));
            }
        }
    }

    /// The pane's been seen: its bell's mark goes.
    fn bell_seen(&mut self) {
        if mem::take(&mut self.bell) {
            self.title.clear();
            self.render();
        }
    }

    /// Sends typed input, drawing its echo early (see `ECHO_WAIT`).
    fn type_bytes(&mut self, bytes: &[u8]) {
        self.bell_seen();
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
        if BEHAVIOUR.get().password_mask {
            let before = self.password;
            self.password = if self.pane.hides_input() { keys::password_length(self.password, bytes) } else { 0 };
            // Nothing is echoed to draw it otherwise.
            if self.password != before {
                self.render();
            }
        }
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
                let block = mods.alt && granularity == Granularity::Cell;
                self.selection = Some(Selection { granularity, anchor: point, head: point, dragged: false, block });
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

    /// Opens the command palette with `entries`, or closes it.
    fn toggle_palette(&mut self, entries: Vec<palette::Entry>) {
        if self.palette.is_some() {
            return self.close_palette();
        }
        // SAFETY: main thread; documented signatures.
        unsafe {
            let palette = Palette::open(self.container, self.view, self.dark(), entries);
            call!(self.window, c"makeFirstResponder:", palette.field);
            self.palette = Some(palette);
        }
    }

    /// Shows the background drawn behind a translucent pane too, when a
    /// program changed it (OSC 11) or the config did.
    fn sync_backing(&mut self) {
        let backing = self.renderer.backing();
        if self.backing != Some(backing) {
            self.backing = Some(backing);
            let (rgb, opacity, space) = backing;
            // SAFETY: main thread; documented signature.
            unsafe { call!(msg!(self.container, c"layer"), c"setBackgroundColor:", color_alpha(rgb, opacity, space)) };
        }
    }

    fn close_palette(&mut self) {
        if self.palette.take().is_some() {
            // SAFETY: main thread; documented signature.
            unsafe { call!(self.window, c"makeFirstResponder:", self.view) };
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
            match self.selection {
                // The ends as dragged: a block's corners, not its text's start and end.
                Some(s) if s.block => term.text_block(s.anchor, s.head),
                _ => term.text_range(start, end),
            }
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

    /// The mouse on the scrollbar: dragging the knob, or clicking beside it.
    fn scrollbar_mouse(&mut self, drag: scrollbar::Drag) {
        let (history, rows) = {
            let term = self.pane.term.lock().unwrap();
            (term.scrollback().len(), term.rows())
        };
        let to = self.scrollbar.drag(drag, history, rows, self.scroll_offset);
        self.scrollbar.flash();
        schedule_scrollbar_fade();
        self.scroll_to(|_, _, _| to);
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

    /// Whether `event` is in the window's top padding, above the first row
    /// of a pane at the top.
    fn in_top_padding(&self, event: Id) -> bool {
        // SAFETY: main thread; `event` is a mouse NSEvent; documented signatures.
        let (y, height) = unsafe {
            let point: CGPoint = msg!(event, c"locationInWindow");
            (point.y, send_rect(msg!(self.window, c"contentView"), c"bounds").size.height)
        };
        let pad = self.renderer.padding_px().1 / self.renderer.scale();
        y >= height - pad
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
        if focused {
            self.bell = false; // seen
        }
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

    /// The search bar's or the palette's text changed (the view is the
    /// delegate of both).
    extern "C" fn search_changed(view: Id, _: Sel, notification: Id) {
        // SAFETY: an NSNotification about a text field.
        let field: Id = unsafe { msg!(notification, c"object") };
        with_surface(view, |w| match &mut w.palette {
            Some(palette) if palette.field == field => palette.update(),
            _ => w.search(None),
        });
    }

    /// Keys in the search bar: Return steps to older matches (with Shift,
    /// newer), Escape closes it.
    extern "C" fn search_command(view: Id, _: Sel, control: Id, _text_view: Id, command: Sel) -> Bool {
        let palette = with_surface(view, |w| w.palette.as_ref().is_some_and(|p| p.field == control)).unwrap_or(false);
        if palette {
            return palette_command(view, command);
        }
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

    /// Keys in the palette: the arrows move the selection, Return runs
    /// it, Escape closes.
    fn palette_command(view: Id, command: Sel) -> Bool {
        if command == sel(c"moveUp:") || command == sel(c"moveDown:") {
            let step = if command == sel(c"moveUp:") { -1 } else { 1 };
            with_surface(view, |w| w.palette.as_mut().map(|p| p.step(step)));
        } else if command == sel(c"insertNewline:") {
            let chosen = with_surface(view, |w| {
                let chosen = w.palette.as_ref().and_then(Palette::chosen);
                w.close_palette();
                chosen
            })
            .flatten();
            match chosen {
                // Once closed, so the action finds the pane as it was.
                Some(action) => perform(view, &action),
                // SAFETY: plain AppKit call.
                None => unsafe { NSBeep() },
            }
        } else if command == sel(c"cancelOperation:") {
            with_surface(view, Surface::close_palette);
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
        // Without a title bar, the top padding moves the window.
        if BEHAVIOUR.get().titlebar == Titlebar::Hidden && with_surface(view, |w| w.in_top_padding(event)) == Some(true) {
            // SAFETY: main thread; `event` is this mouse-down.
            unsafe { call!(msg!(view, c"window"), c"performWindowDragWithEvent:", event) };
            return;
        }
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
            let w = app.windows.iter_mut().find(|w| w.root == view && w.shown)?;
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
        with_app(|app| app.windows.iter_mut().find(|w| w.root == view && w.shown).map(|w| w.dragging = None));
    }
    /// Resize pointers over the dividers.
    extern "C" fn reset_cursor_rects(view: Id, _: Sel) {
        let dividers = with_app(|app| {
            let w = app.windows.iter().find(|w| w.root == view && w.shown).filter(|w| !w.zoomed)?;
            let bounds = app.pane_bounds(w.window, view);
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
    /// A window starts moving: if by the mouse, it's watched for a tab bar to join.
    extern "C" fn will_move(_: Id, _: Sel, notification: Id) {
        let window = window_of(notification);
        // SAFETY: main thread; documented class method.
        let buttons: usize = unsafe { msg!(class(c"NSEvent"), c"pressedMouseButtons") };
        let ours = with_app(|app| app.windows.iter().any(|w| w.window == window) && app.quick != Some(window)).unwrap_or(false);
        if buttons & 1 != 0 && ours && WINDOW_DRAG.replace(Some(window)).is_none() {
            run_on_main_after(WINDOW_DRAG_TICK, on_window_drag_tick);
        }
    }
    extern "C" fn did_change_active(_: Id, _: Sel, _: Id) {
        with_app(App::update_secure_input);
    }
    extern "C" fn will_terminate(_: Id, _: Sel, _: Id) {
        with_app(|app| {
            if mem::take(&mut app.secure_input_on) {
                // SAFETY: matches the call that turned it on.
                unsafe { DisableSecureEventInput() };
            }
            app.save_session();
            if let Some(socket) = app.mux.socket_path() {
                let _ = fs::remove_file(socket);
            }
        });
    }
    extern "C" fn did_resize(_: Id, _: Sel, notification: Id) {
        let window = window_of(notification);
        with_app(|app| {
            // Its tabs share another width now.
            for bar in app.bars.iter_mut().filter(|b| b.0 == window) {
                bar.2 = None;
            }
            app.update_tab_bar(window);
            app.layout(window, true);
        });
    }
    fn set_key(notification: Id, key: bool) {
        let window = window_of(notification);
        with_app(|app| {
            let active = app.shown(window)?.active;
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
        // Another app or window took the keys: it keeps them.
        let window = window_of(notification);
        with_app(|app| {
            if app.quick == Some(window) && app.config.quick_autohide {
                app.hide_quick(false);
            }
        });
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
            let alert = close_alert("Quit Mightty?", &running.replace("Closing", "Quitting"));
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
            (c"applicationDidBecomeActive:", action(did_change_active), c"v@:@"),
            (c"windowWillMove:", action(will_move), c"v@:@"),
            (c"applicationDidResignActive:", action(did_change_active), c"v@:@"),
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
        Action::PreviousTab if with_app(|app| app.select_own_tab(None, -1)) != Some(true) => send(c"selectPreviousTab:", NIL),
        Action::NextTab if with_app(|app| app.select_own_tab(None, 1)) != Some(true) => send(c"selectNextTab:", NIL),
        Action::PreviousTab | Action::NextTab => {}
        Action::JumpToAttention => drop(with_app(App::jump_to_attention)),
        Action::OpenConfig => drop(with_app(|app| app.open_config())),
        Action::OpenSettings => drop(with_app(App::open_settings)),
        Action::ReloadConfig => drop(with_app(App::reload_config)),
        Action::ToggleSecureInput => drop(with_app(App::toggle_secure_input)),
        Action::ToggleQuickTerminal => drop(with_app(App::toggle_quick_terminal)),
        Action::Undo => drop(with_app(|app| app.reopen_closed(true))),
        Action::MoveTabToNewWindow | Action::CloseOtherTabs => {
            // As from the tab bar's menu, for the key window's tab.
            let found = with_app(|app| {
                let (window, _) = app.key_window()?;
                let index = app.tabs_of(window).iter().position(|&t| app.windows[t].shown)?;
                let bar = app.bars.iter().find(|b| b.0 == window)?.1.bar;
                Some((bar, index))
            });
            if let Some((bar, index)) = found.flatten() {
                let click = if *action == Action::MoveTabToNewWindow { tabbar::Click::NewWindow(index) } else { tabbar::Click::CloseOthers(index) };
                on_tab_click(bar, click);
            }
        }
        Action::ReopenClosed => drop(with_app(|app| app.reopen_closed(false))),
        Action::PromptSurfaceTitle => {
            let current = |s: &mut Surface| {
                let program = || s.pane.term.lock().unwrap().title().trim_start_matches(['●', ' ']).to_owned();
                (s.window, s.title_override.clone().unwrap_or_else(program))
            };
            if let Some((window, current)) = with_surface(view, current) {
                ask_title(window, view, &current);
            }
        }
        Action::ToggleCommandPalette => drop(with_app(|app| {
            let entries = palette::entries(&app.config.keybinds);
            app.surfaces.iter_mut().find(|s| s.view == view).map(|s| s.toggle_palette(entries))
        })),
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
    if with_app(|app| app.select_own_tab(index, 0)) == Some(true) {
        return;
    }
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
        let app_menu = submenu(bar, "Mightty");
        add_action(app_menu, "Settings…", Action::OpenSettings);
        add_item(app_menu, "Open Config File", c"openConfig:", Action::OpenConfig);
        add_item(app_menu, "Reload Configuration", c"reloadConfig:", Action::ReloadConfig);
        separator(app_menu);
        add_action(app_menu, "Secure Keyboard Entry", Action::ToggleSecureInput);
        separator(app_menu);
        add_item(app_menu, "Quit Mightty", c"terminate:", Action::Quit);
        let shell_menu = submenu(bar, "Shell");
        add_item(shell_menu, "New Window", c"newWindow:", Action::NewWindow);
        add_item(shell_menu, "New Tab", c"newTab:", Action::NewTab);
        add_item(shell_menu, "Split Right", c"splitRight:", Action::NewSplit(Direction::Right));
        add_item(shell_menu, "Split Down", c"splitDown:", Action::NewSplit(Direction::Down));
        separator(shell_menu);
        add_action(shell_menu, "Change Title…", Action::PromptSurfaceTitle);
        add_item(shell_menu, "Close", c"closeSplit:", Action::CloseSurface);
        add_item(shell_menu, "Close Window", c"performClose:", Action::CloseWindow);
        add_action(shell_menu, "Reopen Closed", Action::ReopenClosed);
        let edit_menu = submenu(bar, "Edit");
        add_action(edit_menu, "Undo Close", Action::Undo);
        separator(edit_menu);
        add_item(edit_menu, "Copy", c"copy:", Action::Copy);
        add_item(edit_menu, "Paste", c"paste:", Action::Paste);
        add_action(edit_menu, "Clear Screen", Action::ClearScreen);
        separator(edit_menu);
        add_item(edit_menu, "Find…", c"performFind:", Action::StartSearch);
        add_item(edit_menu, "Find Next", c"findNext:", Action::NavigateSearch(1));
        add_item(edit_menu, "Find Previous", c"findPrevious:", Action::NavigateSearch(-1));
        let view_menu = submenu(bar, "View");
        add_action(view_menu, "Command Palette", Action::ToggleCommandPalette);
        separator(view_menu);
        add_action(view_menu, "Increase Font Size", Action::ChangeFontSize(1.0));
        add_action(view_menu, "Decrease Font Size", Action::ChangeFontSize(-1.0));
        add_action(view_menu, "Reset Font Size", Action::ResetFontSize);
        // AppKit adds the tab items (Show Tab Bar, Move Tab to New Window, …) here.
        let window_menu = submenu(bar, "Window");
        add_item(window_menu, "Minimize", c"performMiniaturize:", Action::MinimizeWindow);
        separator(window_menu);
        add_item(window_menu, "Jump to Pane Needing You", c"jumpToAttention:", Action::JumpToAttention);
        add_action(window_menu, "Quick Terminal", Action::ToggleQuickTerminal);
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
        add_action(window_menu, "Show Previous Tab", Action::PreviousTab);
        add_action(window_menu, "Show Next Tab", Action::NextTab);
        add_action(window_menu, "Move Tab to New Window", Action::MoveTabToNewWindow);
        add_action(window_menu, "Close Other Tabs", Action::CloseOtherTabs);
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
