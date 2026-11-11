/// Maximum number of values (including sub-parameters) kept per sequence.
pub const MAX_PARAMS: usize = 32;

/// CSI/DCS parameters. `;` starts a new group, `:` adds a sub-parameter to the
/// current group, so `38:2::255:0:0` stays one group while `38;5;1` is three.
#[derive(Clone, Debug, Default)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    ends: [u8; MAX_PARAMS],
    n_values: usize,
    groups: usize,
    current: u32,
    pending: bool,
}

impl Params {
    pub fn len(&self) -> usize {
        self.groups
    }

    pub fn is_empty(&self) -> bool {
        self.groups == 0
    }

    /// Group `i`: the parameter followed by its sub-parameters.
    pub fn get(&self, i: usize) -> Option<&[u16]> {
        if i >= self.groups {
            return None;
        }
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        Some(&self.values[start..self.ends[i] as usize])
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u16]> + '_ {
        (0..self.groups).filter_map(|i| self.get(i))
    }

    /// First value of group `i`, with missing or zero values replaced by `default`.
    pub fn arg(&self, i: usize, default: u16) -> u16 {
        match self.get(i).and_then(|g| g.first()) {
            Some(&0) | None => default,
            Some(&v) => v,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.n_values = 0;
        self.groups = 0;
        self.current = 0;
        self.pending = false;
    }

    /// Accepts `0-9`, `:` and `;`.
    pub(crate) fn push_byte(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                self.current = (self.current * 10 + u32::from(b - b'0')).min(u32::from(u16::MAX));
            }
            b':' => self.push_value(),
            _ => {
                self.push_value();
                self.end_group();
            }
        }
        self.pending = true;
    }

    pub(crate) fn finish(&mut self) {
        if self.pending {
            self.push_value();
            self.end_group();
            self.pending = false;
        }
    }

    fn push_value(&mut self) {
        if self.n_values < MAX_PARAMS {
            self.values[self.n_values] = self.current as u16;
            self.n_values += 1;
        }
        self.current = 0;
    }

    fn end_group(&mut self) {
        let start = if self.groups == 0 {
            0
        } else {
            self.ends[self.groups - 1] as usize
        };
        if self.groups < MAX_PARAMS && self.n_values > start {
            self.ends[self.groups] = self.n_values as u8;
            self.groups += 1;
        }
    }
}
