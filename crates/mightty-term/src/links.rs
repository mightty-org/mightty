use std::collections::HashMap;

/// Longest hyperlink target kept, in bytes (as in kitty); longer ones are ignored.
pub(crate) const MAX_URI: usize = 2048;
/// Longest OSC 8 `id=` kept, in bytes.
const MAX_ID: usize = 250;
/// Ids are `u16`, 0 meaning no link.
const IDS: usize = u16::MAX as usize;

/// OSC 8 hyperlink targets, interned: styles carry an id (see
/// [`Style::link`](crate::Style::link)), which moves with the cells. Links
/// that share an OSC 8 `id` and target are one link, so hovering one part
/// of it finds the rest. Ids no style uses are reclaimed when the table runs out.
#[derive(Clone, Default)]
pub(crate) struct Links {
    /// `uris[id - 1]`; freed ones are empty.
    uris: Vec<Box<str>>,
    ids: HashMap<(Box<str>, Box<str>), u16>,
    free: Vec<u16>,
}

impl Links {
    pub(crate) fn get(&self, id: u16) -> Option<&str> {
        let uri = self.uris.get(usize::from(id).checked_sub(1)?)?;
        (!uri.is_empty()).then_some(&**uri)
    }

    /// The id for `uri` under OSC 8 `id` (possibly empty), or `None` when
    /// either is too long or every id is in use.
    pub(crate) fn intern(&mut self, id: &str, uri: &str) -> Option<u16> {
        if uri.is_empty() || uri.len() > MAX_URI || id.len() > MAX_ID {
            return None;
        }
        let key = (Box::from(id), Box::from(uri));
        if let Some(&n) = self.ids.get(&key) {
            return Some(n);
        }
        let n = match self.free.pop() {
            Some(n) => {
                self.uris[usize::from(n) - 1] = uri.into();
                n
            }
            None if self.uris.len() < IDS => {
                self.uris.push(uri.into());
                self.uris.len() as u16
            }
            None => return None,
        };
        self.ids.insert(key, n);
        Some(n)
    }

    pub(crate) fn is_full(&self) -> bool {
        self.free.is_empty() && self.uris.len() == IDS
    }

    /// Frees the ids `live` doesn't contain.
    pub(crate) fn collect(&mut self, live: impl Fn(u16) -> bool) {
        self.free.clear();
        for (i, uri) in self.uris.iter_mut().enumerate() {
            let n = i as u16 + 1;
            if !live(n) {
                *uri = Box::default();
                self.free.push(n);
            }
        }
        self.ids.retain(|_, n| live(*n));
    }
}
