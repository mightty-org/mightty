//! Hand-written bindings to the Objective-C runtime and the Apple frameworks
//! we use. Messages go through `objc_msgSend`, cast to the exact signature
//! of each method (required on arm64, where it is not variadic).

#![allow(non_upper_case_globals)]

use std::borrow::Cow;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;

pub type Id = *mut c_void;
pub type Sel = *const c_void;
pub type Bool = i8;
pub const NIL: Id = ptr::null_mut();
pub const YES: Bool = 1;
pub const NO: Bool = 0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

/// `NSRange`, in UTF-16 code units.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NSRange {
    pub location: usize,
    pub length: usize,
}

pub const NS_NOT_FOUND: usize = isize::MAX as usize;

#[repr(C)]
pub struct CFRange {
    pub location: isize,
    pub length: isize,
}

#[repr(C)]
pub struct MTLOrigin {
    pub x: usize,
    pub y: usize,
    pub z: usize,
}

#[repr(C)]
pub struct MTLSize {
    pub width: usize,
    pub height: usize,
    pub depth: usize,
}

#[repr(C)]
pub struct MTLRegion {
    pub origin: MTLOrigin,
    pub size: MTLSize,
}

#[repr(C)]
pub struct MTLClearColor {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
    fn objc_msgSendSuper();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSendSuper_stret();
    pub fn object_getClass(obj: Id) -> Id;
    pub fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra_bytes: usize) -> Id;
    pub fn objc_registerClassPair(cls: Id);
    pub fn class_addMethod(cls: Id, name: Sel, imp: *const c_void, types: *const c_char) -> Bool;
    fn class_addProtocol(cls: Id, protocol: Id) -> Bool;
    fn objc_getProtocol(name: *const c_char) -> Id;
    pub fn objc_autoreleasePoolPush() -> *mut c_void;
    pub fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    pub static NSPasteboardTypeString: Id;
    pub fn NSBeep();
}

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    pub static kCAGravityTopLeft: Id;
    /// Seconds on the clock Core Animation and Metal timestamps use.
    pub fn CACurrentMediaTime() -> f64;
}

// Blocks runtime, in libSystem.
unsafe extern "C" {
    static _NSConcreteGlobalBlock: [*const c_void; 32];
}

/// An Objective-C block that captures nothing, for APIs taking a callback
/// block: `Block::new(f)` in a `static`, passed as `&BLOCK`.
#[repr(C)]
pub struct Block {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    /// Its signature is the one the block was made for.
    invoke: *const c_void,
    descriptor: &'static BlockDescriptor,
}

#[repr(C)]
pub struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

// SAFETY: a global block is immutable; copying it returns the same block.
unsafe impl Sync for Block {}

impl Block {
    /// A block whose single argument is an object, as in `void (^)(id)`.
    pub const fn new(invoke: unsafe extern "C" fn(*const Block, Id)) -> Self {
        Self::with_invoke(invoke as *const c_void)
    }

    /// `void (^)(BOOL, id)`, as for completion handlers reporting success and an error.
    pub const fn with_flag(invoke: unsafe extern "C" fn(*const Block, Bool, Id)) -> Self {
        Self::with_invoke(invoke as *const c_void)
    }

    const fn with_invoke(invoke: *const c_void) -> Self {
        const BLOCK_IS_GLOBAL: i32 = 1 << 28;
        static DESCRIPTOR: BlockDescriptor = BlockDescriptor { reserved: 0, size: std::mem::size_of::<Block>() };
        // Only the address of the runtime's class object is taken.
        Self { isa: (&raw const _NSConcreteGlobalBlock).cast(), flags: BLOCK_IS_GLOBAL, reserved: 0, invoke, descriptor: &DESCRIPTOR }
    }
}

/// Calls `block`, a `void (^)(void)` handed to us (a completion handler).
///
/// # Safety
/// `block` must be a live block taking no arguments.
pub unsafe fn call_block(block: Id) {
    if block.is_null() {
        return;
    }
    // SAFETY: every block starts with the layout of `Block`.
    unsafe {
        let invoke: unsafe extern "C" fn(Id) = std::mem::transmute((*block.cast::<Block>()).invoke);
        invoke(block);
    }
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    pub fn TISCopyCurrentKeyboardLayoutInputSource() -> CFTypeRef;
    pub fn TISGetInputSourceProperty(source: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    pub static kTISPropertyInputSourceID: CFStringRef;
}

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    pub fn MTLCreateSystemDefaultDevice() -> Id;
}

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CTFontRef = *const c_void;
pub type CGContextRef = *mut c_void;
pub type CGColorSpaceRef = *mut c_void;

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCTFontTraitItalic: u32 = 1 << 0;
pub const kCTFontTraitBold: u32 = 1 << 1;
pub const kCTFontTraitColorGlyphs: u32 = 1 << 13;
pub const kCGImageAlphaPremultipliedFirst: u32 = 2;
pub const kCGBitmapByteOrder32Little: u32 = 2 << 12;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFStringCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        is_external: bool,
    ) -> CFStringRef;
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFDictionaryCreate(
        alloc: *const c_void,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    pub fn CFAttributedStringCreate(alloc: *const c_void, string: CFStringRef, attributes: CFTypeRef) -> CFTypeRef;
    pub static kCFTypeDictionaryKeyCallBacks: [u8; 0];
    pub static kCFTypeDictionaryValueCallBacks: [u8; 0];
    pub static kCFBooleanTrue: CFTypeRef;
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    pub fn CTFontCreateWithName(name: CFStringRef, size: f64, matrix: *const c_void) -> CTFontRef;
    pub fn CTFontCreateCopyWithSymbolicTraits(
        font: CTFontRef,
        size: f64,
        matrix: *const c_void,
        value: u32,
        mask: u32,
    ) -> CTFontRef;
    pub fn CTFontCreateForString(current: CTFontRef, string: CFStringRef, range: CFRange) -> CTFontRef;
    pub fn CTFontGetAscent(font: CTFontRef) -> f64;
    pub fn CTFontGetDescent(font: CTFontRef) -> f64;
    pub fn CTFontGetLeading(font: CTFontRef) -> f64;
    pub fn CTFontGetSymbolicTraits(font: CTFontRef) -> u32;
    pub fn CTFontGetGlyphsForCharacters(font: CTFontRef, chars: *const u16, glyphs: *mut u16, count: isize) -> bool;
    pub fn CTFontGetAdvancesForGlyphs(
        font: CTFontRef,
        orientation: u32,
        glyphs: *const u16,
        advances: *mut CGSize,
        count: isize,
    ) -> f64;
    pub fn CTFontDrawGlyphs(font: CTFontRef, glyphs: *const u16, positions: *const CGPoint, count: usize, ctx: CGContextRef);
    pub fn CTLineCreateWithAttributedString(string: CFTypeRef) -> CFTypeRef;
    pub fn CTLineGetTypographicBounds(line: CFTypeRef, ascent: *mut f64, descent: *mut f64, leading: *mut f64) -> f64;
    pub fn CTLineDraw(line: CFTypeRef, ctx: CGContextRef);
    pub static kCTFontAttributeName: CFStringRef;
    pub static kCTForegroundColorFromContextAttributeName: CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    pub fn CGColorSpaceRelease(space: CGColorSpaceRef);
    pub fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    pub fn CGContextRelease(ctx: CGContextRef);
    pub fn CGContextSetRGBFillColor(ctx: CGContextRef, r: f64, g: f64, b: f64, a: f64);
    pub fn CGContextSetTextPosition(ctx: CGContextRef, x: f64, y: f64);
    pub fn CGContextSetAllowsFontSmoothing(ctx: CGContextRef, allows: bool);
    pub fn CGContextSetShouldSmoothFonts(ctx: CGContextRef, should: bool);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub fn CGBitmapContextCreateImage(ctx: CGContextRef) -> CFTypeRef;
    pub fn CFURLCreateFromFileSystemRepresentation(
        alloc: *const c_void,
        path: *const u8,
        len: isize,
        is_directory: bool,
    ) -> CFTypeRef;
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    pub fn CGImageDestinationCreateWithURL(url: CFTypeRef, kind: CFStringRef, count: usize, options: CFTypeRef) -> CFTypeRef;
    pub fn CGImageDestinationAddImage(dest: CFTypeRef, image: CFTypeRef, properties: CFTypeRef);
    pub fn CGImageDestinationFinalize(dest: CFTypeRef) -> bool;
}

// libdispatch lives in libSystem.
unsafe extern "C" {
    static _dispatch_main_q: u8;
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_after_f(when: u64, queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_time(when: u64, delta: i64) -> u64;
    fn dispatch_sync_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn pthread_main_np() -> i32;
}

/// Runs `work` on the main thread and waits for its result (at once, when
/// this is the main thread). The main thread must not be waiting on the caller.
pub fn run_on_main_sync<F: FnOnce() -> R + Send, R: Send>(work: F) -> R {
    // SAFETY: plain query.
    if unsafe { pthread_main_np() } != 0 {
        return work();
    }
    extern "C" fn trampoline<F: FnOnce() -> R, R>(context: *mut c_void) {
        // SAFETY: `context` is the slot below, alive until dispatch_sync_f returns.
        let slot = unsafe { &mut *context.cast::<(Option<F>, Option<R>)>() };
        slot.1 = slot.0.take().map(|work| work());
    }
    let mut slot = (Some(work), None);
    // SAFETY: dispatch_sync_f returns only after `trampoline` has run.
    unsafe { dispatch_sync_f((&raw const _dispatch_main_q).cast(), (&raw mut slot).cast(), trampoline::<F, R>) };
    slot.1.expect("main-thread work ran")
}

/// Schedules `work` on the main thread's run loop.
pub fn run_on_main(work: extern "C" fn(*mut c_void)) {
    run_on_main_with(0, work);
}

/// Like `run_on_main`, passing `context` (an integer, not a pointer) to `work`.
pub fn run_on_main_with(context: usize, work: extern "C" fn(*mut c_void)) {
    // SAFETY: the main queue is a static object that lives for the whole
    // process, and the context is only ever read back as an integer.
    unsafe { dispatch_async_f((&raw const _dispatch_main_q).cast(), context as *mut c_void, work) }
}

/// Like `run_on_main`, but no sooner than `delay` from now.
pub fn run_on_main_after(delay: std::time::Duration, work: extern "C" fn(*mut c_void)) {
    // SAFETY: as in `run_on_main_with`; `dispatch_time(DISPATCH_TIME_NOW, ns)`.
    unsafe {
        let when = dispatch_time(0, delay.as_nanos().min(i64::MAX as u128) as i64);
        dispatch_after_f(when, (&raw const _dispatch_main_q).cast(), std::ptr::null_mut(), work)
    }
}

pub fn class(name: &CStr) -> Id {
    // SAFETY: valid C string; returns null for unknown classes.
    unsafe { objc_getClass(name.as_ptr()) }
}

pub fn sel(name: &CStr) -> Sel {
    // SAFETY: valid C string.
    unsafe { sel_registerName(name.as_ptr()) }
}

/// Argument tuples that can be passed through `objc_msgSend`.
pub trait MessageArgs {
    /// # Safety
    /// `imp` must be a messenger and the method's real signature must match `(Self) -> R`.
    unsafe fn invoke<R>(self, imp: *const (), obj: Id, sel: Sel) -> R;
}

macro_rules! impl_message_args {
    ($($t:ident),*) => {
        impl<$($t),*> MessageArgs for ($($t,)*) {
            #[allow(non_snake_case)]
            unsafe fn invoke<R>(self, imp: *const (), obj: Id, sel: Sel) -> R {
                let ($($t,)*) = self;
                // SAFETY: upheld by the caller.
                unsafe {
                    let f: unsafe extern "C" fn(Id, Sel $(, $t)*) -> R = std::mem::transmute(imp);
                    f(obj, sel $(, $t)*)
                }
            }
        }
    };
}

impl_message_args!();
impl_message_args!(A);
impl_message_args!(A, B);
impl_message_args!(A, B, C);
impl_message_args!(A, B, C, D);
impl_message_args!(A, B, C, D, E, F, G, H, I, J);

/// Sends `sel` to `obj`.
///
/// # Safety
/// The receiver must respond to `sel` with exactly the signature `(args) -> R`.
pub unsafe fn send<A: MessageArgs, R>(obj: Id, sel_name: &CStr, args: A) -> R {
    unsafe { args.invoke(objc_msgSend as *const (), obj, sel(sel_name)) }
}

/// Sends a message returning a `CGRect` (needs `objc_msgSend_stret` on x86_64).
///
/// # Safety
/// The receiver must respond to `sel` with signature `() -> CGRect`.
pub unsafe fn send_rect(obj: Id, sel_name: &CStr) -> CGRect {
    unsafe { send_rect_with(obj, sel_name, ()) }
}

/// `objc_super`: a receiver and the class to start the method lookup at.
#[repr(C)]
pub struct ObjcSuper {
    pub receiver: Id,
    pub superclass: Id,
}

/// Calls the `superclass` implementation of a method returning a `CGRect`,
/// from an override in a subclass.
///
/// # Safety
/// `obj` must be an instance of a subclass of `superclass`, which must
/// respond to `sel` with signature `(args) -> CGRect`.
pub unsafe fn send_super_rect<A: MessageArgs>(obj: Id, superclass: Id, sel_name: &CStr, args: A) -> CGRect {
    #[cfg(target_arch = "x86_64")]
    let imp = objc_msgSendSuper_stret as *const ();
    #[cfg(not(target_arch = "x86_64"))]
    let imp = objc_msgSendSuper as *const ();
    let sup = ObjcSuper { receiver: obj, superclass };
    // The receiver argument is a pointer to `objc_super`.
    unsafe { args.invoke(imp, (&raw const sup).cast_mut().cast(), sel(sel_name)) }
}

/// `msg!` to `superclass`'s implementation, for results that aren't structs.
///
/// # Safety
/// As for `send_super_rect`, with signature `(args) -> R`.
pub unsafe fn send_super<A: MessageArgs, R>(obj: Id, superclass: Id, sel_name: &CStr, args: A) -> R {
    let sup = ObjcSuper { receiver: obj, superclass };
    unsafe { args.invoke(objc_msgSendSuper as *const (), (&raw const sup).cast_mut().cast(), sel(sel_name)) }
}

/// `send_rect` for a method taking `args`.
///
/// # Safety
/// The receiver must respond to `sel` with signature `(args) -> CGRect`.
pub unsafe fn send_rect_with<A: MessageArgs>(obj: Id, sel_name: &CStr, args: A) -> CGRect {
    #[cfg(target_arch = "x86_64")]
    let imp = objc_msgSend_stret as *const ();
    #[cfg(not(target_arch = "x86_64"))]
    let imp = objc_msgSend as *const ();
    unsafe { args.invoke(imp, obj, sel(sel_name)) }
}

/// `msg!(obj, c"selector:with:", a, b)`; the return type is inferred.
macro_rules! msg {
    ($obj:expr, $sel:expr $(, $arg:expr)* $(,)?) => {
        $crate::ffi::send($obj, $sel, ($($arg,)*))
    };
}

/// Like `msg!` for methods returning `void`.
macro_rules! call {
    ($($t:tt)*) => {{
        let (): () = msg!($($t)*);
    }};
}

pub(crate) use {call, msg};

/// `[[cls alloc] init]`.
pub unsafe fn new_object(cls: &CStr) -> Id {
    unsafe { msg!(msg!(class(cls), c"alloc"), c"init") }
}

/// An autoreleased `NSString`.
pub fn nsstring(s: &str) -> Id {
    // SAFETY: bytes/len describe a valid UTF-8 buffer; the result is +1 and
    // handed to the current autorelease pool.
    unsafe {
        let cf = CFStringCreateWithBytes(ptr::null(), s.as_ptr(), s.len() as isize, kCFStringEncodingUTF8, false);
        msg!(cf as Id, c"autorelease")
    }
}

/// Copies an `NSString` into Rust; null becomes "".
pub unsafe fn string_from_ns(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    let p: *const c_char = unsafe { msg!(s, c"UTF8String") };
    if p.is_null() {
        return String::new();
    }
    match unsafe { CStr::from_ptr(p) }.to_string_lossy() {
        Cow::Borrowed(s) => s.to_owned(),
        Cow::Owned(s) => s,
    }
}

/// A `CFString` the caller must `CFRelease`.
pub fn cfstring(s: &str) -> CFStringRef {
    // SAFETY: valid UTF-8 buffer.
    unsafe { CFStringCreateWithBytes(ptr::null(), s.as_ptr(), s.len() as isize, kCFStringEncodingUTF8, false) }
}

/// Registers a new Objective-C class with the given methods.
pub unsafe fn register_class(name: &CStr, superclass: &CStr, methods: &[(&CStr, *const c_void, &CStr)]) -> Id {
    unsafe { register_class_with(name, superclass, &[], methods) }
}

/// Like `register_class`, also adopting `protocols` (which some AppKit
/// APIs check, such as `NSTextInputClient`).
pub unsafe fn register_class_with(
    name: &CStr,
    superclass: &CStr,
    protocols: &[&CStr],
    methods: &[(&CStr, *const c_void, &CStr)],
) -> Id {
    unsafe {
        let cls = objc_allocateClassPair(class(superclass), name.as_ptr(), 0);
        assert!(!cls.is_null(), "class {name:?} already registered");
        for protocol in protocols {
            let protocol = objc_getProtocol(protocol.as_ptr());
            if !protocol.is_null() {
                class_addProtocol(cls, protocol);
            }
        }
        for &(selector, imp, types) in methods {
            class_addMethod(cls, sel(selector), imp, types.as_ptr());
        }
        objc_registerClassPair(cls);
        cls
    }
}
