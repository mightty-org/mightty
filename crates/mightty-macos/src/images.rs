//! Image files decoded by macOS (ImageIO), for inline images in formats
//! the terminal doesn't decode itself: JPEG, GIF and the like.

use std::ptr;

use mightty_term::DecodeLimits;

use crate::ffi::*;

/// The formats taken, by type identifier: the common ones for pictures.
/// Others ImageIO reads (TIFF, camera raw, PSD…) aren't offered output
/// from any program to parse.
const TYPES: &[&str] = &[
    "public.png",
    "public.jpeg",
    "com.compuserve.gif",
    "org.webmproject.webp",
    "public.heic",
    "public.heif",
];

/// `data`'s first image as premultiplied RGBA in sRGB, top row first and
/// turned upright, if it's in one of [`TYPES`] and within `limits`.
/// A [`mightty_term::ImageDecoder`].
pub fn decode(data: &[u8], limits: DecodeLimits) -> Option<(u32, u32, Vec<u8>)> {
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
        let pixels =
            (!source.is_null() && allowed(CGImageSourceGetType(source)) && fits(source, limits))
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

/// Whether `kind`, a type identifier ImageIO found, is one of [`TYPES`].
fn allowed(kind: CFStringRef) -> bool {
    !kind.is_null()
        && TYPES.iter().any(|name| {
            let name = cfstring(name);
            // SAFETY: both are CFStrings; `name` was made here.
            unsafe {
                let same = CFEqual(kind, name);
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

    /// A 2×1 GIF: a red pixel, then a blue one.
    const GIF: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x02, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xFF, 0x00,
        0x00, 0x00, 0x00, 0xFF, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x02,
        0x02, 0x44, 0x0A, 0x00, 0x3B,
    ];

    fn limits(max_side: u32, max_bytes: usize, shown_side: u32) -> DecodeLimits {
        DecodeLimits {
            max_side,
            max_bytes,
            shown_side,
        }
    }

    #[test]
    fn decodes_a_gif() {
        let (w, h, pixels) = decode(GIF, limits(100, 100, 100)).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(pixels, [255, 0, 0, 255, 0, 0, 255, 255]);
        // Shrunk to the side shown.
        let (w, h, _) = decode(GIF, limits(100, 100, 1)).unwrap();
        assert_eq!((w, h), (1, 1));
        // Too large a side or too many bytes, or not an image.
        assert!(decode(GIF, limits(1, 100, 100)).is_none());
        assert!(decode(GIF, limits(100, 7, 100)).is_none());
        assert!(decode(b"hello", limits(100, 100, 100)).is_none());
    }
}
