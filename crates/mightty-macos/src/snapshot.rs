//! Writes rendered frames to PNG (`MIGHTTY_SNAPSHOT=path`), for checking
//! renderer output without screen capture and, later, visual tests.

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use crate::ffi::*;

/// Reads back a finished BGRA8 texture and saves it as a PNG.
///
/// # Safety
/// `texture` must be a CPU-readable BGRA8 `MTLTexture` whose rendering has completed.
pub unsafe fn save_texture(texture: Id, path: &Path) -> bool {
    unsafe {
        let width: usize = msg!(texture, c"width");
        let height: usize = msg!(texture, c"height");
        let mut pixels = vec![0u8; width * height * 4];
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width,
                height,
                depth: 1,
            },
        };
        call!(
            texture,
            c"getBytes:bytesPerRow:fromRegion:mipmapLevel:",
            pixels.as_mut_ptr() as *mut c_void,
            width * 4,
            region,
            0usize
        );

        let colorspace = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            colorspace,
            kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
        );
        let image = CGBitmapContextCreateImage(ctx);
        let bytes = path.as_os_str().as_bytes();
        let url = CFURLCreateFromFileSystemRepresentation(
            ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            false,
        );
        let png = cfstring("public.png");
        let dest = CGImageDestinationCreateWithURL(url, png, 1, ptr::null());
        let ok = !dest.is_null() && {
            CGImageDestinationAddImage(dest, image, ptr::null());
            CGImageDestinationFinalize(dest)
        };
        for object in [dest, png, url, image] {
            if !object.is_null() {
                CFRelease(object);
            }
        }
        CGContextRelease(ctx);
        CGColorSpaceRelease(colorspace);
        ok
    }
}
