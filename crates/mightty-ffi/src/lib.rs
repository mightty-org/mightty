//! libmightty: the C ABI of the terminal core, declared in `include/mightty.h`.
//!
//! The header is written by hand and is the contract; `tests/c_api.rs`
//! compiles and runs a C program against it so the two can't drift apart.
//! Every entry point catches panics so none unwinds into C.

#![allow(non_camel_case_types)]

use std::ffi::c_char;
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

// `::` because the opaque handle below shares the crate's name.
use ::mightty_term::{Color, MarkKind, MouseMode, Term};

pub type mightty_result = i32;
pub const MIGHTTY_OK: mightty_result = 0;
pub const MIGHTTY_ERR_NULL: mightty_result = 1;
pub const MIGHTTY_ERR_INVALID: mightty_result = 2;
pub const MIGHTTY_ERR_BUFFER: mightty_result = 3;
pub const MIGHTTY_ERR_INTERNAL: mightty_result = 4;

const ABI_VERSION: u32 = 1;

/// Opaque handle.
pub struct mightty_term(Term);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct mightty_cell {
    pub codepoint: u32,
    pub fg: u32,
    pub bg: u32,
    pub flags: u16,
    pub fg_kind: u8,
    pub bg_kind: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mightty_state {
    pub size: u32,
    pub cols: u32,
    pub rows: u32,
    pub cursor_col: u32,
    pub cursor_row: u32,
    pub scrollback_len: u32,
    pub lines_scrolled: u64,
    pub first_line: u64,
    pub cursor_visible: u8,
    pub alt_screen: u8,
    pub app_cursor: u8,
    pub app_keypad: u8,
    pub bracketed_paste: u8,
    pub focus_events: u8,
    pub mouse_mode: u8,
    pub mouse_sgr: u8,
    pub sync_output: u8,
    pub cursor_shape: u8,
    pub _reserved: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct mightty_mark {
    pub line: u64,
    pub kind: u32,
    pub exit_code: i32,
    pub has_exit_code: u8,
    pub _reserved: [u8; 7],
}

// Layouts promised by the header.
const _: () = assert!(mem::size_of::<mightty_cell>() == 16);
const _: () = assert!(mem::size_of::<mightty_state>() == 56);
const _: () = assert!(mem::size_of::<mightty_mark>() == 24);

/// Runs `f`, turning a panic into `MIGHTTY_ERR_INTERNAL`.
fn guard(f: impl FnOnce() -> mightty_result) -> mightty_result {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(MIGHTTY_ERR_INTERNAL)
}

/// The (out, cap, len) protocol: always report the size, copy only if it fits.
///
/// # Safety
/// `out` must be valid for `cap` writes of `T`; `len` must be valid or null.
unsafe fn copy_out<T: Copy>(src: &[T], out: *mut T, cap: usize, len: *mut usize) -> mightty_result {
    if len.is_null() {
        return MIGHTTY_ERR_NULL;
    }
    // SAFETY: checked non-null; validity is the caller's contract.
    unsafe { *len = src.len() };
    if src.len() > cap {
        return MIGHTTY_ERR_BUFFER;
    }
    if !src.is_empty() {
        if out.is_null() {
            return MIGHTTY_ERR_NULL;
        }
        // SAFETY: `out` holds at least `cap >= src.len()` elements.
        unsafe { ptr::copy_nonoverlapping(src.as_ptr(), out, src.len()) };
    }
    MIGHTTY_OK
}

/// # Safety
/// `term` must be null or a live handle from `mightty_term_new`.
unsafe fn term_ref<'a>(term: *const mightty_term) -> Option<&'a Term> {
    // SAFETY: caller contract.
    unsafe { term.as_ref() }.map(|t| &t.0)
}

/// # Safety
/// As `term_ref`, with no other reference to the handle alive.
unsafe fn term_mut<'a>(term: *mut mightty_term) -> Option<&'a mut Term> {
    // SAFETY: caller contract.
    unsafe { term.as_mut() }.map(|t| &mut t.0)
}

fn color(c: Color) -> (u8, u32) {
    match c {
        Color::Default => (0, 0),
        Color::Indexed(i) => (1, i.into()),
        Color::Rgb(r, g, b) => (2, u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn mightty_abi_version() -> u32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn mightty_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// The most columns or rows a terminal may have: past this, allocating
/// the grid could abort the process, which no error code can report.
const MAX_SIDE: u32 = 10_000;

#[unsafe(no_mangle)]
pub extern "C" fn mightty_term_new(
    cols: u32,
    rows: u32,
    scrollback_lines: u32,
) -> *mut mightty_term {
    if cols > MAX_SIDE || rows > MAX_SIDE {
        return ptr::null_mut();
    }
    catch_unwind(|| {
        let mut term =
            Term::with_scrollback(cols as usize, rows as usize, scrollback_lines as usize);
        // As the header promises: no I/O, so kitty images come only inline.
        term.set_image_files(false);
        Box::into_raw(Box::new(mightty_term(term)))
    })
    .unwrap_or(ptr::null_mut())
}

/// # Safety
/// `term` must be null or a handle from `mightty_term_new` not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_free(term: *mut mightty_term) {
    if !term.is_null() {
        // SAFETY: caller contract; ownership returns to Rust here.
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(term) })));
    }
}

/// # Safety
/// `term` must be a live handle; `bytes` must be valid for `len` reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_feed(
    term: *mut mightty_term,
    bytes: *const u8,
    len: usize,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_mut(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        if len == 0 {
            return MIGHTTY_OK;
        }
        if bytes.is_null() {
            return MIGHTTY_ERR_NULL;
        }
        // SAFETY: caller contract.
        term.feed(unsafe { std::slice::from_raw_parts(bytes, len) });
        MIGHTTY_OK
    })
}

/// # Safety
/// `term` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_resize(
    term: *mut mightty_term,
    cols: u32,
    rows: u32,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_mut(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        if cols > MAX_SIDE || rows > MAX_SIDE {
            return MIGHTTY_ERR_INVALID;
        }
        term.resize(cols as usize, rows as usize);
        MIGHTTY_OK
    })
}

/// # Safety
/// `term` must be a live handle; buffers follow the (out, cap, len) protocol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_take_responses(
    term: *mut mightty_term,
    out: *mut u8,
    cap: usize,
    len: *mut usize,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_mut(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        // SAFETY: caller contract.
        let result = unsafe { copy_out(term.pending_responses(), out, cap, len) };
        if result == MIGHTTY_OK {
            term.take_responses();
        }
        result
    })
}

/// # Safety
/// `term` must be a live handle; `rang` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_take_bell(
    term: *mut mightty_term,
    rang: *mut u8,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_mut(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        if rang.is_null() {
            return MIGHTTY_ERR_NULL;
        }
        // SAFETY: checked non-null.
        unsafe { *rang = term.take_bell().into() };
        MIGHTTY_OK
    })
}

/// # Safety
/// `term` must be a live handle; `out` must point to at least `out->size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_get_state(
    term: *const mightty_term,
    out: *mut mightty_state,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_ref(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        if out.is_null() {
            return MIGHTTY_ERR_NULL;
        }
        // SAFETY: `size` is the first field; every version has it.
        let size = unsafe { (*out).size } as usize;
        if size < mem::size_of::<u32>() {
            return MIGHTTY_ERR_INVALID;
        }
        let (row, col) = term.cursor();
        let m = term.modes();
        // Older callers get the prefix they know about, and `size` says
        // how much was filled, for newer callers of an older library.
        let n = size.min(mem::size_of::<mightty_state>());
        let state = mightty_state {
            size: n as u32,
            cols: term.cols() as u32,
            rows: term.rows() as u32,
            cursor_col: col as u32,
            cursor_row: row as u32,
            scrollback_len: term.scrollback().len() as u32,
            lines_scrolled: term.lines_scrolled(),
            first_line: term.first_line(),
            cursor_visible: m.show_cursor.into(),
            alt_screen: term.is_alt_screen().into(),
            app_cursor: m.app_cursor.into(),
            app_keypad: m.app_keypad.into(),
            bracketed_paste: m.bracketed_paste.into(),
            focus_events: m.focus_events.into(),
            mouse_mode: match m.mouse {
                MouseMode::Off => 0,
                MouseMode::Click => 1,
                MouseMode::Drag => 2,
                MouseMode::Motion => 3,
            },
            mouse_sgr: m.mouse_sgr.into(),
            sync_output: m.sync_output.into(),
            cursor_shape: m.cursor_shape.min(255) as u8,
            _reserved: [0; 6],
        };
        // SAFETY: the caller promised `size` writable bytes.
        unsafe {
            ptr::copy_nonoverlapping(
                (&state as *const mightty_state).cast::<u8>(),
                out.cast::<u8>(),
                n,
            )
        };
        MIGHTTY_OK
    })
}

/// # Safety
/// `term` must be a live handle; buffers follow the (out, cap, len) protocol;
/// `wrapped` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_read_line(
    term: *const mightty_term,
    line: u64,
    out: *mut mightty_cell,
    cap: usize,
    len: *mut usize,
    wrapped: *mut u8,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_ref(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        let Some(row) = term.line(line) else {
            return MIGHTTY_ERR_INVALID;
        };
        // Rows are stored lazily short; report at least the screen width.
        let width = row.cells.len().max(term.cols());
        let cells: Vec<mightty_cell> = (0..width)
            .map(|col| term.resolve(*row.cell(col)))
            .map(|cell| {
                let (fg_kind, fg) = color(cell.fg);
                let (bg_kind, bg) = color(cell.bg);
                mightty_cell {
                    codepoint: cell.c.into(),
                    fg,
                    bg,
                    flags: cell.flags.bits(),
                    fg_kind,
                    bg_kind,
                }
            })
            .collect();
        if !wrapped.is_null() {
            // SAFETY: checked non-null.
            unsafe { *wrapped = row.wrapped.into() };
        }
        // SAFETY: caller contract.
        unsafe { copy_out(&cells, out, cap, len) }
    })
}

/// # Safety
/// `term` must be a live handle; buffers follow the (out, cap, len) protocol.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mightty_term_text_range(
    term: *const mightty_term,
    start_line: u64,
    start_col: u32,
    end_line: u64,
    end_col: u32,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_ref(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        let text = term.text_range(
            (start_line, start_col as usize),
            (end_line, end_col as usize),
        );
        // SAFETY: caller contract.
        unsafe { copy_out(text.as_bytes(), out.cast(), cap, len) }
    })
}

/// Shared body of the string getters.
///
/// # Safety
/// As the getters.
unsafe fn string_getter(
    term: *const mightty_term,
    get: impl FnOnce(&Term) -> String,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_ref(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        let text = get(term);
        // SAFETY: caller contract.
        unsafe { copy_out(text.as_bytes(), out.cast(), cap, len) }
    })
}

/// # Safety
/// `term` must be a live handle; buffers follow the (out, cap, len) protocol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_screen_text(
    term: *const mightty_term,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> mightty_result {
    // SAFETY: forwarded caller contract.
    unsafe { string_getter(term, Term::screen_text, out, cap, len) }
}

/// # Safety
/// `term` must be a live handle; buffers follow the (out, cap, len) protocol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_title(
    term: *const mightty_term,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> mightty_result {
    // SAFETY: forwarded caller contract.
    unsafe { string_getter(term, |t| t.title().to_owned(), out, cap, len) }
}

/// # Safety
/// `term` must be a live handle; buffers follow the (out, cap, len) protocol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_cwd(
    term: *const mightty_term,
    out: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> mightty_result {
    // SAFETY: forwarded caller contract.
    unsafe {
        string_getter(
            term,
            |t| t.cwd().unwrap_or_default().to_owned(),
            out,
            cap,
            len,
        )
    }
}

/// # Safety
/// `term` must be a live handle; buffers follow the (out, cap, len) protocol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mightty_term_marks(
    term: *const mightty_term,
    out: *mut mightty_mark,
    cap: usize,
    len: *mut usize,
) -> mightty_result {
    guard(|| {
        // SAFETY: caller contract.
        let Some(term) = (unsafe { term_ref(term) }) else {
            return MIGHTTY_ERR_NULL;
        };
        let marks: Vec<mightty_mark> = term
            .marks()
            .iter()
            .map(|m| {
                let (kind, exit_code) = match m.kind {
                    MarkKind::PromptStart => (0, None),
                    MarkKind::CommandStart => (1, None),
                    MarkKind::OutputStart => (2, None),
                    MarkKind::CommandEnd { exit_code } => (3, exit_code),
                };
                mightty_mark {
                    line: m.line,
                    kind,
                    exit_code: exit_code.unwrap_or(0),
                    has_exit_code: exit_code.is_some().into(),
                    _reserved: [0; 7],
                }
            })
            .collect();
        // SAFETY: caller contract.
        unsafe { copy_out(&marks, out, cap, len) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_bounded() {
        assert!(mightty_term_new(u32::MAX, 24, 0).is_null());
        let term = mightty_term_new(0, 0, u32::MAX);
        assert!(!term.is_null());
        // SAFETY: a live handle, freed once.
        unsafe {
            assert_eq!(mightty_term_resize(term, 80, u32::MAX), MIGHTTY_ERR_INVALID);
            assert_eq!(mightty_term_resize(term, MAX_SIDE, 1), MIGHTTY_OK);
            mightty_term_free(term);
        }
    }
}
