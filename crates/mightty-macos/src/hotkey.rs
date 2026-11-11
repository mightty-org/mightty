//! `global:` keybinds: keys that act whichever app is active, registered as
//! Carbon hot keys, which need no Accessibility permission (unlike an event
//! tap). Keys are found by their place on a US keyboard.

use std::cell::RefCell;
use std::ffi::c_void;

use crate::bindings::{ALT, Action, Bindings, CMD, CTRL, Key, Named, SHIFT, Trigger};

#[repr(C)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}

#[repr(C)]
#[derive(Default)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type Handler = extern "C" fn(call: *mut c_void, event: *mut c_void, data: *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: Handler,
        count: u32,
        types: *const EventTypeSpec,
        data: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        code: u32,
        mods: u32,
        id: EventHotKeyId,
        target: *mut c_void,
        options: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(hot_key: *mut c_void) -> i32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        kind: u32,
        actual: *mut u32,
        size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
}

const fn four(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// The hot keys registered, and their actions by id.
struct Registered {
    keys: Vec<*mut c_void>,
    actions: Vec<Action>,
    handler: bool,
}

thread_local! {
    static REGISTERED: RefCell<Registered> = const { RefCell::new(Registered { keys: Vec::new(), actions: Vec::new(), handler: false }) };
}

/// Registers `bindings`' global keys in place of those before; `perform`
/// runs an action when its key is pressed. Returns the triggers that
/// couldn't be registered (taken by another app, or with no key code).
pub fn register(bindings: &Bindings, perform: fn(&Action)) -> Vec<Trigger> {
    PERFORM.set(Some(perform));
    let mut failed = Vec::new();
    REGISTERED.with_borrow_mut(|r| {
        // SAFETY: Carbon calls on the main thread with documented signatures.
        unsafe {
            for key in r.keys.drain(..) {
                UnregisterEventHotKey(key);
            }
            r.actions.clear();
            if bindings.global().is_empty() {
                return;
            }
            let target = GetApplicationEventTarget();
            if !r.handler {
                let pressed = EventTypeSpec {
                    class: four(b"keyb"),
                    kind: 5,
                }; // kEventHotKeyPressed
                r.handler = InstallEventHandler(
                    target,
                    on_hot_key,
                    1,
                    &pressed,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) == 0;
            }
            for (trigger, action) in bindings.global() {
                let mut key = std::ptr::null_mut();
                let id = EventHotKeyId {
                    signature: four(b"mtty"),
                    id: r.actions.len() as u32,
                };
                let registered = key_code(trigger.key).is_some_and(|code| {
                    RegisterEventHotKey(code, carbon_mods(trigger.mods), id, target, 0, &mut key)
                        == 0
                });
                if registered {
                    r.keys.push(key);
                    r.actions.push(action.clone());
                } else {
                    failed.push(*trigger);
                }
            }
        }
    });
    failed
}

thread_local! {
    static PERFORM: std::cell::Cell<Option<fn(&Action)>> = const { std::cell::Cell::new(None) };
}

extern "C" fn on_hot_key(_: *mut c_void, event: *mut c_void, _: *mut c_void) -> i32 {
    let mut id = EventHotKeyId::default();
    // SAFETY: a hot key event, whose direct object is an EventHotKeyID.
    let status = unsafe {
        let (name, kind) = (four(b"----"), four(b"hkid")); // kEventParamDirectObject, typeEventHotKeyID
        GetEventParameter(
            event,
            name,
            kind,
            std::ptr::null_mut(),
            size_of::<EventHotKeyId>(),
            std::ptr::null_mut(),
            (&raw mut id).cast(),
        )
    };
    if status != 0 || id.signature != four(b"mtty") {
        return -9874; // eventNotHandledErr
    }
    let action = REGISTERED.with_borrow(|r| r.actions.get(id.id as usize).cloned());
    if let (Some(action), Some(perform)) = (action, PERFORM.get()) {
        perform(&action);
    }
    0
}

fn carbon_mods(mods: u8) -> u32 {
    let mut bits = 0;
    for (ours, carbon) in [(CMD, 0x100), (SHIFT, 0x200), (ALT, 0x800), (CTRL, 0x1000)] {
        if mods & ours != 0 {
            bits |= carbon;
        }
    }
    bits
}

/// The virtual key code of `key` on a US keyboard.
fn key_code(key: Key) -> Option<u32> {
    let named = |n: Named| match n {
        Named::Enter => 36,
        Named::Tab => 48,
        Named::Space => 49,
        Named::Backspace => 51,
        Named::Delete => 117,
        Named::Escape => 53,
        Named::Up => 126,
        Named::Down => 125,
        Named::Left => 123,
        Named::Right => 124,
        Named::Home => 115,
        Named::End => 119,
        Named::PageUp => 116,
        Named::PageDown => 121,
        Named::F(n) => [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111][n as usize - 1],
    };
    match key {
        Key::Named(n) => Some(named(n)),
        Key::Char(c) => {
            const LAYOUT: &str = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.\0\0`";
            LAYOUT
                .chars()
                .position(|k| k == c && k != '\0')
                .map(|i| i as u32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes() {
        let code = |s: &str| key_code(Trigger::parse(s).unwrap().key);
        assert_eq!(code("a"), Some(0));
        assert_eq!(code("b"), Some(11));
        assert_eq!(code("1"), Some(18));
        assert_eq!(code("grave_accent"), Some(50));
        assert_eq!(code("space"), Some(49));
        assert_eq!(code("f12"), Some(111));
        assert_eq!(code("é"), None);
        assert_eq!(carbon_mods(CMD | CTRL), 0x1100);
    }
}
