//! The command palette (⌘⇧P, as in Ghostty): a panel at the top of the pane
//! listing actions with their keys, filtered as the field is typed in; the
//! arrows move the selection, Return runs it, Escape closes. What runs is
//! up to `app`; this is the list and its AppKit side.

use crate::bindings::{Action, Bindings, Key, Named, Trigger, ALT, CMD, CTRL, SHIFT};
use crate::ffi::*;
use crate::split::Direction;

const WIDTH: f64 = 520.0;
const MARGIN: f64 = 10.0;
const PAD: f64 = 6.0;
const FIELD_HEIGHT: f64 = 26.0;
const ROW_HEIGHT: f64 = 24.0;
/// Rows shown at most; the rest are reached by typing more.
const ROWS: usize = 10;

/// Every action the palette offers, by title.
fn commands() -> Vec<(&'static str, Action)> {
    use Action::*;
    vec![
        ("New Window", NewWindow),
        ("New Tab", NewTab),
        ("Split Right", NewSplit(Direction::Right)),
        ("Split Down", NewSplit(Direction::Down)),
        ("Change Title…", PromptSurfaceTitle),
        ("Close Pane", CloseSurface),
        ("Undo Close", Undo),
        ("Reopen Closed", ReopenClosed),
        ("Close Window", CloseWindow),
        ("Minimize Window", MinimizeWindow),
        ("Copy", Copy),
        ("Paste", Paste),
        ("Find…", StartSearch),
        ("Clear Screen", ClearScreen),
        ("Next Pane", GotoSplitStep(1)),
        ("Previous Pane", GotoSplitStep(-1)),
        ("Zoom Pane", ToggleSplitZoom),
        ("Equalize Panes", EqualizeSplits),
        ("Next Tab", NextTab),
        ("Previous Tab", PreviousTab),
        ("Jump to Pane Needing You", JumpToAttention),
        ("Increase Font Size", ChangeFontSize(1.0)),
        ("Decrease Font Size", ChangeFontSize(-1.0)),
        ("Reset Font Size", ResetFontSize),
        ("Scroll to Top", ScrollToTop),
        ("Scroll to Bottom", ScrollToBottom),
        ("Scroll Page Up", ScrollPageUp),
        ("Scroll Page Down", ScrollPageDown),
        ("Quick Terminal", ToggleQuickTerminal),
        ("Secure Keyboard Entry", ToggleSecureInput),
        ("Open Settings", OpenConfig),
        ("Reload Configuration", ReloadConfig),
        ("Quit mightty", Quit),
    ]
}

/// One line of the palette: its title, the keys bound to it, what it does.
pub struct Entry {
    title: &'static str,
    keys: String,
    pub action: Action,
}

/// The commands with the first key `bindings` gives each.
pub fn entries(bindings: &Bindings) -> Vec<Entry> {
    commands()
        .into_iter()
        .map(|(title, action)| Entry { title, keys: bindings.trigger_for(&action).map(symbols).unwrap_or_default(), action })
        .collect()
}

/// A trigger as menus show it: `⌃⌥⇧⌘P`.
fn symbols(trigger: Trigger) -> String {
    let mut out: String = [(CTRL, '⌃'), (ALT, '⌥'), (SHIFT, '⇧'), (CMD, '⌘')]
        .into_iter()
        .filter(|&(bit, _)| trigger.mods & bit != 0)
        .map(|(_, c)| c)
        .collect();
    match trigger.key {
        Key::Char(c) => out.extend(c.to_uppercase()),
        Key::Named(Named::F(n)) => out += &format!("F{n}"),
        Key::Named(n) => out += match n {
            Named::Enter => "↩",
            Named::Tab => "⇥",
            Named::Space => "Space",
            Named::Backspace => "⌫",
            Named::Delete => "⌦",
            Named::Escape => "⎋",
            Named::Up => "↑",
            Named::Down => "↓",
            Named::Left => "←",
            Named::Right => "→",
            Named::Home => "↖",
            Named::End => "↘",
            Named::PageUp => "⇞",
            Named::PageDown => "⇟",
            Named::F(_) => "",
        },
    }
    out
}

/// The titles matching `query`, best first: its letters in order, ignoring
/// case. A title starting with it beats one with a word starting with it,
/// then one whose words' initials hold it (`nt`: New Tab), then letters
/// scattered, fewer gaps first; ties keep the list's order.
fn filter(query: &str, titles: &[&str]) -> Vec<usize> {
    let query: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    let mut scored: Vec<(usize, usize)> = titles
        .iter()
        .enumerate()
        .filter_map(|(i, title)| {
            let lower = title.to_lowercase();
            let squashed: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
            let wanted: String = query.iter().collect();
            if squashed.starts_with(&wanted) {
                return Some((0, i));
            }
            if lower.split(' ').any(|word| word.starts_with(&wanted)) {
                return Some((1, i));
            }
            let mut initials = lower.split(' ').filter_map(|word| word.chars().next());
            if query.iter().all(|&c| initials.any(|i| i == c)) {
                return Some((2, i));
            }
            // In order, counting the gaps between letters found.
            let (mut at, mut gaps, mut last) = (0, 0, None);
            for (j, c) in lower.chars().enumerate() {
                if at < query.len() && c == query[at] {
                    gaps += usize::from(last.is_some_and(|l| l + 1 != j));
                    last = Some(j);
                    at += 1;
                }
            }
            (at == query.len()).then_some((3 + gaps, i))
        })
        .collect();
    scored.sort();
    scored.into_iter().map(|(_, i)| i).collect()
}

pub struct Palette {
    /// Ours (+1); holds the controls below, which live as long as it does.
    panel: Id,
    pub field: Id,
    /// A line under the field, and the selected row's rounded tint.
    divider: Id,
    highlight: Id,
    /// Each row's title and keys.
    rows: Vec<(Id, Id)>,
    entries: Vec<Entry>,
    /// Entries matching the field, best first, and the one selected.
    matches: Vec<usize>,
    selected: usize,
}

impl Palette {
    /// Adds the palette to `container`, at the top. `target` is the field's
    /// delegate; `dark` picks a dark panel for a dark theme.
    pub unsafe fn open(container: Id, target: Id, dark: bool, entries: Vec<Entry>) -> Self {
        unsafe {
            let panel: Id = msg!(msg!(class(c"NSVisualEffectView"), c"alloc"), c"initWithFrame:", rect(0.0, 0.0, WIDTH, FIELD_HEIGHT));
            call!(panel, c"setMaterial:", 6isize); // popover
            call!(panel, c"setBlendingMode:", 1isize); // within the window
            call!(panel, c"setState:", 1isize); // always active
            call!(panel, c"setAutoresizingMask:", 2usize | 8); // centred, at the top
            let name = if dark { "NSAppearanceNameDarkAqua" } else { "NSAppearanceNameAqua" };
            let appearance: Id = msg!(class(c"NSAppearance"), c"appearanceNamed:", nsstring(name));
            call!(panel, c"setAppearance:", appearance);
            call!(panel, c"setWantsLayer:", YES);
            let layer: Id = msg!(panel, c"layer");
            call!(layer, c"setCornerRadius:", 10.0f64);
            call!(layer, c"setMasksToBounds:", YES);
            call!(layer, c"setBorderWidth:", 1.0f64);
            let border: Id = msg!(msg!(class(c"NSColor"), c"separatorColor"), c"CGColor");
            call!(layer, c"setBorderColor:", border);

            let field: Id = msg!(msg!(class(c"NSTextField"), c"alloc"), c"initWithFrame:", rect(0.0, 0.0, WIDTH, FIELD_HEIGHT));
            call!(field, c"setPlaceholderString:", nsstring("Run a command…"));
            call!(field, c"setBezeled:", NO);
            call!(field, c"setDrawsBackground:", NO);
            call!(field, c"setFocusRingType:", 1usize); // none: the panel frames it
            let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 15.0f64);
            call!(field, c"setFont:", font);
            call!(field, c"setDelegate:", target);
            call!(panel, c"addSubview:", field);
            call!(field, c"release");

            // Faint: the text colour, lightly, so it suits either appearance.
            let tint = |alpha: f64| -> Id {
                let color: Id = msg!(class(c"NSColor"), c"colorWithWhite:alpha:", if dark { 1.0f64 } else { 0.0f64 }, alpha);
                msg!(color, c"CGColor")
            };
            let plain = |radius: f64, alpha: f64| -> Id {
                let view: Id = msg!(msg!(class(c"NSView"), c"alloc"), c"initWithFrame:", rect(0.0, 0.0, 0.0, 0.0));
                call!(view, c"setWantsLayer:", YES);
                let layer: Id = msg!(view, c"layer");
                call!(layer, c"setBackgroundColor:", tint(alpha));
                call!(layer, c"setCornerRadius:", radius);
                call!(panel, c"addSubview:", view);
                call!(view, c"release");
                view
            };
            let divider = plain(0.0, 0.1);
            // Behind the rows' text: added before it.
            let highlight = plain(6.0, if dark { 0.12 } else { 0.08 });

            let label = |align: isize| -> Id {
                let label: Id = msg!(class(c"NSTextField"), c"labelWithString:", nsstring(""));
                call!(label, c"setAlignment:", align);
                let font: Id = msg!(class(c"NSFont"), c"systemFontOfSize:", 13.0f64);
                call!(label, c"setFont:", font);
                call!(panel, c"addSubview:", label);
                label
            };
            let rows = (0..ROWS).map(|_| (label(0), label(2))).collect(); // left, right

            call!(container, c"addSubview:", panel);
            let mut palette = Self { panel, field, divider, highlight, rows, entries, matches: Vec::new(), selected: 0 };
            palette.update();
            palette
        }
    }

    /// Filters by the field's text, selecting the best match.
    pub fn update(&mut self) {
        // SAFETY: main thread; documented signature.
        let query = unsafe { string_from_ns(msg!(self.field, c"stringValue")) };
        let titles: Vec<&str> = self.entries.iter().map(|e| e.title).collect();
        self.matches = filter(&query, &titles);
        self.selected = 0;
        self.show();
    }

    /// Moves the selection by `step`, wrapping around.
    pub fn step(&mut self, step: isize) {
        let shown = self.matches.len().min(ROWS);
        if shown > 0 {
            self.selected = (self.selected as isize + step).rem_euclid(shown as isize) as usize;
            self.show();
        }
    }

    /// The selected entry's action.
    pub fn chosen(&self) -> Option<Action> {
        self.matches.get(self.selected).map(|&i| self.entries[i].action.clone())
    }

    /// Lays the panel out for the matches, centred at the top of its container.
    fn show(&self) {
        // SAFETY: main thread; documented signatures.
        unsafe {
            let container: Id = msg!(self.panel, c"superview");
            let bounds = send_rect(container, c"bounds");
            let width = WIDTH.min(bounds.size.width - 2.0 * MARGIN).max(200.0);
            let shown = self.matches.len().min(ROWS);
            let height = PAD + FIELD_HEIGHT + PAD + shown as f64 * ROW_HEIGHT + if shown > 0 { PAD } else { 0.0 };
            let frame = rect(((bounds.size.width - width) / 2.0).max(0.0), bounds.size.height - height - MARGIN, width, height);
            call!(self.panel, c"setFrame:", frame);
            call!(self.field, c"setFrame:", rect(PAD + 8.0, height - PAD - FIELD_HEIGHT + 2.0, width - 2.0 * PAD - 16.0, FIELD_HEIGHT - 4.0));
            let below_field = height - PAD - FIELD_HEIGHT - PAD / 2.0;
            call!(self.divider, c"setFrame:", rect(0.0, below_field, width, 1.0));
            call!(self.divider, c"setHidden:", if shown > 0 { NO } else { YES });
            call!(self.highlight, c"setHidden:", if shown > 0 { NO } else { YES });
            let color = |name: &std::ffi::CStr| -> Id { msg!(class(c"NSColor"), name) };
            // Rows hold their text vertically centred, a line's height being less than a row's.
            let inset = (ROW_HEIGHT - 17.0) / 2.0;
            for (r, &(title, keys)) in self.rows.iter().enumerate() {
                let entry = self.matches.get(r).filter(|_| r < shown).map(|&i| &self.entries[i]);
                call!(title, c"setHidden:", if entry.is_some() { NO } else { YES });
                call!(keys, c"setHidden:", if entry.is_some() { NO } else { YES });
                let Some(entry) = entry else { continue };
                let y = height - PAD - FIELD_HEIGHT - PAD - (r + 1) as f64 * ROW_HEIGHT;
                if r == self.selected {
                    call!(self.highlight, c"setFrame:", rect(PAD, y, width - 2.0 * PAD, ROW_HEIGHT));
                }
                call!(title, c"setFrame:", rect(PAD + 10.0, y + inset, width - 2.0 * PAD - 140.0, ROW_HEIGHT - 2.0 * inset));
                call!(keys, c"setFrame:", rect(width - PAD - 10.0 - 120.0, y + inset, 120.0, ROW_HEIGHT - 2.0 * inset));
                call!(title, c"setStringValue:", nsstring(entry.title));
                call!(keys, c"setStringValue:", nsstring(&entry.keys));
                call!(title, c"setTextColor:", color(c"labelColor"));
                call!(keys, c"setTextColor:", color(c"secondaryLabelColor"));
            }
        }
    }
}

impl Drop for Palette {
    fn drop(&mut self) {
        // SAFETY: main thread; the panel's subviews go with it.
        unsafe {
            call!(self.panel, c"removeFromSuperview");
            call!(self.panel, c"release");
        }
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect { origin: CGPoint { x, y }, size: CGSize { width, height } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtering() {
        let titles = ["New Window", "New Tab", "Split Right", "Next Tab", "Reload Configuration", "Previous Tab"];
        assert_eq!(filter("", &titles), [0, 1, 2, 3, 4, 5]);
        // A start beats a word's start, which beats scattered letters.
        assert_eq!(filter("tab", &titles), [1, 3, 5]);
        // Initials beat letters side by side elsewhere (`nt` in Font).
        assert_eq!(filter("nt", &titles), [1, 3, 4]);
        assert_eq!(filter("nt", &["Decrease Font Size", "New Tab"]), [1, 0]);
        assert_eq!(filter("newt", &titles), [1]);
        assert_eq!(filter("RELOAD", &titles), [4]);
        assert_eq!(filter("rc", &titles), [4]);
        assert!(filter("xyz", &titles).is_empty());
    }

    #[test]
    fn shortcuts() {
        let keys = |s: &str| symbols(Trigger::parse(s).unwrap());
        assert_eq!(keys("cmd+shift+p"), "⇧⌘P");
        assert_eq!(keys("ctrl+alt+page_up"), "⌃⌥⇞");
        assert_eq!(keys("cmd+f5"), "⌘F5");
        let entries = entries(&Bindings::default());
        let new_tab = entries.iter().find(|e| e.action == Action::NewTab).unwrap();
        assert_eq!(new_tab.keys, "⌘T");
    }
}
