//! The kitty graphics protocol: images sent in APC `G` commands, stored,
//! placed on the grid and deleted. Placements are anchored to absolute
//! lines, so they scroll with the text into scrollback; each screen
//! (primary, alternate) keeps its own images, as in kitty.
//!
//! Supported: transmission inline (`t=d`), from a file (`t=f`) or a temp
//! file (`t=t`), as RGB, RGBA or PNG (`f=24/32/100`), zlib-compressed or
//! not (`o=z`), in chunks (`m=1`); `a=t/T/p/q/d` with every delete
//! specifier; source rectangles, cell offsets, cell sizes and z-index;
//! Unicode placeholders (`U=1`), except placement ids in the underline
//! colour. Not yet: shared memory (`t=s`), animation and relative
//! placements, which are refused with an error.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cell::{Color, Flags};
use crate::{Term, base64};

/// The largest image side accepted, in pixels.
const MAX_SIDE: u32 = 10_000;
/// Image data a screen keeps, as decoded pixels; the oldest images go first
/// past it (kitty's default quota).
const MAX_STORED: usize = 320 << 20;
/// The largest transmission accepted, before decoding.
const MAX_TRANSMIT: usize = 400 << 20;
/// The most cells a placement may ask to cover each way.
const MAX_CELLS: u32 = 1000;
/// Images and placements a screen keeps; the oldest go first past them.
/// Lookups scan, and every placement is drawn each frame.
const MAX_IMAGES: usize = 4096;
const MAX_PLACEMENTS: usize = 4096;

/// Pixels of a transmitted image: premultiplied RGBA, top row first.
#[derive(Debug)]
pub struct ImageData {
    /// Unique across terminals and screens, for renderers' texture caches.
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

static SERIAL: AtomicU64 = AtomicU64::new(1);

struct Image {
    id: u32,
    /// `I=`: a number the client chose, answered with the id assigned.
    number: u32,
    data: Arc<ImageData>,
}

/// An image placed on the grid.
#[derive(Clone, Debug)]
pub struct Placement {
    pub image: Arc<ImageData>,
    image_id: u32,
    id: u32,
    /// Absolute line (see `Term::line`) of its top row; on the alternate
    /// screen, `lines_scrolled` plus the screen row.
    pub line: i64,
    pub col: usize,
    /// The part of the image shown: x, y, width, height in its pixels.
    pub source: [u32; 4],
    /// Pixels from the top-left of its first cell.
    pub offset: [u32; 2],
    /// Cells asked for (`c=`, `r=`; 0 for unset): the image is scaled to fill them.
    pub size: [u32; 2],
    /// Cells covered, columns and rows.
    pub cells: [u32; 2],
    /// Drawn below the text when negative.
    pub z: i32,
}

impl Placement {
    fn covers(&self, line: i64, col: usize) -> bool {
        (self.line..self.line + self.cells[1] as i64).contains(&line) && (self.col..self.col + self.cells[0] as usize).contains(&col)
    }
}

/// One screen's images and placements.
#[derive(Default)]
pub(crate) struct Graphics {
    /// Oldest first.
    images: Vec<Image>,
    placements: Vec<Placement>,
    /// Placed with `U=1`: shown only where text holds [`PLACEHOLDER`]s.
    virtuals: Vec<Placement>,
    /// A transmission arriving in chunks.
    loading: Option<Loading>,
    stored: usize,
    /// Ids assigned to images sent without one, counting down from the top.
    next_id: u32,
    /// Ids for placements sent without one.
    next_placement: u32,
}

struct Loading {
    command: Command,
    data: Vec<u8>,
    /// Base64 not yet decoded: less than a quantum of four.
    carry: Vec<u8>,
    too_big: bool,
}

impl Loading {
    fn push(&mut self, payload: &[u8]) {
        if self.too_big || self.data.len() + payload.len() > MAX_TRANSMIT {
            self.too_big = true;
            return;
        }
        self.carry.extend_from_slice(payload);
        let whole = self.carry.len() / 4 * 4;
        match base64::decode(&self.carry[..whole]) {
            Some(bytes) => self.data.extend(bytes),
            None => self.too_big = true,
        }
        self.carry.drain(..whole);
    }

    fn finish(mut self) -> Result<(Command, Vec<u8>), &'static str> {
        if !self.carry.is_empty() {
            match base64::decode(&self.carry) {
                Some(bytes) => self.data.extend(bytes),
                None => return Err("EINVAL:bad base64"),
            }
        }
        if self.too_big { Err("EFBIG:image data too large or damaged") } else { Ok((self.command, self.data)) }
    }
}

/// A command's keys; zero or the default where unset.
#[derive(Clone, Debug)]
struct Command {
    action: u8,
    quiet: u8,
    format: u32,
    medium: u8,
    compressed: bool,
    more: bool,
    /// `s`, `v`: pixel width and height of raw data.
    width: u32,
    height: u32,
    /// `S`, `O`: bytes to read from a file, and from where.
    read_size: u64,
    read_offset: u64,
    id: u32,
    number: u32,
    placement: u32,
    source: [u32; 4],
    offset: [u32; 2],
    cols: u32,
    rows: u32,
    cursor_stays: bool,
    unicode: bool,
    relative: bool,
    z: i32,
    delete: u8,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            action: b't',
            quiet: 0,
            format: 32,
            medium: b'd',
            compressed: false,
            more: false,
            width: 0,
            height: 0,
            read_size: 0,
            read_offset: 0,
            id: 0,
            number: 0,
            placement: 0,
            source: [0; 4],
            offset: [0; 2],
            cols: 0,
            rows: 0,
            cursor_stays: false,
            unicode: false,
            relative: false,
            z: 0,
            delete: b'a',
        }
    }
}

impl Command {
    /// `k=v,k=v`, single-letter keys; unknown keys are ignored, bad values
    /// are an error.
    fn parse(control: &[u8]) -> Option<Command> {
        let mut c = Command::default();
        for pair in control.split(|&b| b == b',').filter(|p| !p.is_empty()) {
            let [key, b'=', value @ ..] = pair else { return None };
            let number = || -> Option<u32> { std::str::from_utf8(value).ok()?.parse().ok() };
            let letter = || -> Option<u8> { (value.len() == 1).then(|| value[0]) };
            match *key {
                b'a' => c.action = letter()?,
                b'q' => c.quiet = number()? as u8,
                b'f' => c.format = number()?,
                b't' => c.medium = letter()?,
                b'o' => c.compressed = letter()? == b'z',
                b'm' => c.more = number()? == 1,
                b's' => c.width = number()?,
                b'v' => c.height = number()?,
                b'S' => c.read_size = number()?.into(),
                b'O' => c.read_offset = number()?.into(),
                b'i' => c.id = number()?,
                b'I' => c.number = number()?,
                b'p' => c.placement = number()?,
                b'x' => c.source[0] = number()?,
                b'y' => c.source[1] = number()?,
                b'w' => c.source[2] = number()?,
                b'h' => c.source[3] = number()?,
                b'X' => c.offset[0] = number()?,
                b'Y' => c.offset[1] = number()?,
                b'c' => c.cols = number()?,
                b'r' => c.rows = number()?,
                b'C' => c.cursor_stays = number()? == 1,
                b'U' => c.unicode = number()? == 1,
                b'P' | b'Q' => c.relative = true,
                b'z' => c.z = std::str::from_utf8(value).ok()?.parse().ok()?,
                b'd' => c.delete = letter()?,
                _ => {}
            }
        }
        Some(c)
    }
}

impl Graphics {
    pub(crate) fn placements(&self) -> &[Placement] {
        &self.placements
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    fn image(&self, id: u32) -> Option<&Image> {
        self.images.iter().find(|i| i.id == id)
    }

    /// The newest image sent with number `number`.
    fn numbered(&self, number: u32) -> Option<&Image> {
        self.images.iter().rev().find(|i| i.number == number)
    }

    fn store(&mut self, image: Image) {
        self.remove_image(image.id);
        self.stored += image.data.pixels.len();
        self.images.push(image);
        // Over quota: the oldest images not on screen go first, then any.
        let over = |g: &Self| g.stored > MAX_STORED || g.images.len() > MAX_IMAGES;
        if over(self) {
            let mut placed: HashSet<u32> = self.placements.iter().chain(&self.virtuals).map(|p| p.image_id).collect();
            while over(self) && self.images.len() > 1 {
                let unplaced = self.images.iter().position(|i| !placed.contains(&i.id));
                let id = self.images[unplaced.unwrap_or(0)].id;
                placed.remove(&id);
                self.remove_image(id);
            }
        }
    }

    fn remove_image(&mut self, id: u32) {
        if let Some(at) = self.images.iter().position(|i| i.id == id) {
            let image = self.images.remove(at);
            self.stored -= image.data.pixels.len();
        }
        self.placements.retain(|p| p.image_id != id);
        self.virtuals.retain(|p| p.image_id != id);
    }

    /// Counting down from the top, clear of the small ids clients pick.
    fn fresh_id(&mut self) -> u32 {
        loop {
            self.next_id = if self.next_id <= 1 { u32::MAX } else { self.next_id - 1 };
            if self.image(self.next_id).is_none() {
                return self.next_id;
            }
        }
    }

    /// Moves placements anchored in screen rows `top..=bottom` by `delta`
    /// rows, as the text there scrolls; those that leave the region go.
    pub(crate) fn scroll(&mut self, lines_scrolled: u64, top: usize, bottom: usize, delta: i64) {
        let (top, bottom) = (lines_scrolled as i64 + top as i64, lines_scrolled as i64 + bottom as i64);
        self.placements.retain_mut(|p| {
            if !(top..=bottom).contains(&p.line) {
                return true;
            }
            p.line += delta;
            p.line + p.cells[1] as i64 > top && p.line <= bottom
        });
    }

    /// Drops placements wholly above `line`: history that's gone.
    pub(crate) fn forget_above(&mut self, line: u64) {
        self.placements.retain(|p| p.line + p.cells[1] as i64 > line as i64);
    }

    /// Drops placements that touch lines `from..to`.
    pub(crate) fn clear_lines(&mut self, from: u64, to: u64) {
        let (from, to) = (from as i64, to as i64);
        self.placements.retain(|p| p.line + p.cells[1] as i64 <= from || p.line >= to);
    }
}

/// `ESC _G <keys>;<message> ESC \`, as the client asked to hear (`q=`):
/// only for commands naming an image.
fn reply(c: &Command, message: &str) -> Option<Vec<u8>> {
    let ok = message == "OK";
    if (c.id == 0 && c.number == 0) || (ok && c.quiet >= 1) || c.quiet >= 2 {
        return None;
    }
    let mut keys = format!("i={}", c.id);
    if c.number != 0 {
        keys += &format!(",I={}", c.number);
    }
    if c.placement != 0 {
        keys += &format!(",p={}", c.placement);
    }
    Some(format!("\x1b_G{keys};{message}\x1b\\").into_bytes())
}

/// `O_NOFOLLOW`: opening a symlink fails instead of opening its target.
/// `O_NONBLOCK`: opening a pipe doesn't wait for a writer.
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(not(target_os = "linux"))]
const O_NOFOLLOW: i32 = 0x0100;
#[cfg(not(target_os = "linux"))]
const O_NONBLOCK: i32 = 0x0004;

/// The user this process runs as.
fn current_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid takes nothing and always succeeds.
    unsafe { getuid() }
}

/// Every failure to read a file, or an image from it: output can come
/// from other machines (ssh), and the reply mustn't tell them whether a
/// file exists, how big it is or what it holds.
const FILE_ERROR: &str = "EBADF:can't read an image from the file";

/// Where `t=t` names a temp file as the protocol says (a name holding
/// `tty-graphics-protocol`, in a temp directory), the path to it with its
/// directory resolved, so `..` and symlinked directories can't lead out.
/// Output can come from other machines (ssh), so this guards the deletion.
fn temp_file(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return None;
    }
    let name = path.file_name()?;
    if !name.to_string_lossy().contains("tty-graphics-protocol") {
        return None;
    }
    let dir = fs::canonicalize(path.parent()?).ok()?;
    let temp_dirs = [std::env::temp_dir(), "/tmp".into(), "/var/tmp".into(), "/var/folders".into(), "/dev/shm".into()];
    temp_dirs.iter().filter_map(|t| fs::canonicalize(t).ok()).any(|t| dir.starts_with(t)).then(|| dir.join(name))
}

/// Reads what `t=f` or `t=t` names. Only regular files, opened without
/// waiting, so not devices or pipes, and nothing under /proc, /sys or
/// /dev; temp files only as `temp_file` allows, never through a symlink,
/// and deleted once read. Errors say nothing (see `FILE_ERROR`).
fn read_file(c: &Command, path: &[u8]) -> Option<Vec<u8>> {
    let path = PathBuf::from(std::str::from_utf8(path).ok()?);
    if !path.is_absolute() {
        return None;
    }
    let temp = c.medium == b't';
    let mut options = fs::OpenOptions::new();
    options.read(true);
    let path = if temp {
        options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
        temp_file(&path)?
    } else {
        // Resolved first, so a symlink can't lead into them.
        let path = fs::canonicalize(&path).ok()?;
        if ["/proc", "/sys", "/dev"].iter().any(|dir| path.starts_with(dir)) {
            return None;
        }
        options.custom_flags(O_NONBLOCK);
        path
    };
    let mut file = options.open(&path).ok()?;
    // Checked on what was opened, not the path, which could change meanwhile;
    // a temp file must be the user's, not one planted in a shared /tmp.
    let meta = file.metadata().ok()?;
    if !meta.is_file() || (temp && meta.uid() != current_uid()) {
        return None;
    }
    file.seek(SeekFrom::Start(c.read_offset)).ok()?;
    let size = if c.read_size > 0 { c.read_size } else { meta.len().saturating_sub(c.read_offset) };
    if size > MAX_TRANSMIT as u64 {
        return None;
    }
    let mut data = Vec::with_capacity(size as usize);
    file.take(size).read_to_end(&mut data).ok()?;
    if temp {
        let _ = fs::remove_file(&path);
    }
    Some(data)
}

/// A PNG's width and height, from its header: the chunk that must come first.
fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.get(..8)? != b"\x89PNG\r\n\x1a\n" || data.get(12..16)? != b"IHDR" {
        return None;
    }
    let be32 = |at: usize| Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?));
    Some((be32(16)?, be32(20)?))
}

/// Pixels from transmitted bytes, as the command's format says.
fn decode(c: &Command, mut data: Vec<u8>) -> Result<ImageData, &'static str> {
    let raw = matches!(c.format, 24 | 32);
    // Sizes are checked before anything is inflated, which they bound.
    if raw && (!(1..=MAX_SIDE).contains(&c.width) || !(1..=MAX_SIDE).contains(&c.height)) {
        return Err("EINVAL:bad image size");
    }
    // Over the quota on its own, an image would push out every other and
    // still not fit. A PNG's size is checked from its header, below.
    if raw && c.width as usize * c.height as usize * 4 > MAX_STORED {
        return Err("EFBIG:image too large");
    }
    if c.compressed {
        let limit = if raw { c.width as usize * c.height as usize * (c.format / 8) as usize } else { MAX_TRANSMIT };
        data = mightty_image::inflate::zlib(&data, limit).ok_or("EINVAL:bad zlib data")?;
    }
    let (width, height, pixels) = match c.format {
        24 | 32 => {
            let (w, h) = (c.width, c.height);
            let bpp = (c.format / 8) as usize;
            let n = w as usize * h as usize;
            if data.len() < n * bpp {
                return Err("ENODATA:not enough image data");
            }
            let mut pixels = Vec::with_capacity(n * 4);
            for px in data[..n * bpp].chunks_exact(bpp) {
                let a = if bpp == 4 { px[3] } else { 255 };
                let pre = |v: u8| ((v as u32 * a as u32 + 127) / 255) as u8;
                pixels.extend([pre(px[0]), pre(px[1]), pre(px[2]), a]);
            }
            (w, h, pixels)
        }
        100 => {
            if png_size(&data).is_none_or(|(w, h)| w as usize * h as usize * 4 > MAX_STORED) {
                return Err("EBADPNG:can't decode the PNG");
            }
            let image = mightty_image::png::decode_limited(&data, MAX_SIDE).ok_or("EBADPNG:can't decode the PNG")?;
            (image.width, image.height, image.pixels)
        }
        _ => return Err("EINVAL:unknown format"),
    };
    Ok(ImageData { serial: SERIAL.fetch_add(1, Ordering::Relaxed), width, height, pixels })
}

/// U+10EEEE: a cell showing part of an image placed with `U=1`. Its
/// foreground colour is the image id; diacritics after it give the row,
/// the column and the id's top byte, each as an index into [`DIACRITICS`].
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Combining characters standing for 0, 1, 2…, in code point order
/// (kitty's gen/rowcolumn-diacritics.txt).
static DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187,
    0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

fn diacritic(c: char) -> Option<u32> {
    DIACRITICS.binary_search(&(c as u32)).ok().map(|i| i as u32)
}

/// A run of placeholder cells in a row, showing one image.
#[derive(Clone, Debug)]
pub struct PlaceholderRun {
    pub image: Arc<ImageData>,
    /// Absolute line, and the column of the run's first cell.
    pub line: u64,
    pub col: usize,
    pub len: usize,
    /// The cell of the image's box the first cell shows: column, row.
    pub at: [u32; 2],
    /// The box, in cells, the image is fitted into, centred: columns, rows.
    pub cells: [u32; 2],
    /// The part of the image shown: x, y, width, height in its pixels.
    pub source: [u32; 4],
    pub z: i32,
}

impl Term {
    /// Placeholder runs in lines `first..first + rows` showing images
    /// placed with `U=1` on the active screen.
    pub fn placeholder_runs(&self, first: u64, rows: usize) -> Vec<PlaceholderRun> {
        let graphics = &self.graphics[usize::from(self.alt_active)];
        let mut runs: Vec<PlaceholderRun> = Vec::new();
        if graphics.virtuals.is_empty() {
            return runs;
        }
        for line in first..first + rows as u64 {
            let Some(row) = self.line(line) else { continue };
            // The cell before, if a placeholder: its colour, image id, row and column.
            let mut before: Option<(Color, u32, u32, u32)> = None;
            for (col, &cell) in row.cells.iter().enumerate() {
                let text = match self.graphemes.get(cell) {
                    Some(text) => text,
                    None if cell.c == PLACEHOLDER && !cell.flags().contains(Flags::GRAPHEME) => "",
                    None => {
                        before = None;
                        continue;
                    }
                };
                let mut chars = text.chars();
                if !text.is_empty() && chars.next() != Some(PLACEHOLDER) {
                    before = None;
                    continue;
                }
                let fg = self.style(cell).fg;
                let id = match fg {
                    Color::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
                    Color::Indexed(i) => i.into(),
                    Color::Default => 0,
                };
                // Left out, each continues the cell before: same row, next column.
                let follows = before.filter(|b| b.0 == fg);
                let mut marks = chars.map(diacritic);
                let (r, c, top) = (marks.next().flatten(), marks.next().flatten(), marks.next().flatten());
                let r = r.or(follows.map(|b| b.2)).unwrap_or(0);
                let c = c.or(follows.filter(|b| b.2 == r).map(|b| b.3 + 1)).unwrap_or(0);
                let id = top.map_or(follows.map_or(id, |b| b.1), |top| id | top << 24);
                before = Some((fg, id, r, c));
                let Some(p) = graphics.virtuals.iter().find(|p| p.image_id == id) else { continue };
                if r >= p.cells[1] || c >= p.cells[0] {
                    continue;
                }
                match runs.last_mut() {
                    Some(run)
                        if run.line == line
                            && run.col + run.len == col
                            && Arc::ptr_eq(&run.image, &p.image)
                            && c.checked_sub(run.len as u32) == Some(run.at[0])
                            && r == run.at[1]
                            && run.cells == p.cells =>
                    {
                        run.len += 1
                    }
                    _ => runs.push(PlaceholderRun {
                        image: p.image.clone(),
                        line,
                        col,
                        len: 1,
                        at: [c, r],
                        cells: p.cells,
                        source: p.source,
                        z: p.z,
                    }),
                }
            }
        }
        runs
    }
}

impl Term {
    /// Where a graphics command goes: the active screen's images.
    pub(crate) fn graphics_mut(&mut self) -> &mut Graphics {
        &mut self.graphics[usize::from(self.alt_active)]
    }

    /// An APC string: a graphics command if it starts with `G`.
    pub(crate) fn apc(&mut self, data: &[u8]) {
        let Some(command) = data.strip_prefix(b"G") else { return };
        let (control, payload) = match command.iter().position(|&b| b == b';') {
            Some(at) => (&command[..at], &command[at + 1..]),
            None => (command, &[][..]),
        };
        // Damaged commands are dropped: there's no image id to reply to.
        let parsed = Command::parse(control);
        let (mut loading, more) = match self.graphics_mut().loading.take() {
            // A chunk of a transmission underway: only `m` (and `q`) count.
            Some(loading) => (loading, parsed.is_some_and(|c| c.more)),
            None => {
                let Some(command) = parsed else { return };
                let more = command.more && matches!(command.action, b't' | b'T' | b'q');
                (Loading { command, data: Vec::new(), carry: Vec::new(), too_big: false }, more)
            }
        };
        loading.push(payload);
        if more {
            self.graphics_mut().loading = Some(loading);
            return;
        }
        let command = loading.command.clone();
        match loading.finish() {
            Ok((command, data)) => self.graphics_command(command, data),
            Err(error) => self.graphics_reply(&command, error),
        }
    }

    fn graphics_reply(&mut self, c: &Command, message: &str) {
        if let Some(bytes) = reply(c, message) {
            self.respond(&bytes);
        }
    }

    fn graphics_command(&mut self, mut c: Command, data: Vec<u8>) {
        match c.action {
            b't' | b'T' | b'q' => {
                let pixels = match c.medium {
                    b'd' => decode(&c, data),
                    // Any file can be named, so only as a whole, uncompressed
                    // PNG: raw pixels, offsets and zlib would tell a remote
                    // program about the file by whether they decode.
                    b'f' if c.format != 100 || c.compressed || c.read_offset != 0 || c.read_size != 0 => {
                        Err("EINVAL:a file must be sent as a whole, uncompressed PNG")
                    }
                    b'f' | b't' => read_file(&c, &data).and_then(|data| decode(&c, data).ok()).ok_or(FILE_ERROR),
                    b's' => Err("EINVAL:shared memory isn't supported"),
                    _ => Err("EINVAL:unknown transmission medium"),
                };
                let image = match pixels {
                    Ok(image) => image,
                    Err(error) => return self.graphics_reply(&c, error),
                };
                if c.action == b'q' {
                    return self.graphics_reply(&c, "OK");
                }
                let graphics = self.graphics_mut();
                let id = match c.id {
                    0 => graphics.fresh_id(),
                    id => id,
                };
                if c.id == 0 && c.number != 0 {
                    c.id = id;
                }
                graphics.store(Image { id, number: c.number, data: Arc::new(image) });
                if c.action == b'T' {
                    self.place(&c, id);
                } else {
                    self.graphics_reply(&c, "OK");
                }
            }
            b'p' => {
                let graphics = &self.graphics[usize::from(self.alt_active)];
                let found = match (c.id, c.number) {
                    (0, 0) => None,
                    (0, number) => graphics.numbered(number),
                    (id, _) => graphics.image(id),
                };
                match found.map(|i| i.id) {
                    Some(id) => {
                        c.id = id;
                        self.place(&c, id);
                    }
                    None => self.graphics_reply(&c, "ENOENT:no such image"),
                }
            }
            b'd' => self.delete_images(&c),
            b'f' | b'a' | b'c' => self.graphics_reply(&c, "EINVAL:animation isn't supported"),
            _ => self.graphics_reply(&c, "EINVAL:unknown action"),
        }
    }

    /// Shows image `id` at the cursor, which then moves past it unless `C=1`.
    fn place(&mut self, c: &Command, id: u32) {
        if c.relative {
            return self.graphics_reply(c, "EINVAL:relative placements aren't supported");
        }
        let (cw, ch) = self.cell_pixels;
        let (cursor_row, cursor_col, lines_scrolled) = (self.cursor.row, self.cursor.col, self.lines_scrolled);
        let graphics = self.graphics_mut();
        let Some(image) = graphics.image(id).map(|i| i.data.clone()) else {
            return self.graphics_reply(c, "ENOENT:no such image");
        };
        let [x, y, w, h] = c.source;
        let (x, y) = (x.min(image.width - 1), y.min(image.height - 1));
        let w = if w == 0 { image.width - x } else { w.min(image.width - x) };
        let h = if h == 0 { image.height - y } else { h.min(image.height - y) };
        // Offsets stay within the first cell; sizes within reason.
        let [ox, oy] = [c.offset[0].min(cw - 1), c.offset[1].min(ch - 1)];
        let (asked_cols, asked_rows) = (c.cols.min(MAX_CELLS), c.rows.min(MAX_CELLS));
        // Cells covered: as asked, or as the image's pixels need, keeping
        // its aspect when only one is given.
        let (cols, rows) = match (asked_cols, asked_rows) {
            (0, 0) => ((w + ox).div_ceil(cw), (h + oy).div_ceil(ch)),
            (cols, 0) => (cols, ((cols * cw) as u64 * h as u64 / w as u64).div_ceil(ch as u64) as u32),
            (0, rows) => (((rows * ch) as u64 * w as u64 / h as u64).div_ceil(cw as u64) as u32, rows),
            (cols, rows) => (cols, rows),
        };
        // An image far taller than wide asked for many columns would otherwise
        // need millions of rows, each a line for the cursor to move down.
        let (cols, rows) = (cols.clamp(1, MAX_CELLS), rows.clamp(1, MAX_CELLS));
        let placement_id = match c.placement {
            0 => {
                graphics.next_placement = graphics.next_placement.wrapping_add(1).max(1) | 1 << 31;
                graphics.next_placement
            }
            p => p,
        };
        let placements = if c.unicode { &mut graphics.virtuals } else { &mut graphics.placements };
        placements.retain(|p| !(p.image_id == id && p.id == placement_id));
        if placements.len() >= MAX_PLACEMENTS {
            placements.remove(0);
        }
        placements.push(Placement {
            image,
            image_id: id,
            id: placement_id,
            line: (lines_scrolled + cursor_row as u64) as i64,
            col: cursor_col,
            source: [x, y, w, h],
            offset: [ox, oy],
            size: [asked_cols, asked_rows],
            cells: [cols, rows],
            z: c.z,
        });
        self.graphics_reply(c, "OK");
        // Placeholders take the room, as the program prints them.
        if !c.cursor_stays && !c.unicode {
            for _ in 1..rows {
                self.index();
            }
            let col = cursor_col + cols as usize;
            self.cursor.col = col.min(self.cols - 1);
            self.cursor.pending_wrap = col >= self.cols;
        }
    }

    /// `a=d`: placements by the `d=` specifier; in capitals, the images
    /// left without placements too.
    fn delete_images(&mut self, c: &Command) {
        let (lines_scrolled, rows) = (self.lines_scrolled as i64, self.rows as i64);
        let cursor = (lines_scrolled + self.cursor.row as i64, self.cursor.col);
        let [x, y, ..] = c.source;
        let cell = (lines_scrolled + y as i64 - 1, (x as usize).wrapping_sub(1));
        let graphics = self.graphics_mut();
        let target = match (c.id, c.number) {
            (0, 0) => None,
            (0, number) => graphics.numbered(number).map(|i| i.id),
            (id, _) => Some(id),
        };
        let doomed = |p: &Placement| match c.delete.to_ascii_lowercase() {
            b'a' => p.line + p.cells[1] as i64 > lines_scrolled && p.line < lines_scrolled + rows,
            b'i' | b'n' => Some(p.image_id) == target && (c.placement == 0 || p.id == c.placement),
            b'c' => p.covers(cursor.0, cursor.1),
            b'p' => p.covers(cell.0, cell.1),
            b'q' => p.covers(cell.0, cell.1) && p.z == c.z,
            b'x' => (p.col..p.col + p.cells[0] as usize).contains(&cell.1),
            b'y' => (p.line..p.line + p.cells[1] as i64).contains(&cell.0),
            b'z' => p.z == c.z,
            b'r' => (x..=y).contains(&p.image_id),
            _ => false,
        };
        let mut touched: HashSet<u32> = graphics.placements.iter().filter(|p| doomed(p)).map(|p| p.image_id).collect();
        graphics.placements.retain(|p| !doomed(p));
        // Virtual placements have no place on the grid to match.
        if matches!(c.delete.to_ascii_lowercase(), b'a' | b'i' | b'n' | b'r' | b'z') {
            let gone = |p: &Placement| c.delete.eq_ignore_ascii_case(&b'a') || doomed(p);
            touched.extend(graphics.virtuals.iter().filter(|p| gone(p)).map(|p| p.image_id));
            graphics.virtuals.retain(|p| !gone(p));
        }
        if c.delete.is_ascii_uppercase() {
            if matches!(c.delete, b'I' | b'N') {
                touched.extend(target);
            }
            for id in touched {
                if !graphics.placements.iter().chain(&graphics.virtuals).any(|p| p.image_id == id) {
                    graphics.remove_image(id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 24×10 terminal with 10×20 pixel cells.
    fn term() -> Term {
        let mut term = Term::new(24, 10);
        term.set_cell_pixels(10, 20);
        term
    }

    fn send(term: &mut Term, control: &str, payload: &[u8]) -> String {
        term.feed(format!("\x1b_G{control};{}\x1b\\", base64::encode(payload)).as_bytes());
        String::from_utf8(term.take_responses()).unwrap()
    }

    fn rgba(width: u32, height: u32) -> Vec<u8> {
        [255, 0, 0, 255].repeat((width * height) as usize)
    }

    /// zlib framing around stored (uncompressed) DEFLATE blocks.
    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = if data.is_empty() { vec![&[]] } else { data.chunks(65535).collect() };
        for (i, block) in blocks.iter().enumerate() {
            z.push((i + 1 == blocks.len()) as u8);
            z.extend((block.len() as u16).to_le_bytes());
            z.extend((!(block.len() as u16)).to_le_bytes());
            z.extend(*block);
        }
        z.extend([0; 4]);
        z
    }

    /// An 8-bit RGBA PNG of `width × height` opaque green pixels.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let chunk = |kind: &[u8; 4], body: &[u8]| [&(body.len() as u32).to_be_bytes()[..], kind, body, &[0; 4]].concat();
        let mut ihdr = [width.to_be_bytes(), height.to_be_bytes()].concat();
        ihdr.extend([8, 6, 0, 0, 0]);
        let row = [&[0u8][..], &[0, 255, 0, 255].repeat(width as usize)].concat();
        let mut out = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
        out.extend(chunk(b"IHDR", &ihdr));
        out.extend(chunk(b"IDAT", &zlib(&row.repeat(height as usize))));
        out.extend(chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn transmit_and_reply() {
        let mut t = term();
        assert_eq!(send(&mut t, "a=t,f=32,s=2,v=1,i=7", &rgba(2, 1)), "\x1b_Gi=7;OK\x1b\\");
        assert!(t.image_placements().is_empty());
        // Quiet: no OK with q=1, nothing at all with q=2.
        assert_eq!(send(&mut t, "a=t,f=32,s=2,v=1,i=8,q=1", &rgba(2, 1)), "");
        assert_eq!(send(&mut t, "a=t,f=32,s=2,v=2,i=9,q=2", &rgba(2, 1)), "");
        assert_eq!(send(&mut t, "a=t,f=32,s=2,v=2,i=9", &rgba(2, 1)), "\x1b_Gi=9;ENODATA:not enough image data\x1b\\");
        // No id, no reply.
        assert_eq!(send(&mut t, "a=t,f=32,s=2,v=1", &rgba(2, 1)), "");
        assert_eq!(send(&mut t, "a=p,i=99", &[]), "\x1b_Gi=99;ENOENT:no such image\x1b\\");
        assert_eq!(send(&mut t, "a=t,t=s,i=3", b"/name"), "\x1b_Gi=3;EINVAL:shared memory isn't supported\x1b\\");
        assert_eq!(send(&mut t, "a=q,f=100,i=31", &png(3, 2)), "\x1b_Gi=31;OK\x1b\\");
        assert_eq!(send(&mut t, "a=q,f=100,i=31", b"not a png"), "\x1b_Gi=31;EBADPNG:can't decode the PNG\x1b\\");
        // A query stores nothing.
        assert_eq!(send(&mut t, "a=p,i=31", &[]), "\x1b_Gi=31;ENOENT:no such image\x1b\\");
    }

    #[test]
    fn place_moves_the_cursor_past_the_image() {
        let mut t = term();
        t.feed(b"ab");
        // 25×45 pixels: 3 columns and 3 rows of 10×20 cells.
        assert_eq!(send(&mut t, "a=T,f=24,s=25,v=45,i=1", &[9; 25 * 45 * 3]), "\x1b_Gi=1;OK\x1b\\");
        let p = &t.image_placements()[0];
        assert_eq!((p.line, p.col, p.cells, p.source), (0, 2, [3, 3], [0, 0, 25, 45]));
        assert_eq!(p.image.pixels[..4], [9, 9, 9, 255]);
        assert_eq!(t.cursor(), (2, 5));
        // C=1 leaves it; c= and r= set the cells, the other kept in proportion.
        send(&mut t, "a=p,i=1,C=1,c=6", &[]);
        assert_eq!(t.image_placements()[1].cells, [6, 6]);
        assert_eq!(t.cursor(), (2, 5));
        send(&mut t, "a=p,i=1,C=1,r=1,c=2,p=4", &[]);
        assert_eq!(t.image_placements()[2].cells, [2, 1]);
        // The same placement id replaces it; 40 pixels high is 22 wide, 3 columns.
        send(&mut t, "a=p,i=1,C=1,r=2,p=4", &[]);
        assert_eq!(t.image_placements().len(), 3);
        assert_eq!(t.image_placements()[2].cells, [3, 2]);
    }

    #[test]
    fn limits() {
        let mut t = term();
        // Sizes are checked before inflating, and data can't inflate past them.
        assert_eq!(send(&mut t, "a=t,f=32,s=99999,v=99999,o=z,i=1", &zlib(&[0; 64])), "\x1b_Gi=1;EINVAL:bad image size\x1b\\");
        assert_eq!(send(&mut t, "a=t,f=32,s=2,v=2,o=z,i=2", &zlib(&rgba(4, 4))), "\x1b_Gi=2;EINVAL:bad zlib data\x1b\\");
        // A 1×10000 image 1000 columns wide: capped at 1000 rows, not millions.
        let start = std::time::Instant::now();
        assert_eq!(send(&mut t, "a=T,f=32,s=1,v=10000,c=1000,i=3", &rgba(1, 10000)), "\x1b_Gi=3;OK\x1b\\");
        assert_eq!(t.image_placements()[0].cells, [1000, 1000]);
        assert!(start.elapsed().as_secs() < 2);
        // One image can't take the whole quota, raw or PNG.
        assert_eq!(send(&mut t, "a=t,f=32,s=10000,v=10000,i=4", b""), "\x1b_Gi=4;EFBIG:image too large\x1b\\");
        let mut huge = png(1, 1);
        huge[16..24].copy_from_slice(&[0, 0, 0x27, 0x10, 0, 0, 0x27, 0x10]);
        assert_eq!(send(&mut t, "a=t,f=100,i=5", &huge), "\x1b_Gi=5;EBADPNG:can't decode the PNG\x1b\\");
    }

    #[test]
    fn unicode_placeholders() {
        let mut t = term();
        // A 2×2-cell virtual placement: nothing drawn, the cursor stays.
        assert_eq!(send(&mut t, "a=T,U=1,f=32,s=4,v=4,c=2,r=2,i=7", &rgba(4, 4)), "\x1b_Gi=7;OK\x1b\\");
        assert!(t.image_placements().is_empty());
        assert_eq!(t.cursor(), (0, 0));
        // Row and column diacritics, or left out after the first cell;
        // image 7 as a 256-colour index, then as RGB with a top byte.
        t.feed("\x1b[38;5;7m\u{10EEEE}\u{305}\u{305}\u{10EEEE}\r\n\u{10EEEE}\u{30D}\u{10EEEE}\u{30D}\u{30D}".as_bytes());
        t.feed("\x1b[38;2;0;0;7m x\u{10EEEE}\u{305}\u{305}\u{305}".as_bytes());
        let runs = t.placeholder_runs(0, 3);
        let shape: Vec<_> = runs.iter().map(|r| (r.line, r.col, r.len, r.at)).collect();
        assert_eq!(shape, [(0, 0, 2, [0, 0]), (1, 0, 2, [0, 1]), (1, 4, 1, [0, 0])]);
        assert_eq!(runs[0].cells, [2, 2]);
        // Past the box, or another image: nothing.
        t.feed("\r\n\x1b[38;5;7m\u{10EEEE}\u{30E}\x1b[38;5;8m\u{10EEEE}".as_bytes());
        assert!(t.placeholder_runs(2, 1).is_empty());
        // Every diacritic joins its placeholder.
        assert!(DIACRITICS.iter().all(|&d| crate::char_width(char::from_u32(d).unwrap()) == 0));
        // Deleting the image takes the virtual placement with it.
        send(&mut t, "a=d,d=I,i=7", b"");
        assert!(t.placeholder_runs(0, 3).is_empty());
    }

    #[test]
    fn counts_are_capped() {
        let mut t = term();
        let start = std::time::Instant::now();
        let pixel = rgba(1, 1);
        for i in 1..=MAX_IMAGES as u32 + 10 {
            send(&mut t, &format!("a=t,f=32,s=1,v=1,i={i},q=2"), &pixel);
        }
        for _ in 0..MAX_PLACEMENTS + 10 {
            send(&mut t, "a=p,i=4100,C=1,q=2", b"");
        }
        let graphics = t.graphics_mut();
        assert_eq!(graphics.images.len(), MAX_IMAGES);
        assert!(graphics.image(1).is_none() && graphics.image(MAX_IMAGES as u32 + 10).is_some());
        assert_eq!(graphics.placements.len(), MAX_PLACEMENTS);
        assert!(start.elapsed().as_secs() < 5);
    }

    #[test]
    fn chunks_compression_and_png() {
        let mut t = term();
        let data = base64::encode(&rgba(4, 4));
        let (a, b) = data.split_at(20);
        t.feed(format!("\x1b_Ga=T,f=32,s=4,v=4,i=2,m=1;{a}\x1b\\\x1b_Gm=1;{}\x1b\\\x1b_Gm=0;{}\x1b\\", &b[..8], &b[8..]).as_bytes());
        assert_eq!(t.take_responses(), b"\x1b_Gi=2;OK\x1b\\");
        assert_eq!(t.image_placements()[0].image.pixels, rgba(4, 4));
        assert_eq!(send(&mut t, "a=T,f=32,s=4,v=4,o=z,i=3", &zlib(&rgba(4, 4))), "\x1b_Gi=3;OK\x1b\\");
        assert_eq!(send(&mut t, "a=T,f=100,i=4", &png(30, 20)), "\x1b_Gi=4;OK\x1b\\");
        let p = &t.image_placements()[2];
        assert_eq!((p.image.width, p.image.height, p.cells), (30, 20, [3, 1]));
        assert_eq!(p.image.pixels[..4], [0, 255, 0, 255]);
    }

    #[test]
    fn numbers_get_ids() {
        let mut t = term();
        let reply = send(&mut t, "a=t,f=32,s=1,v=1,I=5", &rgba(1, 1));
        let id = u32::MAX;
        assert_eq!(reply, format!("\x1b_Gi={id},I=5;OK\x1b\\"));
        assert_eq!(send(&mut t, "a=p,I=5,p=2", &[]), format!("\x1b_Gi={id},I=5,p=2;OK\x1b\\"));
        assert_eq!(t.image_placements().len(), 1);
    }

    #[test]
    fn files() {
        let dir = std::env::temp_dir();
        let error = |id: u32| format!("\x1b_Gi={id};{FILE_ERROR}\x1b\\");
        let plain = dir.join(format!("mightty-graphics-test-{}.png", std::process::id()));
        fs::write(&plain, png(2, 2)).unwrap();
        let mut t = term();
        let path = plain.to_str().unwrap().as_bytes();
        assert_eq!(send(&mut t, "a=T,t=f,f=100,i=1", path), "\x1b_Gi=1;OK\x1b\\");
        assert_eq!(t.image_placements()[0].image.pixels, [0, 255, 0, 255].repeat(4));
        // Any file can be named, so only as a PNG: raw pixels or zlib would
        // tell a remote program what the file holds by whether they decode.
        let refused = "\x1b_Gi=8;EINVAL:a file must be sent as a whole, uncompressed PNG\x1b\\";
        assert_eq!(send(&mut t, "a=t,t=f,f=32,s=1,v=1,i=8", path), refused);
        assert_eq!(send(&mut t, "a=t,t=f,f=100,o=z,i=8", path), refused);
        assert_eq!(send(&mut t, "a=t,t=f,f=100,O=8,i=8", path), refused);
        assert_eq!(send(&mut t, "a=t,t=f,f=100,S=8,i=8", path), refused);
        // A temp file must be named for the protocol, and is deleted once read.
        assert_eq!(send(&mut t, "a=t,t=t,f=32,s=2,v=2,O=8,i=2", path), error(2));
        assert!(plain.exists());
        fs::remove_file(&plain).unwrap();
        let temp = dir.join(format!("tty-graphics-protocol-mightty-{}", std::process::id()));
        fs::write(&temp, rgba(2, 2)).unwrap();
        assert_eq!(send(&mut t, "a=t,t=t,f=32,s=2,v=2,i=3", temp.to_str().unwrap().as_bytes()), "\x1b_Gi=3;OK\x1b\\");
        assert!(!temp.exists());
        // Out of the temp directory by `..`, or through a symlink: refused, nothing deleted.
        let outside = std::env::current_dir().unwrap().join(format!("tty-graphics-protocol-outside-{}", std::process::id()));
        fs::write(&outside, rgba(2, 2)).unwrap();
        let sneaky = format!("{}/../{}", dir.display(), outside.strip_prefix("/").unwrap().display());
        let up = "/..".repeat(dir.components().count());
        let sneaky = sneaky.replacen("/../", &format!("{up}/"), 1);
        assert_eq!(send(&mut t, "a=t,t=t,f=32,s=2,v=2,i=6", sneaky.as_bytes()), error(6));
        let link = dir.join(format!("tty-graphics-protocol-link-{}", std::process::id()));
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_eq!(send(&mut t, "a=t,t=t,f=32,s=2,v=2,i=7", link.to_str().unwrap().as_bytes()), error(7));
        assert!(outside.exists());
        fs::remove_file(&link).unwrap();
        fs::remove_file(&outside).unwrap();
        // Missing, not a file, not an image, a link into /dev: the same answer.
        assert_eq!(send(&mut t, "a=t,t=f,f=100,i=4", b"/dev/null"), error(4));
        let to_dev = dir.join(format!("mightty-graphics-dev-{}", std::process::id()));
        let _ = fs::remove_file(&to_dev);
        std::os::unix::fs::symlink("/dev/zero", &to_dev).unwrap();
        assert_eq!(send(&mut t, "a=t,t=f,f=100,i=4", to_dev.to_str().unwrap().as_bytes()), error(4));
        fs::remove_file(&to_dev).unwrap();
        assert_eq!(send(&mut t, "a=t,t=f,f=100,i=5", b"/nonexistent/x"), error(5));
        assert_eq!(send(&mut t, "a=t,t=f,f=100,i=5", b"/etc/hosts"), error(5));
        // A pipe with no writer is refused at once, not waited on.
        let fifo = dir.join(format!("mightty-graphics-fifo-{}", std::process::id()));
        let _ = fs::remove_file(&fifo);
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        assert_eq!(send(&mut t, "a=t,t=f,f=100,i=9", fifo.to_str().unwrap().as_bytes()), error(9));
        fs::remove_file(&fifo).unwrap();
    }

    #[test]
    fn delete() {
        let mut t = term();
        send(&mut t, "a=t,f=32,s=1,v=1,i=1", &rgba(1, 1));
        send(&mut t, "a=t,f=32,s=1,v=1,i=2", &rgba(1, 1));
        send(&mut t, "a=p,i=1,p=1,C=1", &[]);
        send(&mut t, "a=p,i=1,p=2,C=1,z=-1", &[]);
        t.feed(b"\x1b[3;5H");
        send(&mut t, "a=p,i=2,C=1", &[]);
        send(&mut t, "a=d,d=p,x=5,y=3", &[]);
        assert_eq!(t.image_placements().len(), 2);
        send(&mut t, "a=d,d=z,z=-1", &[]);
        assert_eq!(t.image_placements().len(), 1);
        // Lowercase keeps the image to place again; capitals free it.
        send(&mut t, "a=d,d=i,i=1", &[]);
        assert_eq!(send(&mut t, "a=p,i=1,C=1", &[]), "\x1b_Gi=1;OK\x1b\\");
        send(&mut t, "a=d,d=I,i=1", &[]);
        assert_eq!(send(&mut t, "a=p,i=1", &[]), "\x1b_Gi=1;ENOENT:no such image\x1b\\");
        send(&mut t, "a=p,i=2,C=1", &[]);
        send(&mut t, "a=d", &[]);
        assert!(t.image_placements().is_empty());
        assert_eq!(send(&mut t, "a=p,i=2,C=1", &[]), "\x1b_Gi=2;OK\x1b\\");
    }

    #[test]
    fn images_scroll_with_text() {
        let mut t = Term::with_scrollback(24, 10, 5);
        t.set_cell_pixels(10, 20);
        send(&mut t, "a=T,f=32,s=1,v=1,i=1", &rgba(1, 1));
        t.feed(&b"\n".repeat(12));
        // Scrolled into history, still placed at its line.
        assert_eq!(t.image_placements()[0].line, 0);
        assert_eq!((t.first_line(), t.lines_scrolled()), (0, 3));
        // History past the limit takes it.
        t.feed(&b"\n".repeat(5));
        assert!(t.image_placements().is_empty());

        // Clearing the screen clears its images; ⌘K's history too.
        send(&mut t, "a=T,f=32,s=1,v=1,i=1", &rgba(1, 1));
        t.feed(b"\x1b[2J");
        assert!(t.image_placements().is_empty());
        send(&mut t, "a=T,f=32,s=1,v=1,i=1", &rgba(1, 1));
        t.feed(b"\n\n");
        t.clear_above_cursor();
        assert!(t.image_placements().is_empty());
    }

    #[test]
    fn alternate_screen_has_its_own() {
        let mut t = term();
        send(&mut t, "a=T,f=32,s=1,v=1,i=1", &rgba(1, 1));
        t.feed(b"\x1b[?1049h");
        assert!(t.image_placements().is_empty());
        assert_eq!(send(&mut t, "a=p,i=1", &[]), "\x1b_Gi=1;ENOENT:no such image\x1b\\");
        send(&mut t, "a=T,f=32,s=1,v=1,i=1", &rgba(1, 1));
        // Scrolling the alternate screen moves its images; off the top, they go.
        t.feed(b"\x1b[3;1H");
        send(&mut t, "a=T,f=32,s=1,v=1,i=1,p=2", &rgba(1, 1));
        t.feed(b"\x1b[10;1H\n");
        let lines: Vec<i64> = t.image_placements().iter().map(|p| p.line).collect();
        assert_eq!(lines, [1]);
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.image_placements().len(), 1);
        t.feed(b"\x1b[?1049h");
        assert!(t.image_placements().is_empty());
    }

    #[test]
    fn size_reports() {
        let mut t = term();
        t.feed(b"\x1b[16t\x1b[14t\x1b[18t");
        assert_eq!(t.take_responses(), b"\x1b[6;20;10t\x1b[4;200;240t\x1b[8;10;24t");
    }
}
