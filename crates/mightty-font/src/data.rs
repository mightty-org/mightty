//! A font file's bytes, shared by its faces: in memory, or mapped from disk
//! so a large file (Apple Color Emoji is 180 MB) costs only the pages read.

use std::fmt;
use std::fs;
use std::io;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct FontData(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl FontData {
    /// `path` mapped read-only, or read where mapping isn't available.
    /// Font files aren't expected to change while open: one cut short
    /// under a mapping would fault on access, as with other font libraries.
    pub fn map(path: &Path) -> io::Result<FontData> {
        #[cfg(unix)]
        {
            let file = fs::File::open(path)?;
            Ok(match map::Mapped::new(&file)? {
                Some(mapped) => FontData(Arc::new(mapped)),
                None => FontData::from(Vec::new()),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(FontData::from(fs::read(path)?))
        }
    }
}

impl From<Vec<u8>> for FontData {
    fn from(bytes: Vec<u8>) -> Self {
        FontData(Arc::new(bytes))
    }
}

impl Deref for FontData {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

impl fmt::Debug for FontData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FontData({} bytes)", self.len())
    }
}

#[cfg(unix)]
mod map {
    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    const PROT_READ: i32 = 1;
    const MAP_PRIVATE: i32 = 2;

    unsafe extern "C" {
        fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut c_void;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
    }

    pub(super) struct Mapped {
        ptr: *mut c_void,
        len: usize,
    }

    // SAFETY: a read-only mapping, never written through, freed once on drop.
    unsafe impl Send for Mapped {}
    unsafe impl Sync for Mapped {}

    impl Mapped {
        /// `None` for an empty file, which can't be mapped.
        pub(super) fn new(file: &File) -> io::Result<Option<Mapped>> {
            let len = usize::try_from(file.metadata()?.len()).map_err(|_| io::Error::other("file too large"))?;
            if len == 0 {
                return Ok(None);
            }
            // SAFETY: a fresh read-only private mapping of an open file; the
            // descriptor may close after, the mapping stays valid.
            let ptr = unsafe { mmap(std::ptr::null_mut(), len, PROT_READ, MAP_PRIVATE, file.as_raw_fd(), 0) };
            if ptr as isize == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(Mapped { ptr, len }))
        }
    }

    impl AsRef<[u8]> for Mapped {
        fn as_ref(&self) -> &[u8] {
            // SAFETY: `len` bytes mapped readable at `ptr` for our lifetime.
            unsafe { std::slice::from_raw_parts(self.ptr.cast(), self.len) }
        }
    }

    impl Drop for Mapped {
        fn drop(&mut self) {
            // SAFETY: mapped by `new` with this length, unmapped once.
            unsafe { munmap(self.ptr, self.len) };
        }
    }
}
