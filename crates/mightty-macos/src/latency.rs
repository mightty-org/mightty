//! Keypress-to-screen latency probe (`MIGHTTY_KEY_LATENCY=path`), for
//! developers. Types keys into the first window through the app's event
//! queue and times each one until the first frame showing its echo is on
//! screen, by Metal's presented time. Not covered: the keyboard and the
//! window server before the event reaches the app.
//!
//! The program in the window must print each key it reads in the
//! bottom-left cell, and a `.` there once it's ready: `bench/keys.py`.

use std::cell::RefCell;
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::ffi::*;
use crate::renderer::Snapshot;

const KEYS: usize = 200;
/// A key not echoed by then counts as lost.
const TIMEOUT: f64 = 1.0;

struct Probe {
    out: PathBuf,
    window: Id,
    /// The key in flight and when it was posted.
    sent: Option<(char, f64)>,
    /// The program has drawn its ready mark.
    ready: bool,
    typed: usize,
    lost: usize,
    rng: u64,
}

thread_local! {
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
}

/// Key time and frame time of the frame awaiting its presented time.
static WATCHED: Mutex<Option<(f64, f64)>> = Mutex::new(None);
/// Per key: milliseconds from the key to its frame being built, and to it
/// being on screen (NaN: that frame was dropped).
static RESULTS: Mutex<Vec<(f64, f64)>> = Mutex::new(Vec::new());

static PRESENTED: Block = Block::new(presented);

unsafe extern "C" fn presented(_: *const Block, drawable: Id) {
    // SAFETY: `drawable` is the MTLDrawable this handler was added to.
    let shown: f64 = unsafe { msg!(drawable, c"presentedTime") };
    if let Some((key, frame)) = WATCHED.lock().unwrap().take() {
        let on_screen = if shown > 0.0 { (shown - key) * 1e3 } else { f64::NAN };
        RESULTS.lock().unwrap().push(((frame - key) * 1e3, on_screen));
    }
}

/// Starts the probe on `window` if `MIGHTTY_KEY_LATENCY` is set.
pub fn start(window: Id) {
    let Some(out) = std::env::var_os("MIGHTTY_KEY_LATENCY") else { return };
    // SAFETY: plain C call.
    let rng = unsafe { CACurrentMediaTime() }.to_bits() | 1;
    PROBE.with(|p| *p.borrow_mut() = Some(Probe { out: out.into(), window, sent: None, ready: false, typed: 0, lost: 0, rng }));
    run_on_main_after(std::time::Duration::from_millis(500), tick);
}

/// Called with each frame of `window` before it's presented; returns the
/// handler to time it with.
pub fn frame(window: Id, snapshot: &Snapshot) -> Option<&'static Block> {
    PROBE.with(|p| {
        let mut p = p.borrow_mut();
        let probe = p.as_mut().filter(|p| p.window == window)?;
        let corner = snapshot.char_at(snapshot.rows().checked_sub(1)?, 0)?;
        probe.ready |= corner == '.';
        let (_, at) = probe.sent.filter(|&(key, _)| key == corner)?;
        probe.sent = None;
        // SAFETY: plain C call.
        *WATCHED.lock().unwrap() = Some((at, unsafe { CACurrentMediaTime() }));
        Some(&PRESENTED)
    })
}

extern "C" fn tick(_: *mut c_void) {
    let done = PROBE.with(|p| {
        let mut p = p.borrow_mut();
        let Some(probe) = p.as_mut() else { return false };
        // SAFETY: plain C call.
        let now = unsafe { CACurrentMediaTime() };
        if probe.sent.is_some_and(|(_, at)| now - at > TIMEOUT) {
            probe.sent = None;
            probe.lost += 1;
        }
        if probe.sent.is_none() && probe.ready && WATCHED.lock().unwrap().is_none() {
            if probe.typed == KEYS {
                return true;
            }
            let key = (b'a' + (probe.typed % 26) as u8) as char;
            // SAFETY: main thread; documented signatures.
            unsafe { post_key(probe.window, key) };
            probe.sent = Some((key, now));
            probe.typed += 1;
        }
        false
    });
    if done {
        finish();
    } else {
        // 30–80 ms apart, so keys land at every phase of the display refresh.
        let jitter = PROBE.with(|p| p.borrow_mut().as_mut().map_or(0, |p| {
            p.rng ^= p.rng << 13;
            p.rng ^= p.rng >> 7;
            p.rng ^= p.rng << 17;
            p.rng % 50
        }));
        run_on_main_after(std::time::Duration::from_millis(30 + jitter), tick);
    }
}

/// Queues a key press as if typed, so it goes through `sendEvent:`.
unsafe fn post_key(window: Id, key: char) {
    unsafe {
        let chars = nsstring(&key.to_string());
        let number: isize = msg!(window, c"windowNumber");
        let event: Id = msg!(
            class(c"NSEvent"),
            c"keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:",
            10usize, // NSEventTypeKeyDown
            CGPoint::default(),
            0usize,
            CACurrentMediaTime(),
            number,
            NIL,
            chars,
            chars,
            NO,
            0u16
        );
        let app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
        call!(app, c"postEvent:atStart:", event, NO);
    }
}

fn finish() {
    let Some(probe) = PROBE.with(|p| p.borrow_mut().take()) else { return };
    let results = std::mem::take(&mut *RESULTS.lock().unwrap());
    let text: String = results.iter().map(|(frame, screen)| format!("{frame:.3}\t{screen:.3}\n")).collect();
    if let Err(e) = fs::write(&probe.out, format!("frame_ms\tscreen_ms\n{text}")) {
        eprintln!("mightty: {}: {e}", probe.out.display());
    }
    let summary = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v.get(((v.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(f64::NAN);
        format!("median {:.2} ms, p95 {:.2} ms, max {:.2} ms", at(0.5), at(0.95), at(1.0))
    };
    let dropped = results.iter().filter(|r| r.1.is_nan()).count();
    eprintln!("mightty: {} keys, {} lost, {} frames dropped", results.len(), probe.lost, dropped);
    eprintln!("  to frame:  {}", summary(results.iter().map(|r| r.0).collect()));
    eprintln!("  to screen: {}", summary(results.iter().map(|r| r.1).filter(|v| !v.is_nan()).collect()));
    // SAFETY: main thread; documented signature.
    unsafe {
        let app: Id = msg!(class(c"NSApplication"), c"sharedApplication");
        call!(app, c"terminate:", NIL);
    }
}
