//! Desktop notifications with a title, body, buttons and urgency
//! (OSC 99): `OSC 99 ; key=value:key=value ; payload ST`.
//!
//! A notification can come in chunks (`d=0` until the last), each adding
//! to its title, body or buttons (`p=`); `e=1` says the payload is
//! base64. A program can close one it showed (`p=close`), ask which are
//! still shown (`p=alive`), and ask what's supported (`p=?`). Clicks,
//! button presses and closing are reported back when asked for.

use crate::base64;
use std::time::Duration;

/// Notifications being sent in chunks, at most; more drop the oldest.
const MAX_PARTIAL: usize = 8;
/// Bytes kept of a payload being put together, before decoding.
const MAX_PAYLOAD: usize = 16 * 1024;
/// Buttons kept, and characters of each label.
const MAX_BUTTONS: usize = 8;
const MAX_BUTTON_TEXT: usize = 64;
/// Characters of an identifier kept.
const MAX_ID: usize = 64;

/// What a program asked of desktop notifications, for the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationRequest {
    /// Show it, replacing one with the same identifier.
    Show(crate::Notification),
    /// Take back the one with this identifier.
    Close(String),
    /// Reply with the identifiers of those still shown; the request's own
    /// identifier is echoed.
    Alive(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

/// When to show it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Occasion {
    #[default]
    Always,
    /// Only if the pane isn't the one being typed into.
    Unfocused,
    /// Only if the pane can't be seen.
    Invisible,
}

/// What an OSC 99 notification adds to a title and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rich {
    /// Letters, digits and `-_+.`; replaces one shown with the same.
    pub id: Option<String>,
    pub urgency: Urgency,
    pub occasion: Occasion,
    pub buttons: Vec<String>,
    /// Clicking it brings its pane to the front.
    pub focus: bool,
    /// Clicks and button presses are reported to the program.
    pub report: bool,
    /// Closing is reported to the program.
    pub report_close: bool,
    pub silent: bool,
    /// Taken back after this long.
    pub expire: Option<Duration>,
}

impl Default for Rich {
    fn default() -> Self {
        Self {
            id: None,
            urgency: Urgency::Normal,
            occasion: Occasion::Always,
            buttons: Vec::new(),
            focus: true,
            report: false,
            report_close: false,
            silent: false,
            expire: None,
        }
    }
}

/// Reply to a click on notification `id` (`None`: it had none), or on its
/// button `button` (from 1).
pub fn activated_reply(id: Option<&str>, button: Option<usize>) -> Vec<u8> {
    let button = button.map(|b| b.to_string()).unwrap_or_default();
    format!("\x1b]99;i={};{button}\x1b\\", id.unwrap_or("0")).into_bytes()
}

/// Reply saying notification `id` was closed.
pub fn closed_reply(id: Option<&str>) -> Vec<u8> {
    format!("\x1b]99;i={}:p=close;\x1b\\", id.unwrap_or("0")).into_bytes()
}

/// Reply to `p=alive` from `id`: the notifications still shown.
pub fn alive_reply(id: &str, alive: &[&str]) -> Vec<u8> {
    format!("\x1b]99;i={id}:p=alive;{}\x1b\\", alive.join(",")).into_bytes()
}

/// A notification whose chunks are still coming.
#[derive(Debug, Default)]
struct Partial {
    id: Option<String>,
    rich: Rich,
    title: Field,
    body: Field,
    buttons: Field,
}

/// A payload being put together: runs of chunks, each base64 or not.
#[derive(Debug, Default)]
struct Field {
    runs: Vec<(Vec<u8>, bool)>,
    len: usize,
}

impl Field {
    fn add(&mut self, payload: &[u8], base64: bool) {
        let payload = &payload[..payload.len().min(MAX_PAYLOAD.saturating_sub(self.len))];
        self.len += payload.len();
        match self.runs.last_mut() {
            // Base64 chunks run together, as one may end mid-character.
            Some((bytes, b)) if *b == base64 => bytes.extend_from_slice(payload),
            _ => self.runs.push((payload.to_vec(), base64)),
        }
    }

    fn text(&self) -> String {
        let mut bytes = Vec::new();
        for (run, base64) in &self.runs {
            if *base64 {
                bytes.extend(decode(run).unwrap_or_default());
            } else {
                bytes.extend_from_slice(run);
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Notifications being put together from chunks.
#[derive(Debug, Default)]
pub(crate) struct Chunks {
    partial: Vec<Partial>,
}

/// What one OSC 99 sequence came to.
pub(crate) enum Parsed {
    Nothing,
    Request(NotificationRequest),
    /// The reply to `p=?`.
    Reply(Vec<u8>),
}

/// `[a-zA-Z0-9-_+.]`, which is safe to echo back; others are dropped.
fn identifier(value: &[u8]) -> Option<String> {
    let id: String = value
        .iter()
        .filter(|c| c.is_ascii_alphanumeric() || b"-_+.".contains(c))
        .take(MAX_ID)
        .map(|&c| char::from(c))
        .collect();
    (!id.is_empty()).then_some(id)
}

/// Base64 that may be several padded pieces run together, as chunks
/// encoded one by one are.
fn decode(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        // A piece ends after its padding.
        let end = match rest.iter().position(|&c| c == b'=') {
            Some(at) => at + rest[at..].iter().take_while(|&&c| c == b'=').count(),
            None => rest.len(),
        };
        out.extend(base64::decode(&rest[..end])?);
        rest = &rest[end..];
    }
    Some(out)
}

impl Chunks {
    /// One `OSC 99 ; meta ; payload`.
    pub(crate) fn parse(&mut self, meta: &[u8], payload: &[u8]) -> Parsed {
        let mut id = None;
        let mut done = true;
        let mut kind: &[u8] = b"title";
        let mut base64 = false;
        let mut set: Vec<(u8, &[u8])> = Vec::new();
        for pair in meta.split(|&c| c == b':') {
            let Some((&key, value)) = pair.split_first() else {
                continue;
            };
            let Some(value) = value.strip_prefix(b"=") else {
                continue;
            };
            match key {
                b'i' => id = identifier(value),
                b'd' => done = value != b"0",
                b'p' => kind = value,
                b'e' => base64 = value == b"1",
                // Unknown keys are ignored, as the protocol asks.
                b'a' | b'o' | b'u' | b'c' | b'w' | b's' => set.push((key, value)),
                _ => {}
            }
        }
        match kind {
            b"?" => return Parsed::Reply(capabilities(id.as_deref())),
            b"close" => {
                return id.map_or(Parsed::Nothing, |id| {
                    Parsed::Request(NotificationRequest::Close(id))
                });
            }
            b"alive" => {
                let id = id.unwrap_or_else(|| "0".into());
                return Parsed::Request(NotificationRequest::Alive(id));
            }
            _ => {}
        }
        let at = match self.partial.iter().position(|p| p.id == id) {
            Some(at) => at,
            None => {
                if self.partial.len() == MAX_PARTIAL {
                    self.partial.remove(0);
                }
                self.partial.push(Partial {
                    id: id.clone(),
                    ..Partial::default()
                });
                self.partial.len() - 1
            }
        };
        let partial = &mut self.partial[at];
        for (key, value) in set {
            apply(&mut partial.rich, key, value);
        }
        let field = match kind {
            b"title" => Some(&mut partial.title),
            b"body" => Some(&mut partial.body),
            b"buttons" => Some(&mut partial.buttons),
            // Icons, and kinds yet to come, are left out.
            _ => None,
        };
        if let Some(field) = field {
            field.add(payload, base64);
        }
        if !done {
            return Parsed::Nothing;
        }
        let mut partial = self.partial.remove(at);
        let title = partial.title.text();
        let body = partial.body.text();
        let buttons = partial.buttons.text();
        partial.rich.id = partial.id;
        partial.rich.buttons = buttons
            .split('\u{2028}')
            .map(|b| crate::safe_text(b, MAX_BUTTON_TEXT))
            .filter(|b| !b.trim().is_empty())
            .take(MAX_BUTTONS)
            .collect();
        if title.is_empty() && body.is_empty() {
            return Parsed::Nothing;
        }
        Parsed::Request(NotificationRequest::Show(crate::Notification {
            title: crate::safe_text(&title, crate::MAX_NOTIFICATION_TEXT),
            body: crate::safe_text(&body, crate::MAX_NOTIFICATION_TEXT),
            rich: Some(partial.rich),
        }))
    }
}

/// Applies one of `a`, `o`, `u`, `c`, `w` and `s`; values not known are
/// ignored, leaving the default.
fn apply(rich: &mut Rich, key: u8, value: &[u8]) {
    match key {
        b'a' => {
            for action in value.split(|&c| c == b',') {
                let (on, action) = match action.strip_prefix(b"-") {
                    Some(action) => (false, action),
                    None => (true, action),
                };
                match action {
                    b"focus" => rich.focus = on,
                    b"report" => rich.report = on,
                    _ => {}
                }
            }
        }
        b'o' => match value {
            b"always" => rich.occasion = Occasion::Always,
            b"unfocused" => rich.occasion = Occasion::Unfocused,
            b"invisible" => rich.occasion = Occasion::Invisible,
            _ => {}
        },
        b'u' => match value {
            b"0" => rich.urgency = Urgency::Low,
            b"1" => rich.urgency = Urgency::Normal,
            b"2" => rich.urgency = Urgency::Critical,
            _ => {}
        },
        b'c' => rich.report_close = value == b"1",
        b'w' => {
            if let Some(ms) = std::str::from_utf8(value)
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
            {
                rich.expire = (ms > 0).then(|| Duration::from_millis(ms as u64));
            }
        }
        b's' => {
            if let Some(name) = base64::decode(value) {
                rich.silent = name == b"silent";
            }
        }
        _ => {}
    }
}

/// The reply to `p=?`: what's supported.
fn capabilities(id: Option<&str>) -> Vec<u8> {
    format!(
        "\x1b]99;i={}:p=?;a=focus,report:c=1:o=always,unfocused,invisible:\
         p=title,body,?,close,alive,buttons:s=system,silent:u=0,1,2:w=1\x1b\\",
        id.unwrap_or("0")
    )
    .into_bytes()
}
