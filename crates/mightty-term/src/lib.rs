//! Terminal state: screen grids, scrollback, cursor and modes, driven by the
//! [`mightty_vt`] parser. No rendering or I/O happens here; bytes the
//! terminal must send back to the application (replies to queries) are
//! collected and drained with [`Term::take_responses`].

mod base64;
mod cell;
mod colors;
mod graphemes;
mod graphics;
mod links;
mod styles;
mod width;
mod window;

use std::collections::VecDeque;
use std::{mem, slice};

use mightty_vt::{Params, Parser, Perform};

use graphemes::Graphemes;
use graphics::Graphics;
use links::Links;

pub use cell::{Cell, Color, Flags, Row, Style, StyleId, StyledCell, Underline};
pub use colors::{Colors, Rgb, default_palette};
pub use graphics::{ImageData, PLACEHOLDER, PlaceholderRun, Placement};
pub use styles::Styles;
/// Inclusive `(line, column)` start and end of some text, start first.
pub type TextRange = ((u64, usize), (u64, usize));

pub use width::char_width;
use width::{emoji_style, width_class};
pub use window::{PointerShape, Progress};

pub const DEFAULT_SCROLLBACK: usize = 10_000;
const MAX_MARKS: usize = 10_000;
/// Longer command lines (OSC 633;E) are cut, so the kept ones stay small.
const MAX_COMMAND_LEN: usize = 4096;
/// Titles (OSC 0/2) are cut to this many characters.
const MAX_TITLE_CHARS: usize = 1024;
/// Style tables smaller than this are never garbage-collected.
const MIN_STYLE_GC: usize = 4096;
/// Characters that end a word for double-click selection. `/ . : - _ ~ = @`
/// are deliberately absent so paths and URLs select whole.
const WORD_DELIMITERS: &str = "'\"`()[]{}<>|│,;";
const TAB_WIDTH: usize = 8;
/// Default colours, unless [`TermOptions`] says otherwise.
pub const DEFAULT_FG: (u8, u8, u8) = (0xd8, 0xd8, 0xd8);
pub const DEFAULT_BG: (u8, u8, u8) = (0x1e, 0x1e, 0x1e);

/// Settings a terminal takes from the user's configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermOptions {
    pub scrollback_lines: usize,
    /// The theme's colours: drawn, and reported to programs that ask
    /// (OSC 4, 10–12), unless they set their own.
    pub colors: Colors,
    /// Whether [mode 2027](Modes::grapheme_cluster) starts set.
    pub wide_emoji: bool,
}

impl Default for TermOptions {
    fn default() -> Self {
        Self {
            scrollback_lines: DEFAULT_SCROLLBACK,
            colors: Colors::default(),
            wide_emoji: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Off,
    /// 1000: press and release.
    Click,
    /// 1002: plus motion while a button is held.
    Drag,
    /// 1003: all motion.
    Motion,
}

#[derive(Clone, Debug)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub autowrap: bool,
    pub origin: bool,
    pub insert: bool,
    pub linefeed_newline: bool,
    pub show_cursor: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub sync_output: bool,
    /// Mode 2027: emoji written with VS16 (U+FE0F) take two cells, as
    /// Unicode's grapheme clusters say. On unless [`TermOptions::wide_emoji`]
    /// says otherwise, as in Ghostty; reset, a cluster takes its first
    /// character's width, as `wcwidth` counts.
    pub grapheme_cluster: bool,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    /// DECSCUSR value (0 = default).
    pub cursor_shape: u16,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            autowrap: true,
            origin: false,
            insert: false,
            linefeed_newline: false,
            show_cursor: true,
            bracketed_paste: false,
            focus_events: false,
            sync_output: false,
            grapheme_cluster: true,
            mouse: MouseMode::Off,
            mouse_sgr: false,
            cursor_shape: 0,
        }
    }
}

/// Shell-integration mark (OSC 133). These split output into
/// prompt / command / output blocks, the structure AI features build on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkKind {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandEnd { exit_code: Option<i32> },
}

/// A program's request for the clipboard (OSC 52); the GUI decides
/// whether to allow it (`clipboard-read`, `clipboard-write`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardRequest {
    /// Put this text on the clipboard.
    Write(String),
    /// Send the clipboard back; answer with [`clipboard_reply`].
    Read { bell_terminated: bool },
}

/// The OSC 52 reply carrying `text` for a `ClipboardRequest::Read`.
pub fn clipboard_reply(text: &str, bell_terminated: bool) -> Vec<u8> {
    let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
    format!("\x1b]52;c;{}{terminator}", base64::encode(text.as_bytes())).into_bytes()
}

/// Clipboard requests kept until taken; older ones are dropped.
const MAX_CLIPBOARD_REQUESTS: usize = 8;

/// A desktop notification a program asked for (OSC 9, OSC 777).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// Kitty keyboard protocol enhancement flags.
pub const KEY_DISAMBIGUATE: u8 = 1;
pub const KEY_EVENT_TYPES: u8 = 2;
pub const KEY_ALTERNATES: u8 = 4;
pub const KEY_ALL_AS_ESCAPES: u8 = 8;
pub const KEY_TEXT: u8 = 16;
const KEY_ALL: u8 = 31;
/// Pushed flags kept per screen; pushing more drops the oldest.
const MAX_KEY_FLAGS: usize = 8;

/// A command starting or finishing, from shell-integration marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandEvent {
    Started {
        command: Option<String>,
    },
    Finished {
        command: Option<String>,
        exit_code: Option<i32>,
    },
}

/// Notifications and command events kept until taken; older ones are dropped.
const MAX_NOTIFICATIONS: usize = 16;
/// Longest title and body kept, in characters.
const MAX_NOTIFICATION_TEXT: usize = 512;

/// One command run by the shell: prompt, command line, output, exit code.
/// Line numbers are absolute (see [`Term::line`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandBlock {
    pub prompt_line: Option<u64>,
    pub command: Option<String>,
    pub output_line: Option<u64>,
    /// Where the next prompt began; `None` while the command runs.
    pub end_line: Option<u64>,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub kind: MarkKind,
    /// Absolute line: scrollback line `k` is `lines_scrolled - scrollback.len() + k`,
    /// screen row `r` is `lines_scrolled + r`.
    pub line: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Charset {
    #[default]
    Ascii,
    DecGraphics,
}

#[derive(Clone, Copy, Debug, Default)]
struct Cursor {
    row: usize,
    col: usize,
    /// Colours and attributes applied to printed characters (SGR state).
    pen: Style,
    /// At the right margin with the next print wrapping first (DEC "last column flag").
    pending_wrap: bool,
}

#[derive(Clone, Copy, Debug)]
struct SavedCursor {
    cursor: Cursor,
    origin: bool,
    charsets: [Charset; 2],
    gl: usize,
}

pub struct Term {
    parser: Parser,
    cols: usize,
    rows: usize,
    /// Screen rows as rings, so a full-screen scroll is a pop and a push.
    primary: VecDeque<Row>,
    alternate: VecDeque<Row>,
    alt_active: bool,
    /// Kitty keyboard protocol flags, a stack per screen (primary,
    /// alternate); the last entry is in force and the first is never popped.
    key_flags: [Vec<u8>; 2],
    scrollback: VecDeque<Row>,
    scrollback_limit: usize,
    lines_scrolled: u64,
    cursor: Cursor,
    /// Saved cursor per screen, indexed by `alt_active`.
    saved: [Option<SavedCursor>; 2],
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    charsets: [Charset; 2],
    gl: usize,
    last_char: Option<char>,
    modes: Modes,
    title: String,
    cwd: Option<String>,
    /// The host OSC 7 named with `cwd` (empty when it named none).
    cwd_host: String,
    marks: Vec<Mark>,
    /// Command lines (OSC 633;E) keyed by the line their output starts on.
    commands: Vec<(u64, String)>,
    /// Command line received but not yet attached to an output-start mark.
    pending_command: Option<String>,
    /// When set, shell-integration marks count only if they carry
    /// `nonce=<this>`, so programs can't fake a finished command.
    mark_nonce: Option<Box<[u8]>>,
    /// The progress bar a program asked for (OSC 9 ; 4), and when.
    progress: Option<(Progress, std::time::Instant)>,
    pointer_shape: PointerShape,
    /// See `take_font_request`.
    font_request: Option<Option<String>>,
    /// The theme's colours, and those programs set over them.
    theme: Colors,
    set_colors: colors::SetColors,
    /// What RIS sets mode 2027 back to: [`TermOptions::wide_emoji`].
    wide_emoji: bool,
    /// Kitty graphics may read images from files and shared memory.
    image_files: bool,
    responses: Vec<u8>,
    bell: bool,
    /// Bells rung so far, for watchers that mustn't take `bell` from the UI.
    bells: u64,
    notifications: Vec<Notification>,
    command_events: Vec<CommandEvent>,
    clipboard: Vec<ClipboardRequest>,
    styles: Styles,
    /// Style ids of the pen and of blanks erased with it (BCE), interned on
    /// first use after SGR changes the pen.
    pen_id: Option<StyleId>,
    erased_id: Option<StyleId>,
    /// Style-table size that triggers the next garbage collection.
    gc_at: usize,
    graphemes: Graphemes,
    links: Links,
    /// The OSC 8 hyperlink being written, 0 for none. Not part of the pen:
    /// SGR resets leave it.
    link: u16,
    /// New links to refuse before collecting unused ones again, after a
    /// collection that freed nothing.
    link_gc_backoff: u32,
    /// The last character joined a cluster with U+200D (zero-width
    /// joiner), so the next one joins it too.
    joining: bool,
    /// Kitty graphics, per screen (primary, alternate).
    graphics: [Graphics; 2],
    /// A cell's size in pixels, as the window draws it: for sizing images
    /// and answering size queries.
    cell_pixels: (u32, u32),
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self::with_scrollback(cols, rows, DEFAULT_SCROLLBACK)
    }

    pub fn with_options(cols: usize, rows: usize, options: &TermOptions) -> Self {
        let mut term = Self::with_scrollback(cols, rows, options.scrollback_lines);
        term.theme = options.colors.clone();
        term.wide_emoji = options.wide_emoji;
        term.modes.grapheme_cluster = options.wide_emoji;
        term
    }

    /// Applies changed options to a running terminal: the theme's colours
    /// (those programs set stay), how much history it keeps (dropping the oldest lines
    /// past a lower limit), and how wide VS16 emoji are from now on.
    /// Whether kitty graphics may read images from files and shared memory
    /// (`t=f`, `t=t`, `t=s`), as on Unix it does unless told not to. Without,
    /// images come only in the output, and the terminal does no I/O.
    pub fn set_image_files(&mut self, allowed: bool) {
        self.image_files = allowed && cfg!(unix);
    }

    pub fn set_options(&mut self, options: &TermOptions) {
        self.theme = options.colors.clone();
        if options.wide_emoji != self.wide_emoji {
            self.wide_emoji = options.wide_emoji;
            self.modes.grapheme_cluster = options.wide_emoji;
        }
        self.scrollback_limit = options.scrollback_lines;
        while self.scrollback.len() > self.scrollback_limit {
            self.scrollback.pop_front();
        }
    }

    pub fn with_scrollback(cols: usize, rows: usize, scrollback_limit: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            parser: Parser::new(),
            cols,
            rows,
            primary: VecDeque::from(vec![Row::new(cols, Cell::default()); rows]),
            alternate: VecDeque::from(vec![Row::new(cols, Cell::default()); rows]),
            alt_active: false,
            key_flags: [vec![0], vec![0]],
            scrollback: VecDeque::new(),
            scrollback_limit,
            lines_scrolled: 0,
            cursor: Cursor::default(),
            saved: [None, None],
            top: 0,
            bottom: rows - 1,
            tabs: default_tabs(cols),
            charsets: [Charset::Ascii; 2],
            gl: 0,
            last_char: None,
            modes: Modes::default(),
            title: String::new(),
            cwd: None,
            cwd_host: String::new(),
            marks: Vec::new(),
            commands: Vec::new(),
            pending_command: None,
            mark_nonce: None,
            progress: None,
            pointer_shape: PointerShape::Default,
            font_request: None,
            theme: Colors::default(),
            set_colors: Box::new([None; colors::FOREGROUND + 3]),
            wide_emoji: true,
            image_files: cfg!(unix),
            responses: Vec::new(),
            bell: false,
            bells: 0,
            notifications: Vec::new(),
            command_events: Vec::new(),
            clipboard: Vec::new(),
            styles: Styles::new(),
            pen_id: None,
            erased_id: None,
            gc_at: MIN_STYLE_GC,
            graphemes: Graphemes::default(),
            links: Links::default(),
            link: 0,
            link_gc_backoff: 0,
            joining: false,
            graphics: Default::default(),
            cell_pixels: (10, 20),
        }
    }

    /// Feeds output from the application.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut parser = mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
        // Images in history that's gone go with it.
        let first = self.first_line();
        self.graphics[0].forget_above(first);
    }

    /// Images placed on the active screen (kitty graphics), in the order
    /// sent; `Placement::line` says where.
    /// Advances kitty graphics animations, on both screens, whose next
    /// frame is due by `now`. Returns when the next frame will be, to draw
    /// again then.
    pub fn animate(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        let [primary, alternate] = &mut self.graphics;
        match (primary.animate(now), alternate.animate(now)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn image_placements(&self) -> &[Placement] {
        self.graphics[usize::from(self.alt_active)].placements()
    }

    /// Sets the size of a cell in pixels. Returns whether it changed.
    pub fn set_cell_pixels(&mut self, width: u32, height: u32) -> bool {
        let size = (width.max(1), height.max(1));
        mem::replace(&mut self.cell_pixels, size) != size
    }

    /// Query replies waiting to be sent, without consuming them.
    pub fn pending_responses(&self) -> &[u8] {
        &self.responses
    }

    /// Bytes to write back to the application (query replies).
    pub fn take_responses(&mut self) -> Vec<u8> {
        mem::take(&mut self.responses)
    }

    /// Returns and clears the bell flag.
    pub fn take_bell(&mut self) -> bool {
        mem::take(&mut self.bell)
    }

    /// How many times the bell has rung.
    pub fn bells(&self) -> u64 {
        self.bells
    }

    /// Notifications asked for since the last call, oldest first.
    pub fn take_notifications(&mut self) -> Vec<Notification> {
        mem::take(&mut self.notifications)
    }

    /// Commands started and finished since the last call, oldest first.
    pub fn take_command_events(&mut self) -> Vec<CommandEvent> {
        mem::take(&mut self.command_events)
    }

    fn command_event(&mut self, event: CommandEvent) {
        if self.command_events.len() == MAX_NOTIFICATIONS {
            self.command_events.remove(0);
        }
        self.command_events.push(event);
    }

    /// The command line of the command running now, from shell integration.
    pub fn running_command(&self) -> Option<String> {
        // Cheap check first: this is polled.
        if self
            .marks
            .last()
            .is_none_or(|m| m.kind != MarkKind::OutputStart)
        {
            return None;
        }
        self.command_blocks().pop()?.command
    }

    /// Clipboard requests (OSC 52) since the last call, oldest first.
    pub fn take_clipboard_requests(&mut self) -> Vec<ClipboardRequest> {
        mem::take(&mut self.clipboard)
    }

    fn request_clipboard(&mut self, request: ClipboardRequest) {
        if self.clipboard.len() == MAX_CLIPBOARD_REQUESTS {
            self.clipboard.remove(0);
        }
        self.clipboard.push(request);
    }

    fn notify(&mut self, title: &[u8], body: &[u8]) {
        let text = |b: &[u8]| {
            String::from_utf8_lossy(b)
                .chars()
                .filter(|&c| !c.is_control() && !is_bidi_control(c))
                .take(MAX_NOTIFICATION_TEXT)
                .collect()
        };
        if self.notifications.len() == MAX_NOTIFICATIONS {
            self.notifications.remove(0);
        }
        self.notifications.push(Notification {
            title: text(title),
            body: text(body),
        });
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// `(row, col)`, zero-based.
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor.row, self.cursor.col)
    }

    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// The machine `cwd` is on, as reported: a shell over ssh reports the
    /// remote one, whose paths mean nothing here.
    pub fn cwd_host(&self) -> &str {
        &self.cwd_host
    }

    /// Accept shell-integration marks (OSC 133, 633;E) only when they carry
    /// `nonce=<nonce>`. For shells whose integration we started: the nonce
    /// is kept from their child processes, so output can't spoof marks.
    pub fn set_mark_nonce(&mut self, nonce: &str) {
        self.mark_nonce = Some(format!("nonce={nonce}").into_bytes().into());
    }

    fn mark_trusted(&self, params: &[&[u8]]) -> bool {
        self.mark_nonce
            .as_deref()
            .is_none_or(|nonce| params.contains(&nonce))
    }

    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    /// Commands the shell ran, oldest first, assembled from shell-integration marks.
    pub fn command_blocks(&self) -> Vec<CommandBlock> {
        let mut blocks = Vec::new();
        let mut current: Option<CommandBlock> = None;
        for mark in &self.marks {
            match mark.kind {
                MarkKind::PromptStart => {
                    blocks.extend(current.take());
                    current = Some(CommandBlock {
                        prompt_line: Some(mark.line),
                        ..Default::default()
                    });
                }
                MarkKind::CommandStart => {}
                MarkKind::OutputStart => {
                    if current.as_ref().is_some_and(|b| b.output_line.is_some()) {
                        blocks.extend(current.take());
                    }
                    let block = current.get_or_insert_default();
                    block.output_line = Some(mark.line);
                    block.command = self
                        .commands
                        .iter()
                        .find(|(line, _)| *line == mark.line)
                        .map(|(_, c)| c.clone());
                }
                MarkKind::CommandEnd { exit_code } => {
                    let mut block = current.take().unwrap_or_default();
                    block.end_line = Some(mark.line);
                    block.exit_code = exit_code;
                    blocks.push(block);
                }
            }
        }
        blocks.extend(current);
        blocks.retain(|b| b.output_line.is_some());
        blocks
    }

    /// Output of a command block; up to the cursor while it is still running.
    pub fn block_output(&self, block: &CommandBlock) -> String {
        let Some(start) = block.output_line else {
            return String::new();
        };
        let end = match block.end_line {
            Some(end) if end > start => end - 1,
            Some(_) => return String::new(),
            None => self.lines_scrolled + self.cursor.row as u64,
        };
        self.text_range((start, 0), (end, usize::MAX))
            .trim_end()
            .to_owned()
    }

    /// Every style cells currently refer to.
    pub fn styles(&self) -> &Styles {
        &self.styles
    }

    /// Colours and attributes of `cell`.
    #[inline]
    pub fn style(&self, cell: Cell) -> &Style {
        self.styles.get(cell.style())
    }

    /// `cell` with its style looked up. A cluster's `c` is its first
    /// character; [`Term::cell_text`] has all of it.
    #[inline]
    pub fn resolve(&self, cell: Cell) -> StyledCell {
        let style = self.style(cell);
        let c = if cell.flags().contains(Flags::GRAPHEME) {
            self.graphemes.base(cell)
        } else {
            cell.c
        };
        StyledCell {
            c,
            fg: style.fg,
            bg: style.bg,
            flags: style.attributes() | cell.flags(),
            link: style.link,
            underline: style.underline(),
            underline_color: style.underline_color,
        }
    }

    /// The target of OSC 8 hyperlink `id` ([`Style::link`]).
    pub fn link(&self, id: u16) -> Option<&str> {
        self.links.get(id)
    }

    /// The OSC 8 hyperlink at `(line, col)`, if any.
    pub fn link_at(&self, line: u64, col: usize) -> Option<u16> {
        let cells = &self.line(line)?.cells;
        let col = if col > 0 && flag_at(cells, col, Flags::WIDE_SPACER) {
            col - 1
        } else {
            col
        };
        let link = self.style(*cells.get(col)?).link;
        (link != 0).then_some(link)
    }

    /// A URL written as plain text around `(line, col)` (`https://…`,
    /// `file://…`, `mailto:…` and the like), across soft wraps: its range
    /// and text. Trailing punctuation and unbalanced closing brackets are
    /// left out, as in `(see https://example.com/a).`
    pub fn url_at(&self, line: u64, col: usize) -> Option<(TextRange, String)> {
        // The logical line around `line`: its characters and their cells.
        let mut first = line;
        while first > 0 && self.line(first - 1).is_some_and(|row| row.wrapped) {
            first -= 1;
        }
        let (mut text, mut at) = (Vec::new(), Vec::new());
        let mut cluster = String::new();
        let mut here = None;
        let mut l = first;
        while let Some(row) = self.line(l) {
            for (c, &cell) in row.cells.iter().enumerate() {
                if cell.flags().contains(Flags::WIDE_SPACER)
                    || cell.flags().contains(Flags::WRAP_SPACER)
                {
                    continue;
                }
                let last = if cell.flags().contains(Flags::WIDE) {
                    c + 1
                } else {
                    c
                };
                if l == line && (c..=last).contains(&col) {
                    here = Some(text.len());
                }
                cluster.clear();
                self.push_cell(&mut cluster, cell);
                for ch in cluster.chars() {
                    text.push(ch);
                    at.push(((l, c), (l, last)));
                }
            }
            if !row.wrapped || text.len() > 4 * links::MAX_URI {
                break;
            }
            l += 1;
        }
        let here = here?;
        let in_url = |c: char| !c.is_whitespace() && !c.is_control() && !"<>\"`{}|^".contains(c);
        if !in_url(text[here]) {
            return None;
        }
        let mut start = here;
        while start > 0 && in_url(text[start - 1]) {
            start -= 1;
        }
        let mut end = here + 1;
        while end < text.len() && in_url(text[end]) {
            end += 1;
        }
        // The last scheme starting at or before the cell, inside this run.
        let token: String = text[start..end].iter().collect();
        let offsets: Vec<usize> = token.char_indices().map(|(i, _)| i).collect();
        let scheme = URL_SCHEMES
            .iter()
            .flat_map(|s| token.match_indices(s).map(|(i, _)| i))
            .map(|i| start + offsets.partition_point(|&o| o < i))
            .filter(|&i| i <= here)
            .max()?;
        let mut url: Vec<char> = text[scheme..end].to_vec();
        // Trailing punctuation, and closing brackets without an opening one.
        while let Some(&last) = url.last() {
            let unbalanced = |open, close| {
                last == close
                    && url.iter().filter(|&&c| c == open).count()
                        < url.iter().filter(|&&c| c == close).count()
            };
            if ".,:;!?'\"".contains(last) || unbalanced('(', ')') || unbalanced('[', ']') {
                url.pop();
            } else {
                break;
            }
        }
        let text: String = url.iter().collect();
        let body = URL_SCHEMES
            .iter()
            .find(|s| text.starts_with(**s))
            .map_or(0, |s| s.len());
        if text.len() <= body || scheme + url.len() <= here {
            return None;
        }
        Some(((at[scheme].0, at[scheme + url.len() - 1].1), text))
    }

    /// The grapheme cluster in `cell`, when it holds more than one
    /// character ([`Flags::GRAPHEME`]).
    pub fn cell_text(&self, cell: Cell) -> Option<&str> {
        self.graphemes.get(cell)
    }

    fn rows_to_text<'a>(&self, rows: impl Iterator<Item = &'a Row>) -> String {
        let mut out = String::new();
        for row in rows {
            let start = out.len();
            let skip = |c: &&Cell| {
                c.flags().contains(Flags::WIDE_SPACER) || c.flags().contains(Flags::WRAP_SPACER)
            };
            for &cell in row.cells.iter().filter(|c| !skip(c)) {
                self.push_cell(&mut out, cell);
            }
            if !row.wrapped {
                let len = out[start..].trim_end_matches(' ').len();
                out.truncate(start + len);
                out.push('\n');
            }
        }
        let len = out.trim_end_matches(['\n', ' ']).len();
        out.truncate(len);
        out
    }

    fn push_cell(&self, out: &mut String, cell: Cell) {
        // Hidden text (SGR 8) isn't shown, so it isn't copied either: a
        // command that looks harmless mustn't paste with more in it.
        if self.style(cell).flags.contains(Flags::HIDDEN) {
            out.push(' ');
            return;
        }
        match self.graphemes.get(cell) {
            Some(text) => out.push_str(text),
            None => out.push(cell.c),
        }
    }

    pub fn is_alt_screen(&self) -> bool {
        self.alt_active
    }

    /// Kitty keyboard protocol enhancements the program asked for (see
    /// `KEY_*`); 0 means legacy encoding.
    pub fn key_flags(&self) -> u8 {
        *self.key_flags[usize::from(self.alt_active)]
            .last()
            .unwrap_or(&0)
    }

    fn key_flags_mut(&mut self) -> &mut Vec<u8> {
        &mut self.key_flags[usize::from(self.alt_active)]
    }

    /// Rows of the active screen.
    pub fn lines(&self) -> &VecDeque<Row> {
        if self.alt_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    pub fn scrollback(&self) -> &VecDeque<Row> {
        &self.scrollback
    }

    /// Lines that have scrolled off the top of the primary screen, ever.
    ///
    /// Together with [`Term::line`] this gives every row an absolute line
    /// number that stays put while output scrolls: screen row `r` is line
    /// `lines_scrolled() + r`, and scrollback starts at [`Term::first_line`].
    pub fn lines_scrolled(&self) -> u64 {
        self.lines_scrolled
    }

    /// Clears history and the rows above the cursor, moving the cursor's
    /// row (usually the prompt) to the top, as ⌘K does in Terminal.app and
    /// Ghostty. Not on the alternate screen, which belongs to its program.
    pub fn clear_above_cursor(&mut self) {
        if self.alt_active {
            return;
        }
        // Counted as scrolled off, so absolute line numbers stay right.
        for _ in 0..self.cursor.row {
            self.primary.pop_front();
            self.primary.push_back(Row::new(self.cols, Cell::default()));
            self.lines_scrolled += 1;
        }
        self.cursor.row = 0;
        self.scrollback.clear();
        let first = self.first_line();
        self.graphics[0].forget_above(first);
    }

    /// Absolute line number of the oldest row still in scrollback.
    pub fn first_line(&self) -> u64 {
        self.lines_scrolled - self.scrollback.len() as u64
    }

    /// The row at absolute line `line`, from scrollback or the active screen.
    /// Scrollback is not reachable while the alternate screen is active.
    pub fn line(&self, line: u64) -> Option<&Row> {
        if line >= self.lines_scrolled {
            return self.lines().get((line - self.lines_scrolled) as usize);
        }
        if self.alt_active || line < self.first_line() {
            return None;
        }
        self.scrollback.get((line - self.first_line()) as usize)
    }

    /// Text between two `(line, column)` points, inclusive, in either order.
    /// Soft-wrapped rows are joined; hard line ends become `\n`.
    pub fn text_range(&self, a: (u64, usize), b: (u64, usize)) -> String {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut out = String::new();
        for line in start.0..=end.0 {
            let Some(row) = self.line(line) else { continue };
            let from = if line == start.0 { start.1 } else { 0 };
            let to = if line == end.0 {
                end.1.saturating_add(1).min(row.cells.len())
            } else {
                row.cells.len()
            };
            let begin = out.len();
            for cell in row.cells.get(from..to).unwrap_or_default() {
                if !cell.flags().contains(Flags::WIDE_SPACER)
                    && !cell.flags().contains(Flags::WRAP_SPACER)
                {
                    self.push_cell(&mut out, *cell);
                }
            }
            let continues = row.wrapped && to == row.cells.len();
            if !continues || line == end.0 {
                let len = out[begin..].trim_end_matches(' ').len();
                out.truncate(begin + len);
                if line != end.0 {
                    out.push('\n');
                }
            }
        }
        out
    }

    /// The text in a block: columns `a.1..=b.1` (either way round) of lines
    /// `a.0..=b.0`, a line each, trailing spaces trimmed. A wide character
    /// counts when its first cell is inside.
    pub fn text_block(&self, a: (u64, usize), b: (u64, usize)) -> String {
        let (lines, cols) = (a.0.min(b.0)..=a.0.max(b.0), a.1.min(b.1)..=a.1.max(b.1));
        let mut out = String::new();
        for line in lines {
            if !out.is_empty() || line != a.0.min(b.0) {
                out.push('\n');
            }
            let Some(row) = self.line(line) else { continue };
            let begin = out.len();
            let end = (*cols.end()).saturating_add(1).min(row.cells.len());
            for cell in row.cells.get(*cols.start()..end).unwrap_or_default() {
                if !cell.flags().contains(Flags::WIDE_SPACER)
                    && !cell.flags().contains(Flags::WRAP_SPACER)
                {
                    self.push_cell(&mut out, *cell);
                }
            }
            let len = out[begin..].trim_end_matches(' ').len();
            out.truncate(begin + len);
        }
        out
    }

    /// Where `query` occurs in scrollback and on screen (only on screen
    /// while the alternate screen is active), oldest first, as inclusive
    /// `(line, column)` ranges like [`Term::text_range`] takes. A match may
    /// span soft-wrapped rows. Case is ignored unless `query` has a capital.
    pub fn search(&self, query: &str) -> Vec<TextRange> {
        self.search_from(query, 0).1
    }

    /// [`Term::search`] from `line` on, to update earlier results as output
    /// arrives: rows in scrollback don't change, so after searching once,
    /// search from the old `lines_scrolled()`. Starts at the beginning of
    /// the logical line holding `line` and returns where that is; matches
    /// starting before it are unaffected.
    pub fn search_from(&self, query: &str, line: u64) -> (u64, Vec<TextRange>) {
        let first = if self.alt_active {
            self.lines_scrolled
        } else {
            self.first_line()
        };
        let mut first = line.max(first);
        while first > 0 && self.line(first - 1).is_some_and(|row| row.wrapped) {
            first -= 1;
        }
        (first, self.search_lines(query, first))
    }

    fn search_lines(&self, query: &str, first: u64) -> Vec<TextRange> {
        let ignore_case = !query.chars().any(char::is_uppercase);
        let fold = |c: char| {
            if ignore_case {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                c
            }
        };
        let needle: Vec<char> = query.chars().map(fold).collect();
        let mut found = Vec::new();
        if needle.is_empty() {
            return found;
        }
        let end = self.lines_scrolled + self.rows as u64;
        // One logical line (soft-wrapped rows joined) at a time: its
        // characters, and the cell each came from.
        let (mut text, mut at) = (Vec::new(), Vec::new());
        let mut cluster = String::new();
        for line in first..end {
            let Some(row) = self.line(line) else { continue };
            for (col, &cell) in row.cells.iter().enumerate() {
                if cell.flags().contains(Flags::WIDE_SPACER)
                    || cell.flags().contains(Flags::WRAP_SPACER)
                {
                    continue;
                }
                cluster.clear();
                self.push_cell(&mut cluster, cell);
                for c in cluster.chars() {
                    text.push(fold(c));
                    at.push((
                        line,
                        col,
                        if cell.flags().contains(Flags::WIDE) {
                            col + 1
                        } else {
                            col
                        },
                    ));
                }
            }
            if row.wrapped && line + 1 < end {
                continue;
            }
            for start in 0..text.len().saturating_sub(needle.len() - 1) {
                if text[start..start + needle.len()] == needle[..] {
                    let (first, last) = (at[start], at[start + needle.len() - 1]);
                    found.push(((first.0, first.1), (last.0, last.2)));
                }
            }
            text.clear();
            at.clear();
        }
        found
    }

    /// Column range `(start, end)`, inclusive, of the word at `(line, col)`.
    /// Paths and URLs count as one word.
    pub fn word_at(&self, line: u64, col: usize) -> Option<(usize, usize)> {
        let cells = &self.line(line)?.cells;
        if col >= cells.len() {
            return Some((col, col)); // blank
        }
        // The right half of a wide character belongs to its left half.
        let char_at = |i: usize| {
            let i = if i > 0 && cells[i].flags().contains(Flags::WIDE_SPACER) {
                i - 1
            } else {
                i
            };
            self.graphemes.base(cells[i])
        };
        let is_word = |c: char| !c.is_whitespace() && !WORD_DELIMITERS.contains(c);
        if !is_word(char_at(col)) {
            return Some((col, col));
        }
        let mut start = col;
        while start > 0 && is_word(char_at(start - 1)) {
            start -= 1;
        }
        let mut end = col;
        while end + 1 < cells.len() && is_word(char_at(end + 1)) {
            end += 1;
        }
        Some((start, end))
    }

    /// Visible text, soft-wrapped rows joined, trailing blanks trimmed.
    pub fn screen_text(&self) -> String {
        self.rows_to_text(self.lines().iter())
    }

    /// Scrollback followed by the primary screen.
    pub fn history_text(&self) -> String {
        self.rows_to_text(self.scrollback.iter().chain(self.primary.iter()))
    }

    /// Resizes without reflow (TODO: rewrap soft-wrapped lines).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        for (is_alt, lines) in [(false, &mut self.primary), (true, &mut self.alternate)] {
            for row in lines.iter_mut() {
                row.cells.truncate(cols);
                if flag_at(&row.cells, cols - 1, Flags::WIDE) {
                    row.cells[cols - 1] = Cell::BLANK;
                }
            }
            if rows < lines.len() {
                let mut excess = lines.len() - rows;
                let active = is_alt == self.alt_active;
                // Drop blank rows below the cursor first, then keep the cursor
                // visible by pushing rows above it off the top.
                while active
                    && excess > 0
                    && lines.len() > self.cursor.row + 1
                    && lines.back().is_some_and(Row::is_blank)
                {
                    lines.pop_back();
                    excess -= 1;
                }
                let shift = if active {
                    excess.min(self.cursor.row)
                } else {
                    0
                };
                for row in lines.drain(..shift) {
                    if !is_alt {
                        self.scrollback.push_back(row);
                        self.lines_scrolled += 1;
                    }
                }
                lines.truncate(rows);
                if is_alt == self.alt_active {
                    self.cursor.row -= shift;
                }
            } else {
                lines.resize(rows, Row::new(cols, Cell::default()));
            }
        }
        while self.scrollback.len() > self.scrollback_limit {
            self.scrollback.pop_front();
        }
        self.cols = cols;
        self.rows = rows;
        self.tabs = default_tabs(cols);
        self.top = 0;
        self.bottom = rows - 1;
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn lines_mut(&mut self) -> &mut VecDeque<Row> {
        if self.alt_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    /// Blank cell carrying the current background (BCE).
    fn blank(&mut self) -> Cell {
        let id = match self.erased_id {
            Some(id) => id,
            None => *self
                .erased_id
                .insert(self.styles.intern(self.cursor.pen.erased())),
        };
        Cell::new(' ', id, Flags::empty())
    }

    /// Style id for printing with the current pen.
    #[inline]
    fn pen(&mut self) -> StyleId {
        match self.pen_id {
            Some(id) => id,
            None => *self.pen_id.insert(self.styles.intern(Style {
                link: self.link,
                ..self.cursor.pen
            })),
        }
    }

    /// The pen changed (SGR, cursor restore, reset). This is also the one
    /// place styles are collected: no style ids are held across it.
    fn pen_changed(&mut self) {
        self.pen_id = None;
        self.erased_id = None;
        if self.styles.len() >= self.gc_at {
            self.collect_styles();
        }
    }

    /// Drops styles no cell uses any more. The next run waits until the
    /// table has doubled, so scans amortise to O(1) per new style. The
    /// table is sized to reach that point without reallocating: with a
    /// fresh colour per cell it holds a style per live cell, and a doubling
    /// `Vec` (plus a copy while collecting) would need several times that.
    fn collect_styles(&mut self) {
        // Ids are renumbered: cached ones would point at other styles, or past the end.
        self.pen_id = None;
        self.erased_id = None;
        let Self {
            styles,
            primary,
            alternate,
            scrollback,
            ..
        } = self;
        let live = styles.live(
            primary
                .iter()
                .chain(alternate.iter())
                .chain(scrollback.iter()),
        );
        let next = (live.len() * 2).max(MIN_STYLE_GC);
        // Room past `next` for the few styles interned before the check runs.
        styles.compact(
            &live,
            primary
                .iter_mut()
                .chain(alternate.iter_mut())
                .chain(scrollback.iter_mut()),
            next + 64,
        );
        self.gc_at = next;
    }

    /// Before writing `[from, to)` on `row`: blanks the far halves of wide
    /// characters the write cuts in two.
    #[inline]
    fn erase_split_wide(&mut self, row: usize, from: usize, to: usize) {
        let cells = &self.lines()[row].cells;
        let left = from > 0 && flag_at(cells, from, Flags::WIDE_SPACER);
        let right = to < self.cols && flag_at(cells, to - 1, Flags::WIDE);
        if left || right {
            let blank = self.blank();
            let cells = &mut self.lines_mut()[row].cells;
            if left {
                cells[from - 1] = blank;
            }
            if right {
                cells[to] = blank;
            }
        }
    }

    fn respond(&mut self, bytes: &[u8]) {
        self.responses.extend_from_slice(bytes);
    }

    // ---- printing ----

    fn input_char(&mut self, c: char) {
        let c = if self.charsets[self.gl] == Charset::DecGraphics {
            dec_graphics(c)
        } else {
            c
        };
        let width = char_width(c);
        if (width == 0 || self.joining || may_join(c)) && self.join(c, width) {
            return;
        }
        if width == 0 {
            return; // nothing before it to join
        }
        self.joining = false;
        self.last_char = Some(c);

        if self.cursor.pending_wrap {
            self.wrap();
        }
        if width == 2 && self.cursor.col + 1 >= self.cols {
            if self.cols < 2 {
                return;
            }
            if self.modes.autowrap {
                let (row, last) = (self.cursor.row, self.cols - 1);
                self.fill(row, last, last + 1);
                let erased = self.blank().style();
                let cells = &mut self.lines_mut()[row].cells;
                grow(cells, last + 1);
                cells[last] = Cell::new(' ', erased, Flags::WRAP_SPACER);
                self.wrap();
            } else {
                self.cursor.col = self.cols - 2;
            }
        }
        if self.modes.insert {
            self.insert_blanks(width);
        }

        let (row, col) = (self.cursor.row, self.cursor.col);
        let pen = self.pen();
        self.erase_split_wide(row, col, col + width);
        let cells = &mut self.lines_mut()[row].cells;
        grow(cells, col + width);
        if width == 2 {
            cells[col] = Cell::new(c, pen, Flags::WIDE);
            cells[col + 1] = Cell::new(' ', pen, Flags::WIDE_SPACER);
        } else {
            cells[col] = Cell::new(c, pen, Flags::empty());
        }

        if col + width >= self.cols {
            self.cursor.col = self.cols - 1;
            self.cursor.pending_wrap = self.modes.autowrap;
        } else {
            self.cursor.col = col + width;
        }
    }

    /// Adds `c` to the character before the cursor when it continues its
    /// grapheme cluster: a combining mark or other zero-width character, an
    /// emoji modifier after an emoji, anything after a zero-width joiner,
    /// or a flag's second letter. Returns whether `c` was used up.
    ///
    /// The cluster keeps its first character's width, as in kitty and
    /// Ghostty, except that a flag takes the second letter's cell too, and
    /// so does a narrow character with an emoji style followed by VS16
    /// (in [mode 2027](Modes::grapheme_cluster)).
    fn join(&mut self, c: char, width: usize) -> bool {
        let (row, col) = (self.cursor.row, self.cursor.col);
        let Some(col) = (if self.cursor.pending_wrap {
            Some(col)
        } else {
            col.checked_sub(1)
        }) else {
            return false;
        };
        let cells = &self.lines()[row].cells;
        let col = if col > 0 && flag_at(cells, col, Flags::WIDE_SPACER) {
            col - 1
        } else {
            col
        };
        let Some(&cell) = cells.get(col) else {
            return false;
        };
        let mut text = String::new();
        self.push_cell(&mut text, cell);
        let wide = cell.flags().contains(Flags::WIDE);
        let single = text.chars().count() == 1;
        let flag = is_regional(c) && !wide && single && text.chars().all(is_regional);
        let emoji = c == '\u{FE0F}'
            && self.modes.grapheme_cluster
            && !wide
            && single
            && text.chars().all(emoji_style);
        let joins = width == 0
            || (self.joining && text.ends_with('\u{200D}'))
            || (wide && matches!(c, '\u{1F3FB}'..='\u{1F3FF}'))
            || flag;
        if !joins {
            return false;
        }
        if text.len() + c.len_utf8() > MAX_CLUSTER_BYTES {
            return true; // dropped: see MAX_CLUSTER_BYTES
        }
        text.push(c);
        self.joining = c == '\u{200D}';
        if self.graphemes.should_collect(&text) {
            let Self {
                graphemes,
                primary,
                alternate,
                scrollback,
                ..
            } = self;
            graphemes.collect(
                primary
                    .iter()
                    .chain(alternate.iter())
                    .chain(scrollback.iter()),
            );
        }
        // Out of ids even so: the character is dropped.
        let Some(id) = self.graphemes.intern(&text) else {
            return true;
        };
        let mut flags = cell.flags() | Flags::GRAPHEME;
        // The flag spreads over the cell its second letter would have
        // taken; the emoji over the next cell.
        let widen = (flag || emoji) && !self.cursor.pending_wrap && col + 1 < self.cols;
        if widen {
            self.erase_split_wide(row, col + 1, col + 2);
            flags.insert(Flags::WIDE);
        }
        let cells = &mut self.lines_mut()[row].cells;
        cells[col] = Cell::new(id, cell.style(), flags);
        if widen {
            grow(cells, col + 2);
            cells[col + 1] = Cell::new(' ', cell.style(), Flags::WIDE_SPACER);
            if col + 2 >= self.cols {
                self.cursor.col = self.cols - 1;
                self.cursor.pending_wrap = self.modes.autowrap;
            } else {
                self.cursor.col = col + 2;
            }
        }
        true
    }

    /// A run of printable text, written a row at a time rather than a
    /// character at a time. DEC graphics and insert mode take the slow path.
    fn input_str(&mut self, s: &str) {
        if self.charsets[self.gl] != Charset::Ascii || self.modes.insert {
            s.chars().for_each(|c| self.input_char(c));
        } else if s.is_ascii() {
            self.input_ascii(s.as_bytes());
        } else {
            s.chars().for_each(|c| self.input_char(c));
        }
    }

    fn input_chars(&mut self, chars: &[char]) {
        if self.charsets[self.gl] != Charset::Ascii || self.modes.insert {
            chars.iter().for_each(|&c| self.input_char(c));
        } else {
            self.input_text(chars);
        }
    }

    /// Like `input_ascii` for text of any width: fills as much of the
    /// current row as fits per pass, with one row borrow per pass.
    fn input_text(&mut self, chars: &[char]) {
        let cols = self.cols;
        let pen = self.pen();
        let mut i = 0;
        while let Some(&first) = chars.get(i) {
            if char_width(first) == 0 || self.joining || may_join(first) {
                // Possibly part of the cluster before it: the careful path decides.
                i += 1;
                self.input_char(first);
                continue;
            }
            if self.cursor.pending_wrap {
                self.wrap();
            }
            if char_width(first) == 2 && self.cursor.col + 1 >= cols {
                // A wide char that doesn't fit: the careful path wraps it.
                i += 1;
                self.input_char(first);
                continue;
            }
            let (row, start) = (self.cursor.row, self.cursor.col);
            // The right edge is only known after writing; see below.
            self.erase_split_wide(row, start, cols);
            let lines = if self.alt_active {
                &mut self.alternate
            } else {
                &mut self.primary
            };
            let cells = &mut lines[row].cells;
            // Room for the most this pass can write (every char wide, plus
            // one: see below), so the loop only stores; the unused (blank)
            // tail is trimmed after.
            let old_len = cells.len();
            let end = cols.min(start + 2 * (chars.len() - i));
            grow(cells, end + 1);
            let out = &mut cells[..end + 1];
            let mut col = start;
            let mut last = None;
            // Wide and narrow characters alternate unpredictably in CJK and
            // emoji text, so rather than branch on width every character
            // writes itself and a spacer; a narrow one's spacer is provisional
            // (the next character overwrites it) and the last is undone.
            let mut under = Cell::BLANK;
            let mut provisional = false;
            while let Some(&c) = chars.get(i) {
                // 1 or 2 here; zero-width characters and ones that may join
                // a cluster end the pass, and the next one starts with them.
                let width = width_class(c);
                if (col + width > end) | (width.wrapping_sub(1) >= 2) {
                    break;
                }
                i += 1;
                let wide = width == 2;
                under = out[col + 1];
                out[col] = Cell::new(c, pen, if wide { Flags::WIDE } else { Flags::empty() });
                out[col + 1] = Cell::new(' ', pen, Flags::WIDE_SPACER);
                col += width;
                provisional = !wide;
                last = Some(c);
                if col == cols {
                    break;
                }
            }
            if provisional {
                out[col] = under;
            }
            cells.truncate(old_len.max(col));
            // Ending on the right half of a wide character orphans it.
            if col < cols
                && col > 0
                && flag_at(cells, col, Flags::WIDE_SPACER)
                && !flag_at(cells, col - 1, Flags::WIDE)
            {
                let blank = self.blank();
                self.lines_mut()[row].cells[col] = blank;
            }
            if last.is_some() {
                self.last_char = last;
                self.joining = false;
            }
            if col >= cols {
                self.cursor.col = cols - 1;
                self.cursor.pending_wrap = self.modes.autowrap;
            } else {
                self.cursor.col = col;
            }
        }
    }

    /// Printable ASCII (0x20..=0x7E), with US-ASCII mapped into GL and insert mode off.
    fn input_ascii(&mut self, mut bytes: &[u8]) {
        let cols = self.cols;
        let pen = self.pen();
        self.joining = false;
        while let [.., last] = bytes {
            self.last_char = Some(*last as char);
            if self.cursor.pending_wrap {
                self.wrap();
            }
            let (row, col) = (self.cursor.row, self.cursor.col);
            let n = bytes.len().min(cols - col);
            self.erase_split_wide(row, col, col + n);
            let lines = if self.alt_active {
                &mut self.alternate
            } else {
                &mut self.primary
            };
            let cells = &mut lines[row].cells;
            // Overwrite what the row already has, append the rest: rows are
            // usually short (see `Row::cells`), so this is mostly appending.
            let existing = cells.len().saturating_sub(col).min(n);
            grow(cells, col);
            for (cell, &b) in cells[col..col + existing].iter_mut().zip(bytes) {
                *cell = Cell::new(b as char, pen, Flags::empty());
            }
            cells.extend(
                bytes[existing..n]
                    .iter()
                    .map(|&b| Cell::new(b as char, pen, Flags::empty())),
            );
            bytes = &bytes[n..];
            if col + n >= cols {
                self.cursor.col = cols - 1;
                self.cursor.pending_wrap = self.modes.autowrap;
            } else {
                self.cursor.col = col + n;
            }
        }
    }

    fn wrap(&mut self) {
        let row = self.cursor.row;
        self.lines_mut()[row].wrapped = true;
        self.cursor.col = 0;
        self.index();
    }

    /// Blanks `[from, to)` on `row`, also erasing wide characters cut in half.
    fn fill(&mut self, row: usize, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let blank = self.blank();
        let cols = self.cols;
        let cells = &mut self.lines_mut()[row].cells;
        if from > 0 && flag_at(cells, from - 1, Flags::WIDE) {
            cells[from - 1] = blank;
        }
        if to < cols && flag_at(cells, to, Flags::WIDE_SPACER) {
            cells[to] = blank;
        }
        if blank == Cell::BLANK && to >= cells.len() {
            cells.truncate(from); // erasing to the end: just shorten the row
            return;
        }
        grow(cells, to);
        fill_cells(&mut cells[from..to], blank);
    }

    fn clear_row(&mut self, row: usize) {
        let blank = self.blank();
        let cols = self.cols;
        clear_rows(slice::from_mut(&mut self.lines_mut()[row]), blank, cols);
    }

    // ---- cursor movement ----

    /// Absolute position; `row` is relative to the scroll region in origin mode.
    fn goto(&mut self, row: usize, col: usize) {
        let (base, max) = if self.modes.origin {
            (self.top, self.bottom)
        } else {
            (0, self.rows - 1)
        };
        self.cursor.row = (base + row).min(max);
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn move_up(&mut self, n: usize) {
        let min = if self.cursor.row >= self.top {
            self.top
        } else {
            0
        };
        self.cursor.row = self.cursor.row.saturating_sub(n).max(min);
        self.cursor.pending_wrap = false;
    }

    fn move_down(&mut self, n: usize) {
        let max = if self.cursor.row <= self.bottom {
            self.bottom
        } else {
            self.rows - 1
        };
        self.cursor.row = (self.cursor.row + n).min(max);
        self.cursor.pending_wrap = false;
    }

    fn move_forward(&mut self, n: usize) {
        self.cursor.col = (self.cursor.col + n).min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn move_back(&mut self, n: usize) {
        self.cursor.col = self.cursor.col.saturating_sub(n);
        self.cursor.pending_wrap = false;
    }

    fn tab_forward(&mut self, n: usize) {
        for _ in 0..n {
            if self.cursor.col + 1 >= self.cols {
                break;
            }
            let mut col = self.cursor.col + 1;
            while col < self.cols && !self.tabs[col] {
                col += 1;
            }
            self.cursor.col = col.min(self.cols - 1);
        }
        self.cursor.pending_wrap = false;
    }

    fn tab_back(&mut self, n: usize) {
        for _ in 0..n {
            if self.cursor.col == 0 {
                break;
            }
            let mut col = self.cursor.col - 1;
            while col > 0 && !self.tabs[col] {
                col -= 1;
            }
            self.cursor.col = col;
        }
        self.cursor.pending_wrap = false;
    }

    // ---- scrolling ----

    fn index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    /// Scrolls the region up; rows leaving the top of the full primary screen go to scrollback.
    fn scroll_up(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        if n == 0 {
            return;
        }
        let blank = self.blank();
        let cols = self.cols;
        let full_screen = top == 0 && bottom == self.rows - 1;
        let to_history = !self.alt_active && top == 0;
        let keep = to_history && self.scrollback_limit > 0;
        let lines = if self.alt_active {
            &mut self.alternate
        } else {
            &mut self.primary
        };
        for _ in 0..n {
            let leaving = if full_screen {
                lines.pop_front()
            } else {
                lines.remove(top)
            };
            let Some(leaving) = leaving else { return };
            let mut fresh = if keep {
                // Once scrollback is full, reuse the evicted row's allocation.
                let recycled = if self.scrollback.len() >= self.scrollback_limit {
                    self.scrollback.pop_front()
                } else {
                    None
                };
                self.scrollback.push_back(leaving);
                recycled.unwrap_or_else(|| Row {
                    cells: Vec::with_capacity(cols),
                    wrapped: false,
                })
            } else {
                leaving
            };
            clear_rows(slice::from_mut(&mut fresh), blank, cols);
            if full_screen {
                lines.push_back(fresh);
            } else {
                lines.insert(bottom, fresh);
            }
        }
        if to_history {
            while self.scrollback.len() > self.scrollback_limit {
                self.scrollback.pop_front();
            }
            self.lines_scrolled += n as u64;
            if !full_screen {
                // The rows under the region stay put on screen, so their
                // absolute lines go up with the count.
                let below = self.lines_scrolled - n as u64 + bottom as u64 + 1;
                for mark in self.marks.iter_mut().filter(|m| m.line >= below) {
                    mark.line += n as u64;
                }
                self.graphics_mut().renumber_from(below, n as u64);
            }
        } else {
            let scrolled = self.lines_scrolled;
            self.graphics_mut()
                .scroll(scrolled, top, bottom, -(n as i64));
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        let blank = self.blank();
        let cols = self.cols;
        let lines = self.lines_mut().make_contiguous();
        lines[top..=bottom].rotate_right(n);
        clear_rows(&mut lines[top..top + n], blank, cols);
        let scrolled = self.lines_scrolled;
        self.graphics_mut().scroll(scrolled, top, bottom, n as i64);
    }

    fn insert_lines(&mut self, n: usize) {
        let row = self.cursor.row;
        if row < self.top || row > self.bottom {
            return;
        }
        let bottom = self.bottom;
        let n = n.min(bottom - row + 1);
        let blank = self.blank();
        let cols = self.cols;
        let lines = self.lines_mut().make_contiguous();
        lines[row..=bottom].rotate_right(n);
        clear_rows(&mut lines[row..row + n], blank, cols);
        let scrolled = self.lines_scrolled;
        self.graphics_mut().scroll(scrolled, row, bottom, n as i64);
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn delete_lines(&mut self, n: usize) {
        let row = self.cursor.row;
        if row < self.top || row > self.bottom {
            return;
        }
        let bottom = self.bottom;
        let n = n.min(bottom - row + 1);
        let blank = self.blank();
        let cols = self.cols;
        let lines = self.lines_mut().make_contiguous();
        lines[row..=bottom].rotate_left(n);
        clear_rows(&mut lines[bottom + 1 - n..=bottom], blank, cols);
        let scrolled = self.lines_scrolled;
        self.graphics_mut()
            .scroll(scrolled, row, bottom, -(n as i64));
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        let top = top.max(1) - 1;
        let bottom = bottom.min(self.rows).max(1) - 1;
        if top < bottom {
            self.top = top;
            self.bottom = bottom;
            self.goto(0, 0);
        }
    }

    // ---- editing ----

    fn insert_blanks(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let n = n.min(cols - col);
        grow(&mut self.lines_mut()[row].cells, cols);
        self.lines_mut()[row].cells[col..].rotate_right(n);
        self.fill(row, col, col + n);
        if flag_at(&self.lines()[row].cells, cols - 1, Flags::WIDE) {
            self.fill(row, cols - 1, cols);
        }
        self.cursor.pending_wrap = false;
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let n = n.min(cols - col);
        grow(&mut self.lines_mut()[row].cells, cols);
        if self.lines()[row].cells[col]
            .flags()
            .contains(Flags::WIDE_SPACER)
        {
            self.fill(row, col - 1, col);
        }
        self.lines_mut()[row].cells[col..].rotate_left(n);
        self.fill(row, cols - n, cols);
        if flag_at(&self.lines()[row].cells, col, Flags::WIDE_SPACER) {
            self.fill(row, col, col + 1);
        }
        self.cursor.pending_wrap = false;
    }

    fn erase_display(&mut self, mode: u16) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        match mode {
            0 => {
                self.fill(row, col, self.cols);
                for r in row + 1..self.rows {
                    self.clear_row(r);
                }
            }
            1 => {
                for r in 0..row {
                    self.clear_row(r);
                }
                self.fill(row, 0, col + 1);
            }
            2 => {
                for r in 0..self.rows {
                    self.clear_row(r);
                }
                // As in kitty, clearing the screen clears its images.
                let (from, rows) = (self.lines_scrolled, self.rows as u64);
                self.graphics_mut().clear_lines(from, from + rows);
            }
            3 => self.scrollback.clear(),
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        match mode {
            0 => self.fill(row, col, self.cols),
            1 => self.fill(row, 0, col + 1),
            2 => self.fill(row, 0, self.cols),
            _ => {}
        }
    }

    // ---- state ----

    fn save_cursor(&mut self) {
        self.saved[self.alt_active as usize] = Some(SavedCursor {
            cursor: self.cursor,
            origin: self.modes.origin,
            charsets: self.charsets,
            gl: self.gl,
        });
    }

    fn restore_cursor(&mut self) {
        self.pen_changed();
        match self.saved[self.alt_active as usize] {
            Some(saved) => {
                self.cursor = saved.cursor;
                self.cursor.row = self.cursor.row.min(self.rows - 1);
                self.cursor.col = self.cursor.col.min(self.cols - 1);
                self.modes.origin = saved.origin;
                self.charsets = saved.charsets;
                self.gl = saved.gl;
            }
            None => {
                self.cursor = Cursor::default();
                self.modes.origin = false;
                self.charsets = [Charset::Ascii; 2];
                self.gl = 0;
            }
        }
    }

    fn set_alt_screen(&mut self, on: bool) {
        if on == self.alt_active {
            return;
        }
        self.alt_active = on;
        if on {
            for r in 0..self.rows {
                self.clear_row(r);
            }
            self.graphics[1].clear();
        }
    }

    /// RIS: everything back to defaults except size and scrollback.
    fn reset(&mut self) {
        let scrollback = mem::take(&mut self.scrollback);
        let styles = mem::replace(&mut self.styles, Styles::new());
        let graphemes = mem::take(&mut self.graphemes);
        let links = mem::take(&mut self.links);
        let (lines_scrolled, gc_at) = (self.lines_scrolled, self.gc_at);
        // What the embedder set, which output mustn't undo.
        let (mark_nonce, theme, cell_pixels, wide_emoji, image_files) = (
            self.mark_nonce.take(),
            mem::take(&mut self.theme),
            self.cell_pixels,
            self.wide_emoji,
            self.image_files,
        );
        *self = Self::with_scrollback(self.cols, self.rows, self.scrollback_limit);
        (
            self.mark_nonce,
            self.theme,
            self.cell_pixels,
            self.wide_emoji,
            self.image_files,
        ) = (mark_nonce, theme, cell_pixels, wide_emoji, image_files);
        self.modes.grapheme_cluster = wide_emoji;
        // The configured font again, if a program had changed it (OSC 50).
        self.font_request = Some(None);
        // Scrollback keeps its style and cluster ids, so it keeps their tables too.
        self.scrollback = scrollback;
        self.styles = styles;
        self.graphemes = graphemes;
        self.links = links;
        self.lines_scrolled = lines_scrolled;
        self.gc_at = gc_at;
    }

    fn set_ansi_modes(&mut self, params: &Params, on: bool) {
        for group in params.iter() {
            match group[0] {
                4 => self.modes.insert = on,
                20 => self.modes.linefeed_newline = on,
                _ => {}
            }
        }
    }

    fn ansi_mode(&self, mode: u16) -> Option<bool> {
        match mode {
            4 => Some(self.modes.insert),
            20 => Some(self.modes.linefeed_newline),
            _ => None,
        }
    }

    /// The modes [`set_dec_modes`](Self::set_dec_modes) changes, and DECNKM.
    fn dec_mode(&self, mode: u16) -> Option<bool> {
        let m = &self.modes;
        Some(match mode {
            1 => m.app_cursor,
            6 => m.origin,
            7 => m.autowrap,
            25 => m.show_cursor,
            47 | 1047 | 1049 => self.alt_active,
            66 => m.app_keypad,
            1000 => m.mouse == MouseMode::Click,
            1002 => m.mouse == MouseMode::Drag,
            1003 => m.mouse == MouseMode::Motion,
            1004 => m.focus_events,
            1006 => m.mouse_sgr,
            2004 => m.bracketed_paste,
            2026 => m.sync_output,
            2027 => m.grapheme_cluster,
            _ => return None,
        })
    }

    fn set_dec_modes(&mut self, params: &Params, on: bool) {
        for group in params.iter() {
            match group[0] {
                1 => self.modes.app_cursor = on,
                6 => {
                    self.modes.origin = on;
                    self.goto(0, 0);
                }
                7 => self.modes.autowrap = on,
                25 => self.modes.show_cursor = on,
                47 | 1047 => self.set_alt_screen(on),
                1048 => {
                    if on {
                        self.save_cursor()
                    } else {
                        self.restore_cursor()
                    }
                }
                1049 => {
                    if on {
                        self.save_cursor();
                        self.set_alt_screen(true);
                    } else {
                        self.set_alt_screen(false);
                        self.restore_cursor();
                    }
                }
                1000 => self.modes.mouse = if on { MouseMode::Click } else { MouseMode::Off },
                1002 => self.modes.mouse = if on { MouseMode::Drag } else { MouseMode::Off },
                1003 => {
                    self.modes.mouse = if on {
                        MouseMode::Motion
                    } else {
                        MouseMode::Off
                    }
                }
                1004 => self.modes.focus_events = on,
                1006 => self.modes.mouse_sgr = on,
                2004 => self.modes.bracketed_paste = on,
                2026 => self.modes.sync_output = on,
                2027 => self.modes.grapheme_cluster = on,
                _ => {}
            }
        }
    }

    fn sgr(&mut self, params: &Params) {
        self.pen_changed();
        let attrs = &mut self.cursor.pen;
        if params.is_empty() {
            *attrs = Style::default();
            return;
        }
        let mut i = 0;
        while let Some(group) = params.get(i) {
            match group[0] {
                0 => *attrs = Style::default(),
                1 => attrs.flags.insert(Flags::BOLD),
                2 => attrs.flags.insert(Flags::DIM),
                3 => attrs.flags.insert(Flags::ITALIC),
                4 => {
                    let underline = match group.get(1) {
                        None | Some(1) => Some(Underline::Single),
                        Some(0) => None,
                        Some(2) => Some(Underline::Double),
                        Some(3) => Some(Underline::Curly),
                        Some(4) => Some(Underline::Dotted),
                        Some(5) => Some(Underline::Dashed),
                        // Unknown: as it was.
                        Some(_) => Some(attrs.underline())
                            .filter(|_| attrs.flags.contains(Flags::UNDERLINE)),
                    };
                    attrs.set_underline(underline);
                }
                21 => attrs.set_underline(Some(Underline::Double)),
                5 | 6 => attrs.flags.insert(Flags::BLINK),
                7 => attrs.flags.insert(Flags::INVERSE),
                8 => attrs.flags.insert(Flags::HIDDEN),
                9 => attrs.flags.insert(Flags::STRIKE),
                22 => attrs.flags.remove(Flags::BOLD | Flags::DIM),
                23 => attrs.flags.remove(Flags::ITALIC),
                24 => attrs.set_underline(None),
                59 => attrs.underline_color = Color::Default,
                25 => attrs.flags.remove(Flags::BLINK),
                27 => attrs.flags.remove(Flags::INVERSE),
                28 => attrs.flags.remove(Flags::HIDDEN),
                29 => attrs.flags.remove(Flags::STRIKE),
                n @ 30..=37 => attrs.fg = Color::Indexed((n - 30) as u8),
                n @ 40..=47 => attrs.bg = Color::Indexed((n - 40) as u8),
                n @ 90..=97 => attrs.fg = Color::Indexed((n - 90 + 8) as u8),
                n @ 100..=107 => attrs.bg = Color::Indexed((n - 100 + 8) as u8),
                39 => attrs.fg = Color::Default,
                49 => attrs.bg = Color::Default,
                n @ (38 | 48 | 58) => {
                    let (color, consumed) = extended_color(params, i);
                    i += consumed;
                    match (n, color) {
                        (38, Some(c)) => attrs.fg = c,
                        (48, Some(c)) => attrs.bg = c,
                        (58, Some(c)) => attrs.underline_color = c,
                        _ => {}
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn osc(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        match params {
            // OSC 8 ; params ; URI: following text links to URI (none if empty).
            [b"8", options, uri @ ..] => {
                let uri = uri.join(&b';');
                let id = options
                    .split(|&b| b == b':')
                    .find_map(|kv| kv.strip_prefix(b"id="))
                    .unwrap_or_default();
                let link = match (std::str::from_utf8(&uri), std::str::from_utf8(id)) {
                    (Ok(uri), Ok(id)) if !uri.is_empty() => {
                        if self.links.is_full() && self.link_gc_backoff == 0 {
                            self.collect_styles();
                            let used = self.styles.links();
                            self.links.collect(|n| used[usize::from(n)]);
                            // Still full: every id is in use. Collecting is a
                            // full scan, so wait a while before trying again.
                            if self.links.is_full() {
                                self.link_gc_backoff = LINK_GC_BACKOFF;
                            }
                        }
                        self.link_gc_backoff = self.link_gc_backoff.saturating_sub(1);
                        self.links.intern(id, uri).unwrap_or(0)
                    }
                    _ => 0,
                };
                if link != self.link {
                    self.link = link;
                    self.pen_changed();
                }
            }
            [b"9", b"4", rest @ ..] => self.set_progress(rest),
            // ConEmu's other OSC 9 ; n ; …, not notifications.
            [b"9", n, ..] if (1..=2).contains(&n.len()) && n.iter().all(u8::is_ascii_digit) => {}
            // iTerm2's notification: OSC 9 ; text.
            [b"9", text @ ..] => self.notify(b"", &text.join(&b';')),
            [b"777", b"notify", title, body @ ..] => self.notify(title, &body.join(&b';')),
            // OSC 52 ; targets ; base64 text, or `?` to read. Every target
            // (clipboard, primary, selection) is the one macOS clipboard.
            [b"52", _targets, b"?"] => {
                self.request_clipboard(ClipboardRequest::Read { bell_terminated })
            }
            [b"52", _targets, data] => {
                if let Some(text) =
                    base64::decode(data).and_then(|bytes| String::from_utf8(bytes).ok())
                {
                    self.request_clipboard(ClipboardRequest::Write(text));
                }
            }
            [b"0" | b"2", rest @ ..] => {
                self.title =
                    displayable(&String::from_utf8_lossy(&rest.join(&b';')), MAX_TITLE_CHARS);
            }
            // The directory decides where new tabs start, so like marks it
            // must carry the nonce: output can't choose it.
            [b"7", url, rest @ ..] if self.mark_trusted(rest) => match parse_file_url(url) {
                Some((host, path)) => (self.cwd_host, self.cwd) = (host, Some(path)),
                None => (self.cwd_host, self.cwd) = (String::new(), None),
            },
            [b"4", pairs @ ..] => self.osc_palette(pairs, terminator),
            [b"22", name] => self.set_pointer_shape(name),
            [b"50", name] => self.set_font(name),
            [b"104", indices @ ..] => self.osc_reset_palette(indices),
            [code @ (b"10" | b"11" | b"12"), specs @ ..] => {
                self.osc_dynamic(usize::from(code[1] - b'0') + 10, specs, terminator)
            }
            [code @ (b"110" | b"111" | b"112"), ..] => {
                self.osc_reset_dynamic(usize::from(code[2] - b'0') + 110)
            }
            [b"133", kind, rest @ ..] if self.mark_trusted(rest) => {
                let kind = match kind.first() {
                    Some(b'A') => MarkKind::PromptStart,
                    Some(b'B') => MarkKind::CommandStart,
                    Some(b'C') => MarkKind::OutputStart,
                    Some(b'D') => MarkKind::CommandEnd {
                        exit_code: rest
                            .first()
                            .and_then(|s| std::str::from_utf8(s).ok()?.parse().ok()),
                    },
                    _ => return,
                };
                if self.marks.len() >= MAX_MARKS {
                    let oldest_kept = self.marks[MAX_MARKS / 2].line;
                    self.marks.drain(..MAX_MARKS / 2);
                    self.commands.retain(|(line, _)| *line >= oldest_kept);
                }
                let line = self.lines_scrolled + self.cursor.row as u64;
                if let MarkKind::CommandEnd { exit_code } = kind
                    && let Some(&Mark {
                        kind: MarkKind::OutputStart,
                        line: started,
                    }) = self.marks.last()
                {
                    let command = self
                        .commands
                        .iter()
                        .rfind(|(l, _)| *l == started)
                        .map(|(_, c)| c.clone());
                    self.command_event(CommandEvent::Finished { command, exit_code });
                }
                self.marks.push(Mark { kind, line });
                if kind == MarkKind::OutputStart
                    && let Some(command) = self.pending_command.take()
                {
                    // Marks on one line all survive pruning by line, so the
                    // commands need their own cap.
                    if self.commands.len() >= MAX_MARKS {
                        self.commands.drain(..MAX_MARKS / 2);
                    }
                    self.commands.push((line, command));
                }
                if kind == MarkKind::OutputStart {
                    let command = self
                        .commands
                        .last()
                        .filter(|(l, _)| *l == line)
                        .map(|(_, c)| c.clone());
                    self.command_event(CommandEvent::Started { command });
                }
            }
            // VS Code's command-line report; `\\` and `\xNN` escapes.
            [b"633", b"E", command, rest @ ..] if self.mark_trusted(rest) => {
                let mut command = unescape_command(command);
                if command.len() > MAX_COMMAND_LEN {
                    let mut end = MAX_COMMAND_LEN;
                    while !command.is_char_boundary(end) {
                        end -= 1;
                    }
                    command.truncate(end);
                }
                self.pending_command = Some(command);
            }
            _ => {}
        }
    }
}

impl Perform for Term {
    fn print(&mut self, c: char) {
        self.input_char(c);
    }

    fn print_str(&mut self, s: &str) {
        self.input_str(s);
    }

    fn print_ascii(&mut self, s: &str) {
        if self.charsets[self.gl] != Charset::Ascii || self.modes.insert {
            s.chars().for_each(|c| self.input_char(c));
        } else {
            self.input_ascii(s.as_bytes());
        }
    }

    fn print_chars(&mut self, chars: &[char]) {
        self.input_chars(chars);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {
                self.bell = true;
                self.bells += 1;
            }
            0x08 => self.move_back(1),
            0x09 => self.tab_forward(1),
            0x0A..=0x0C => {
                self.index();
                if self.modes.linefeed_newline {
                    self.cursor.col = 0;
                }
            }
            0x0D => {
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            0x0E => self.gl = 1,
            0x0F => self.gl = 0,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        if ignore {
            return;
        }
        let arg = |i: usize, default: u16| params.arg(i, default) as usize;
        match (intermediates, action) {
            ([], b'@') => self.insert_blanks(arg(0, 1)),
            ([], b'A') => self.move_up(arg(0, 1)),
            ([], b'B' | b'e') => self.move_down(arg(0, 1)),
            ([], b'C' | b'a') => self.move_forward(arg(0, 1)),
            ([], b'D') => self.move_back(arg(0, 1)),
            ([], b'E') => {
                self.move_down(arg(0, 1));
                self.cursor.col = 0;
            }
            ([], b'F') => {
                self.move_up(arg(0, 1));
                self.cursor.col = 0;
            }
            ([], b'G' | b'`') => {
                self.cursor.col = (arg(0, 1) - 1).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            ([], b'H' | b'f') => self.goto(arg(0, 1) - 1, arg(1, 1) - 1),
            ([], b'I') => self.tab_forward(arg(0, 1)),
            ([] | [b'?'], b'J') => self.erase_display(params.arg(0, 0)),
            ([] | [b'?'], b'K') => self.erase_line(params.arg(0, 0)),
            ([], b'L') => self.insert_lines(arg(0, 1)),
            ([], b'M') => self.delete_lines(arg(0, 1)),
            ([], b'P') => self.delete_chars(arg(0, 1)),
            ([], b'S') => self.scroll_up(arg(0, 1)),
            ([], b'T') => self.scroll_down(arg(0, 1)),
            ([], b'X') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                self.fill(row, col, (col + arg(0, 1)).min(self.cols));
            }
            ([], b'Z') => self.tab_back(arg(0, 1)),
            ([], b'b') => {
                if let Some(c) = self.last_char {
                    // More than a screenful only overwrites itself. A row
                    // at a time, as text is printed: a character at a time,
                    // a few bytes could keep the terminal busy for long.
                    let mut left = arg(0, 1).min(self.cols * self.rows);
                    let row = vec![c; left.min(self.cols)];
                    while left > 0 {
                        let n = left.min(row.len());
                        self.input_chars(&row[..n]);
                        left -= n;
                    }
                }
            }
            ([], b'c') if arg(0, 0) == 0 => self.respond(b"\x1b[?62;22c"),
            ([b'>'], b'c') => self.respond(b"\x1b[>1;10;0c"),
            // XTVERSION: the terminal's name and version.
            ([b'>'], b'q') if params.arg(0, 0) == 0 => self.respond(
                concat!("\x1bP>|mightty ", env!("CARGO_PKG_VERSION"), "\x1b\\").as_bytes(),
            ),
            ([], b'd') => {
                let col = self.cursor.col;
                self.goto(arg(0, 1) - 1, col);
            }
            ([], b'g') => match arg(0, 0) {
                0 => self.tabs[self.cursor.col] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            ([], b'h') => self.set_ansi_modes(params, true),
            ([], b'l') => self.set_ansi_modes(params, false),
            ([b'?'], b'h') => self.set_dec_modes(params, true),
            ([b'?'], b'l') => self.set_dec_modes(params, false),
            // DECRQM: whether a mode is set (1), reset (2) or unknown (0).
            ([b'$'] | [b'?', b'$'], b'p') => {
                let mode = params.arg(0, 0);
                let dec = intermediates[0] == b'?';
                let state = if dec {
                    self.dec_mode(mode)
                } else {
                    self.ansi_mode(mode)
                };
                let reply = format!(
                    "\x1b[{}{mode};{}$y",
                    if dec { "?" } else { "" },
                    state.map_or(0, |on| if on { 1 } else { 2 })
                );
                self.respond(reply.as_bytes());
            }
            ([], b'm') => self.sgr(params),
            ([], b'n') => match arg(0, 0) {
                5 => self.respond(b"\x1b[0n"),
                6 => {
                    // A restored cursor can sit above a region set since.
                    let base = if self.modes.origin { self.top } else { 0 };
                    let reply = format!(
                        "\x1b[{};{}R",
                        self.cursor.row.saturating_sub(base) + 1,
                        self.cursor.col + 1
                    );
                    self.respond(reply.as_bytes());
                }
                _ => {}
            },
            ([], b'r') => self.set_scroll_region(arg(0, 1), arg(1, self.rows as u16)),
            ([], b's') => self.save_cursor(),
            ([], b'u') => self.restore_cursor(),
            // Kitty keyboard protocol: query, push, pop, set.
            ([b'?'], b'u') => {
                let reply = format!("\x1b[?{}u", self.key_flags());
                self.respond(reply.as_bytes());
            }
            ([b'>'], b'u') => {
                let flags = arg(0, 0) as u8 & KEY_ALL;
                let stack = self.key_flags_mut();
                if stack.len() > MAX_KEY_FLAGS {
                    stack.remove(1);
                }
                stack.push(flags);
            }
            ([b'<'], b'u') => {
                let stack = self.key_flags_mut();
                let keep = stack.len().saturating_sub(arg(0, 1).max(1)).max(1);
                stack.truncate(keep);
                if keep == 1 {
                    stack[0] = 0;
                }
            }
            ([b'='], b'u') => {
                let flags = arg(0, 0) as u8 & KEY_ALL;
                let mode = arg(1, 1);
                let current = self.key_flags_mut().last_mut().expect("never empty");
                match mode {
                    1 => *current = flags,
                    2 => *current |= flags,
                    3 => *current &= !flags,
                    _ => {}
                }
            }
            ([b' '], b'q') => self.modes.cursor_shape = params.arg(0, 0),
            // XTWINOPS size reports: the text area and a cell in pixels, the grid in cells.
            ([], b't') => {
                let (cw, ch) = self.cell_pixels;
                let reply = match arg(0, 0) {
                    14 => format!(
                        "\x1b[4;{};{}t",
                        ch as usize * self.rows,
                        cw as usize * self.cols
                    ),
                    16 => format!("\x1b[6;{ch};{cw}t"),
                    18 => format!("\x1b[8;{};{}t", self.rows, self.cols),
                    _ => return,
                };
                self.respond(reply.as_bytes());
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.index(),
            ([], b'E') => {
                self.index();
                self.cursor.col = 0;
            }
            ([], b'H') => self.tabs[self.cursor.col] = true,
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.reset(),
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([b'(' | b')'], set) => {
                let slot = (intermediates[0] == b')') as usize;
                self.charsets[slot] = if set == b'0' {
                    Charset::DecGraphics
                } else {
                    Charset::Ascii
                };
            }
            ([b'#'], b'8') => {
                let cell = Cell::new('E', StyleId::DEFAULT, Flags::empty());
                let cols = self.cols;
                for row in self.lines_mut() {
                    row.cells.clear();
                    row.cells.resize(cols, cell);
                    row.wrapped = false;
                }
                self.top = 0;
                self.bottom = self.rows - 1;
                self.goto(0, 0);
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.osc(params, bell_terminated);
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        self.apc(data);
    }
}

/// SGR 38/48/58 colour starting at group `i`. Returns the colour and the
/// number of extra groups consumed (semicolon form spreads over groups).
fn extended_color(params: &Params, i: usize) -> (Option<Color>, usize) {
    let group = params.get(i).unwrap_or(&[]);
    if group.len() > 1 {
        // Colon form: 38:5:n, 38:2:r:g:b, or 38:2:colorspace:r:g:b.
        let color = match (group[1], &group[2..]) {
            (5, [n, ..]) => Some(Color::Indexed(*n as u8)),
            (2, [_, r, g, b, ..]) | (2, [r, g, b]) => {
                Some(Color::Rgb(*r as u8, *g as u8, *b as u8))
            }
            _ => None,
        };
        return (color, 0);
    }
    let next = |k: usize| params.get(i + k).and_then(|g| g.first().copied());
    match next(1) {
        Some(5) => (next(2).map(|n| Color::Indexed(n as u8)), 2),
        Some(2) => match (next(2), next(3), next(4)) {
            (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(r as u8, g as u8, b as u8)), 4),
            _ => (None, 4),
        },
        _ => (None, 0),
    }
}

/// Undoes the escaping of OSC 633;E: `\\` → `\`, `\xNN` → byte NN.
fn unescape_command(raw: &[u8]) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let hex = |j: usize| raw.get(j).and_then(|&c| (c as char).to_digit(16));
        match (raw[i], raw.get(i + 1)) {
            (b'\\', Some(b'\\')) => {
                out.push(b'\\');
                i += 2;
            }
            (b'\\', Some(b'x')) if hex(i + 2).is_some() && hex(i + 3).is_some() => {
                out.push((hex(i + 2).unwrap() * 16 + hex(i + 3).unwrap()) as u8);
                i += 4;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    displayable(&String::from_utf8_lossy(&out), MAX_COMMAND_LEN)
}

/// `file://host/path` as (host, decoded path).
fn parse_file_url(url: &[u8]) -> Option<(String, String)> {
    let rest = url.strip_prefix(b"file://")?;
    let (host, path) = rest.split_at(rest.iter().position(|&c| c == b'/')?);
    let host = String::from_utf8(host.to_vec()).ok()?;
    if host.chars().any(char::is_control) {
        return None;
    }
    let mut out = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            path[i],
            path.get(i + 1).copied().and_then(hex),
            path.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(hi), Some(lo)) => {
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Paths are shown and printed (`mightty ctl list`): no control characters.
    let path = String::from_utf8(out).ok()?;
    if path.chars().any(char::is_control) {
        return None;
    }
    Some((host, path))
}

/// `text` with control and bidirectional-override characters replaced by
/// U+FFFD, and cut to `max` characters: for titles and command lines that
/// are shown in the UI or printed to terminals, where they could otherwise
/// run escape sequences or reorder what's shown.
fn displayable(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || is_bidi_control(c) {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .take(max)
        .collect()
}

/// Bidirectional formatting characters, which can make text read other
/// than it is: a file name ending `txt.exe` shown as `exe.txt`.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}')
}

/// DEC Special Graphics (`ESC ( 0`): line drawing used by many TUIs.
fn dec_graphics(c: char) -> char {
    const MAP: [char; 32] = [
        ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', //
        '⎺', '⎻', '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
    ];
    match c {
        '_'..='~' => MAP[c as usize - '_' as usize],
        _ => c,
    }
}

/// Blanks rows in place, keeping their allocations. Default blanks just
/// empty the row (see `Row::cells`); coloured ones (BCE) fill all `cols`.
fn clear_rows(rows: &mut [Row], blank: Cell, cols: usize) {
    for row in rows {
        row.cells.clear();
        if blank != Cell::BLANK {
            row.cells.resize(cols, blank);
        }
        row.wrapped = false;
    }
}

/// Grows a lazily short row so that `cells[..len]` exists, copying from a
/// static row of blanks (a memcpy, unlike `resize`'s per-cell loop).
fn grow(cells: &mut Vec<Cell>, len: usize) {
    static BLANKS: [Cell; 256] = [Cell::BLANK; 256];
    while cells.len() < len {
        let n = (len - cells.len()).min(BLANKS.len());
        cells.extend_from_slice(&BLANKS[..n]);
    }
}

fn flag_at(cells: &[Cell], col: usize, flag: Flags) -> bool {
    cells.get(col).is_some_and(|c| c.flags().contains(flag))
}

/// `cells.fill(value)` by doubling `copy_within`: a few vectorised memcpys
/// instead of one store per field per cell. Hot: runs for every scrolled line.
fn fill_cells(cells: &mut [Cell], value: Cell) {
    let Some(first) = cells.first_mut() else {
        return;
    };
    *first = value;
    let mut filled = 1;
    while filled < cells.len() {
        let n = filled.min(cells.len() - filled);
        cells.copy_within(..n, filled);
        filled += n;
    }
}

/// Longest grapheme cluster kept, in UTF-8 bytes; what follows is dropped.
/// Without a cap, a stream of combining marks on one cell would copy an
/// ever longer cluster per mark. Real ones fit: a family of four with skin
/// tones is 41 bytes. With every id in use the table then holds at most a
/// few MiB.
const MAX_CLUSTER_BYTES: usize = 64;

/// See `Term::link_gc_backoff`.
const LINK_GC_BACKOFF: u32 = 4096;

/// What a plain-text URL starts with, for [`Term::url_at`].
const URL_SCHEMES: &[&str] = &[
    "https://", "http://", "file://", "ftp://", "ssh://", "git://", "mailto:",
];

/// A regional indicator: flags are pairs of them.
fn is_regional(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}

/// Might join the cluster before it even though it has width: a regional
/// indicator or an emoji skin-tone modifier.
#[inline]
fn may_join(c: char) -> bool {
    width_class(c) == width::JOINS
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|c| c > 0 && c % TAB_WIDTH == 0).collect()
}

#[cfg(test)]
mod tests;
