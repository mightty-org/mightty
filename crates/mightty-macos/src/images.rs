//! Image files decoded by macOS (ImageIO), for inline images in formats
//! the terminal doesn't decode itself: JPEG, GIF and the like.
//!
//! The files come from program output, so they're decoded in a separate
//! process, `mightty decode-image`. It closes every file it was given and
//! enters a sandbox before it reads the file, so a decoder fooled by a
//! crafted file can't reach the shells, the user's files, the network or
//! other programs. It's killed if it takes too long, and what it answers
//! is checked like any other image.

use std::ffi::{CStr, c_char};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};
use std::{ptr, thread};

use mightty_term::DecodeLimits;

use crate::ffi::*;

/// The argument that starts `mightty` as the decoding process.
pub const HELPER_ARG: &str = "decode-image";

/// How long the decoding process may take for one image. The terminal
/// waits for it, so this is short.
const TIMEOUT: Duration = Duration::from_secs(2);

/// How long images other than PNGs are refused after one took too long:
/// a stream of images made to be slow can't keep the terminal waiting.
const BACKOFF: Duration = Duration::from_secs(10);

/// The most a request may be: its limits, then the file.
const MAX_REQUEST: u64 = 16 + (64 << 20);

/// The sandbox a file is decoded in, by its format.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// PNG, JPEG, GIF and WebP: decoded with nothing but memory.
    Picture,
    /// HEIC and HEIF, which macOS decodes with its video decoder: a
    /// separate system service, reached through one more opening.
    Heif,
}

impl Kind {
    /// The sandbox `data` needs, by its first bytes; the decoder checks
    /// the format again, so a file can't claim a looser sandbox than its
    /// format gets.
    fn of(data: &[u8]) -> Kind {
        if data.get(4..8) == Some(b"ftyp") {
            Kind::Heif
        } else {
            Kind::Picture
        }
    }

    /// The formats taken, by type identifier: the common ones for
    /// pictures. Others ImageIO reads (TIFF, camera raw, PSD…) aren't
    /// offered output from any program to parse.
    fn types(self) -> &'static [&'static str] {
        match self {
            Kind::Picture => &[
                "public.png",
                "public.jpeg",
                "com.compuserve.gif",
                "org.webmproject.webp",
            ],
            Kind::Heif => &["public.heic", "public.heif"],
        }
    }

    fn arg(self) -> &'static str {
        match self {
            Kind::Picture => "picture",
            Kind::Heif => "heif",
        }
    }

    /// Enters this kind's sandbox: from here on, no files, network or
    /// other processes. Whether it did.
    fn enter_sandbox(self) -> bool {
        // The video decoder's plug-ins, the service that runs them, the
        // shared memory its frames come back in, and whether it's in a VM.
        const HEIF: &CStr = c"(version 1) (deny default) \
            (allow file-read* (subpath \"/System/Library/Video/Plug-Ins\")) \
            (allow iokit-open-user-client (iokit-user-client-class \"IOSurfaceRootUserClient\")) \
            (allow mach-lookup (global-name \"com.apple.coremedia.videodecoder\")) \
            (allow sysctl-read (sysctl-name \"kern.hv_vmm_present\"))";
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: a profile, named or written out, as a C string; the
        // error, if any, is freed once.
        unsafe {
            let failed = match self {
                Kind::Picture => {
                    sandbox_init(c"pure-computation".as_ptr(), SANDBOX_NAMED, &mut error)
                }
                Kind::Heif => sandbox_init(HEIF.as_ptr(), 0, &mut error),
            };
            if !error.is_null() {
                sandbox_free_error(error);
            }
            failed == 0
        }
    }
}

/// [`decode_in`] this executable. A [`mightty_term::ImageDecoder`].
pub fn decode_sandboxed(data: &[u8], limits: DecodeLimits) -> Option<(u32, u32, Vec<u8>)> {
    static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
    static TIMED_OUT: Mutex<Option<Instant>> = Mutex::new(None);
    let timed_out = || TIMED_OUT.lock().unwrap_or_else(|e| e.into_inner());
    if timed_out().is_some_and(|at| at.elapsed() < BACKOFF) {
        return None;
    }
    let exe = EXE
        .get_or_init(|| std::env::current_exe().ok())
        .as_deref()?;
    match decode_in(exe, data, limits) {
        Ok(image) => image,
        Err(TimedOut) => {
            *timed_out() = Some(Instant::now());
            None
        }
    }
}

/// The decoding process didn't answer in time.
#[derive(Debug)]
pub struct TimedOut;

/// `data` decoded by `exe` started with [`HELPER_ARG`], in a sandbox:
/// the request is the limits, then the file; the answer the width,
/// height and pixels. `None` if it isn't an image within `limits`.
pub fn decode_in(
    exe: &Path,
    data: &[u8],
    limits: DecodeLimits,
) -> Result<Option<(u32, u32, Vec<u8>)>, TimedOut> {
    if data.len() as u64 + 16 > MAX_REQUEST {
        return Ok(None);
    }
    let spawned = {
        // The pipes are made before they're close-on-exec: held so no
        // pane's shell, started meanwhile, gets a copy.
        let _spawning = mightty_pty::spawning();
        Command::new(exe)
            .args([HELPER_ARG, Kind::of(data).arg()])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    };
    let Ok(mut child) = spawned else {
        return Ok(None);
    };
    let (Some(mut input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(None);
    };
    let mut head = Vec::with_capacity(16);
    head.extend(limits.max_side.to_le_bytes());
    head.extend((limits.max_bytes as u64).to_le_bytes());
    head.extend(limits.shown_side.to_le_bytes());
    let most = 8 + limits.max_bytes as u64;
    let mut request = head;
    request.extend_from_slice(data);
    // Written and read on their own threads, so neither can block the
    // other, and a process that stops answering can be killed. They're
    // left to end on their own, not waited for: another program started
    // as the pipes were made could hold a copy of them open.
    let (tx, rx) = mpsc::channel();
    let writing = thread::Builder::new().spawn(move || {
        let _ = input.write_all(&request);
    });
    let reading = thread::Builder::new().spawn(move || {
        let mut out = Vec::new();
        let read = output.take(most + 1).read_to_end(&mut out);
        let _ = tx.send(read.ok().map(|_| out));
    });
    let answer = match (writing, reading) {
        (Ok(_), Ok(_)) => match rx.recv_timeout(TIMEOUT) {
            Ok(answer) => Ok(answer),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(TimedOut),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(None),
        },
        _ => Ok(None),
    };
    // Done with it, whatever it did: killed, its ends of the pipes close,
    // so neither thread is left waiting on it, even if it closed its
    // output early and stopped reading.
    let _ = child.kill();
    let _ = child.wait();
    let Some(out) = answer? else {
        return Ok(None);
    };
    Ok(answer_image(&out, limits))
}

/// The width, height and pixels in the decoding process's answer, if
/// they're an image within `limits`: it's checked like any other input.
fn answer_image(out: &[u8], limits: DecodeLimits) -> Option<(u32, u32, Vec<u8>)> {
    let (size, pixels) = out.split_at_checked(8)?;
    let w = u32::from_le_bytes(size[..4].try_into().ok()?);
    let h = u32::from_le_bytes(size[4..].try_into().ok()?);
    let side = 1..=limits.max_side;
    let sane = side.contains(&w)
        && side.contains(&h)
        && u64::from(w) * u64::from(h) * 4 == pixels.len() as u64
        && pixels.len() <= limits.max_bytes;
    sane.then(|| (w, h, pixels.to_vec()))
}

/// `mightty decode-image KIND`: reads a request, answers it, and exits.
pub fn helper_main(args: &[String]) -> ExitCode {
    let kind = match args {
        [kind] if kind == "picture" => Kind::Picture,
        [kind] if kind == "heif" => Kind::Heif,
        _ => return ExitCode::from(2),
    };
    // Every descriptor past stdin, stdout and stderr: nothing in this
    // process uses them, as it has only just started.
    let open: Vec<i32> = std::fs::read_dir("/dev/fd")
        .map(|dir| {
            dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    for fd in open.into_iter().filter(|&fd| fd > 2) {
        // SAFETY: see above; one that's already closed (the directory's
        // own) fails harmlessly.
        unsafe { close(fd) };
    }
    // ImageIO reads the app's Info.plist and sets itself up the first
    // time it's used, which it can't once in the sandbox: done here with
    // an image of Mightty's own.
    let warm = DecodeLimits {
        max_side: 2,
        max_bytes: 8,
        shown_side: 2,
    };
    decode(WARM_UP, Kind::Picture, warm);
    if !kind.enter_sandbox() {
        return ExitCode::FAILURE;
    }
    let mut request = Vec::new();
    if std::io::stdin()
        .take(MAX_REQUEST)
        .read_to_end(&mut request)
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    let Some((head, data)) = request.split_at_checked(16) else {
        return ExitCode::FAILURE;
    };
    let limits = DecodeLimits {
        max_side: u32::from_le_bytes(head[..4].try_into().unwrap()),
        max_bytes: u64::from_le_bytes(head[4..12].try_into().unwrap()) as usize,
        shown_side: u32::from_le_bytes(head[12..].try_into().unwrap()),
    };
    let Some((w, h, pixels)) = decode(data, kind, limits) else {
        return ExitCode::FAILURE;
    };
    let mut out = std::io::stdout().lock();
    let written = out
        .write_all(&w.to_le_bytes())
        .and_then(|()| out.write_all(&h.to_le_bytes()))
        .and_then(|()| out.write_all(&pixels))
        .and_then(|()| out.flush());
    if written.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// A 2×1 GIF: a red pixel, then a blue one.
const WARM_UP: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x02, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xFF, 0x00, 0x00,
    0x00, 0x00, 0xFF, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44,
    0x0A, 0x00, 0x3B,
];

/// `data`'s first image as premultiplied RGBA in sRGB, top row first and
/// turned upright, if it's in one of `kind`'s formats and within
/// `limits`. In this process: [`decode_in`] runs it in a sandbox.
fn decode(data: &[u8], kind: Kind, limits: DecodeLimits) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: `data` outlives the CFData that borrows it (released before
    // returning, with everything made from it); documented signatures;
    // each object made here is released once.
    unsafe {
        let cf_data = CFDataCreateWithBytesNoCopy(
            ptr::null(),
            data.as_ptr(),
            data.len() as isize,
            kCFAllocatorNull,
        );
        if cf_data.is_null() {
            return None;
        }
        let source = CGImageSourceCreateWithData(cf_data, ptr::null());
        let pixels = (!source.is_null()
            && allowed(CGImageSourceGetType(source), kind)
            && fits(source, limits))
        .then(|| thumbnail(source, limits.shown_side))
        .filter(|image| !image.is_null())
        .and_then(|image| {
            let (w, h) = (CGImageGetWidth(image), CGImageGetHeight(image));
            let pixels = draw(image, w, h, limits);
            CFRelease(image);
            pixels.map(|p| (w as u32, h as u32, p))
        });
        if !source.is_null() {
            CFRelease(source);
        }
        CFRelease(cf_data);
        pixels
    }
}

/// Whether `found`, a type identifier ImageIO found, is one of `kind`'s.
fn allowed(found: CFStringRef, kind: Kind) -> bool {
    !found.is_null()
        && kind.types().iter().any(|name| {
            let name = cfstring(name);
            // SAFETY: both are CFStrings; `name` was made here.
            unsafe {
                let same = CFEqual(found, name);
                CFRelease(name);
                same
            }
        })
}

/// Whether the first image in `source` is within `limits`, by the size
/// its file gives: checked before anything is decoded, since a small
/// file can hold a huge image.
///
/// # Safety
/// `source` must be a `CGImageSource`.
unsafe fn fits(source: CFTypeRef, limits: DecodeLimits) -> bool {
    // SAFETY: documented signatures; the properties are released once,
    // and the numbers in them are only read while they're held.
    unsafe {
        let props = CGImageSourceCopyPropertiesAtIndex(source, 0, ptr::null());
        if props.is_null() {
            return false;
        }
        let side = |key: CFStringRef| {
            let number = CFDictionaryGetValue(props, key);
            let mut value = 0i64;
            (!number.is_null()
                && CFNumberGetValue(number, kCFNumberSInt64Type, (&raw mut value).cast()))
            .then_some(value)
            .and_then(|v| u32::try_from(v).ok())
        };
        let size = side(kCGImagePropertyPixelWidth).zip(side(kCGImagePropertyPixelHeight));
        CFRelease(props);
        size.is_some_and(|(w, h)| {
            (1..=limits.max_side).contains(&w)
                && (1..=limits.max_side).contains(&h)
                && u64::from(w) * u64::from(h) * 4 <= limits.max_bytes as u64
        })
    }
}

/// The first image in `source`, upright and no more than `side` pixels on
/// its longer side: ImageIO decodes it at about that size, which costs
/// far less than decoding it whole and shrinking it after.
///
/// # Safety
/// `source` must be a `CGImageSource`.
unsafe fn thumbnail(source: CFTypeRef, side: u32) -> CFTypeRef {
    // SAFETY: documented signatures; the number and the options are
    // released once, after the dictionary retained what it holds.
    unsafe {
        let side = i64::from(side.max(1));
        let number = CFNumberCreate(ptr::null(), kCFNumberSInt64Type, (&raw const side).cast());
        let keys = [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceThumbnailMaxPixelSize,
        ];
        let values = [kCFBooleanTrue, kCFBooleanTrue, number];
        let options = CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr().cast(),
            values.as_ptr(),
            keys.len() as isize,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        );
        CFRelease(number);
        let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options);
        if !options.is_null() {
            CFRelease(options);
        }
        image
    }
}

/// # Safety
/// `image` must be a `CGImage` `w` × `h` pixels.
unsafe fn draw(image: CFTypeRef, w: usize, h: usize, limits: DecodeLimits) -> Option<Vec<u8>> {
    let side = limits.max_side as usize;
    if !(1..=side).contains(&w) || !(1..=side).contains(&h) || w * h * 4 > limits.max_bytes {
        return None;
    }
    let mut pixels = vec![0u8; w * h * 4];
    // SAFETY: the context draws into `pixels`, `w * 4` bytes a row, and
    // is released before `pixels` is returned; documented signatures.
    unsafe {
        let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        // Bytes in RGBA order: the default byte order, alpha last.
        let ctx = CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            w,
            h,
            8,
            w * 4,
            space,
            kCGImageAlphaPremultipliedLast,
        );
        CGColorSpaceRelease(space);
        if ctx.is_null() {
            return None;
        }
        let rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: w as f64,
                height: h as f64,
            },
        };
        CGContextDrawImage(ctx, rect, image);
        CGContextRelease(ctx);
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_side: u32, max_bytes: usize, shown_side: u32) -> DecodeLimits {
        DecodeLimits {
            max_side,
            max_bytes,
            shown_side,
        }
    }

    #[test]
    fn decodes_a_gif() {
        let (w, h, pixels) = decode(WARM_UP, Kind::Picture, limits(100, 100, 100)).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(pixels, [255, 0, 0, 255, 0, 0, 255, 255]);
        // Shrunk to the side shown.
        let (w, h, _) = decode(WARM_UP, Kind::Picture, limits(100, 100, 1)).unwrap();
        assert_eq!((w, h), (1, 1));
        // Too large a side or too many bytes, or not an image.
        assert!(decode(WARM_UP, Kind::Picture, limits(1, 100, 100)).is_none());
        assert!(decode(WARM_UP, Kind::Picture, limits(100, 7, 100)).is_none());
        assert!(decode(b"hello", Kind::Picture, limits(100, 100, 100)).is_none());
        // Not in the formats for the sandbox asked for.
        assert!(decode(WARM_UP, Kind::Heif, limits(100, 100, 100)).is_none());
    }

    #[test]
    fn kinds() {
        assert_eq!(Kind::of(WARM_UP), Kind::Picture);
        assert_eq!(Kind::of(b"\0\0\0\x18ftypheic"), Kind::Heif);
        assert_eq!(Kind::of(b"ftyp"), Kind::Picture);
    }
}
