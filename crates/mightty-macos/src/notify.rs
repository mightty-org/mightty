//! Notification Center banners for panes that need the user while mightty
//! is in the background. macOS delivers them only to apps in a bundle (see
//! `scripts/bundle.sh`); run from a bare binary, there are none.

use crate::ffi::*;

/// Whether this process is an app bundle, so banners can be delivered.
pub fn available() -> bool {
    // SAFETY: documented signatures.
    unsafe {
        let bundle: Id = msg!(class(c"NSBundle"), c"mainBundle");
        let id: Id = msg!(bundle, c"bundleIdentifier");
        !id.is_null() && !class(c"UNUserNotificationCenter").is_null()
    }
}

unsafe fn center() -> Id {
    unsafe {
        msg!(
            class(c"UNUserNotificationCenter"),
            c"currentNotificationCenter"
        )
    }
}

/// Makes `delegate` receive clicks on banners
/// (`userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:`).
pub fn set_delegate(delegate: Id) {
    if available() {
        // SAFETY: main thread; documented signature.
        unsafe { call!(center(), c"setDelegate:", delegate) };
    }
}

static AUTHORIZED: Block = Block::with_flag(authorized);

unsafe extern "C" fn authorized(_: *const Block, _granted: Bool, _error: Id) {
    // Nothing to do either way: the user decides in System Settings.
}

/// Asks once for permission to show banners and the count on the Dock
/// icon, which macOS hides without it; macOS remembers the answer.
pub fn request_permission() {
    if !available() {
        return;
    }
    static ASKED: std::sync::Once = std::sync::Once::new();
    // UNAuthorizationOptionBadge | UNAuthorizationOptionSound | UNAuthorizationOptionAlert.
    // SAFETY: documented signature; the handler is a global block.
    ASKED.call_once(|| unsafe {
        call!(
            center(),
            c"requestAuthorizationWithOptions:completionHandler:",
            1usize | 2 | 4,
            &AUTHORIZED
        )
    });
}

fn identifier(pane: u64) -> Id {
    nsstring(&format!("pane-{pane}"))
}

/// Shows a banner for `pane`, replacing any it had.
pub fn post(pane: u64, title: &str, body: &str) {
    if !available() {
        return;
    }
    request_permission();
    // SAFETY: main thread; documented signatures.
    unsafe {
        let content: Id = msg!(new_object(c"UNMutableNotificationContent"), c"autorelease");
        call!(content, c"setTitle:", nsstring(title));
        call!(content, c"setBody:", nsstring(body));
        let sound: Id = msg!(class(c"UNNotificationSound"), c"defaultSound");
        call!(content, c"setSound:", sound);
        let number: Id = msg!(class(c"NSNumber"), c"numberWithUnsignedLongLong:", pane);
        let info: Id = msg!(
            class(c"NSDictionary"),
            c"dictionaryWithObject:forKey:",
            number,
            nsstring("pane")
        );
        call!(content, c"setUserInfo:", info);
        let request: Id = msg!(
            class(c"UNNotificationRequest"),
            c"requestWithIdentifier:content:trigger:",
            identifier(pane),
            content,
            NIL
        );
        call!(
            center(),
            c"addNotificationRequest:withCompletionHandler:",
            request,
            NIL
        );
    }
}

/// Takes `pane`'s banner back, from the screen and Notification Center.
pub fn remove(pane: u64) {
    if !available() {
        return;
    }
    // SAFETY: main thread; documented signatures.
    unsafe {
        let ids: Id = msg!(class(c"NSArray"), c"arrayWithObject:", identifier(pane));
        call!(
            center(),
            c"removeDeliveredNotificationsWithIdentifiers:",
            ids
        );
    }
}

/// The pane a clicked banner (`response`, a UNNotificationResponse) was for.
pub fn pane_of(response: Id) -> Option<u64> {
    // SAFETY: documented signatures.
    unsafe {
        let request: Id = msg!(msg!(response, c"notification"), c"request");
        let info: Id = msg!(msg!(request, c"content"), c"userInfo");
        let number: Id = msg!(info, c"objectForKey:", nsstring("pane"));
        (!number.is_null()).then(|| msg!(number, c"unsignedLongLongValue"))
    }
}

/// A banner a program asked for with OSC 99, as clicks report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rich {
    pub pane: u64,
    /// Its identifier, or one made up for it (see `key`).
    pub key: String,
    /// The identifier the program gave it.
    pub id: Option<String>,
    pub focus: bool,
    pub report: bool,
    pub report_close: bool,
}

impl Rich {
    fn identifier(&self) -> Id {
        nsstring(&format!("rich-{}-{}", self.pane, self.key))
    }
}

/// What was done to a banner a program asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    Clicked,
    /// One of its buttons, from 1.
    Button(usize),
    Dismissed,
}

thread_local! {
    /// The categories made known to macOS: their names and button sets.
    static CATEGORIES: std::cell::RefCell<Vec<(String, Vec<String>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The name of the category for banners with `buttons`: the same for the
/// same labels, so banners already up keep theirs.
pub fn category_name(buttons: &[String]) -> String {
    // FNV-1a over the labels, each ended by a byte they can't contain.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in buttons.iter().flat_map(|b| b.bytes().chain([0])) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    format!("rich-{hash:016x}")
}

/// Makes the category for `buttons` known to macOS, keeping those in
/// `in_use` (the categories of banners still up) and dropping the rest.
/// Every one asks to hear of dismissals, to report closing.
pub fn use_category(buttons: &[String], in_use: &[&str]) {
    if !available() {
        return;
    }
    let name = category_name(buttons);
    CATEGORIES.with_borrow_mut(|sets| {
        if sets.iter().any(|(n, _)| *n == name) {
            return;
        }
        sets.retain(|(n, _)| in_use.contains(&n.as_str()));
        sets.push((name, buttons.to_vec()));
        // SAFETY: main thread; documented signatures.
        unsafe {
            let all: Id = msg!(new_object(c"NSMutableSet"), c"autorelease");
            for (name, set) in sets.iter() {
                let actions: Id = msg!(new_object(c"NSMutableArray"), c"autorelease");
                for (n, label) in set.iter().enumerate() {
                    let action: Id = msg!(
                        class(c"UNNotificationAction"),
                        c"actionWithIdentifier:title:options:",
                        nsstring(&format!("button-{}", n + 1)),
                        nsstring(label),
                        0usize
                    );
                    call!(actions, c"addObject:", action);
                }
                let empty: Id = msg!(class(c"NSArray"), c"array");
                let category: Id = msg!(
                    class(c"UNNotificationCategory"),
                    c"categoryWithIdentifier:actions:intentIdentifiers:options:",
                    nsstring(name),
                    actions,
                    empty,
                    1usize // UNNotificationCategoryOptionCustomDismissAction
                );
                call!(all, c"addObject:", category);
            }
            call!(center(), c"setNotificationCategories:", all);
        }
    });
}

/// Shows a banner a program asked for, replacing one with the same key.
pub fn post_rich(banner: &Rich, title: &str, body: &str, details: &mightty_term::Rich) {
    if !available() {
        return;
    }
    request_permission();
    // SAFETY: main thread; documented signatures.
    unsafe {
        let content: Id = msg!(new_object(c"UNMutableNotificationContent"), c"autorelease");
        call!(content, c"setTitle:", nsstring(title));
        call!(content, c"setBody:", nsstring(body));
        if !details.silent {
            let sound: Id = msg!(class(c"UNNotificationSound"), c"defaultSound");
            call!(content, c"setSound:", sound);
        }
        // Passive, active and time-sensitive; the last needs an
        // entitlement, without which macOS treats it as active.
        let level: usize = match details.urgency {
            mightty_term::Urgency::Low => 0,
            mightty_term::Urgency::Normal => 1,
            mightty_term::Urgency::Critical => 2,
        };
        let level_sel = sel(c"setInterruptionLevel:");
        let responds: Bool = msg!(content, c"respondsToSelector:", level_sel);
        if responds != NO {
            call!(content, c"setInterruptionLevel:", level);
        }
        call!(
            content,
            c"setCategoryIdentifier:",
            nsstring(&category_name(&details.buttons))
        );
        let info: Id = msg!(new_object(c"NSMutableDictionary"), c"autorelease");
        let number = |n: u64| -> Id { msg!(class(c"NSNumber"), c"numberWithUnsignedLongLong:", n) };
        call!(
            info,
            c"setObject:forKey:",
            number(banner.pane),
            nsstring("rich-pane")
        );
        call!(
            info,
            c"setObject:forKey:",
            nsstring(&banner.key),
            nsstring("key")
        );
        if let Some(id) = &banner.id {
            call!(info, c"setObject:forKey:", nsstring(id), nsstring("id"));
        }
        for (flag, name) in [
            (banner.focus, "focus"),
            (banner.report, "report"),
            (banner.report_close, "report-close"),
        ] {
            call!(
                info,
                c"setObject:forKey:",
                number(flag.into()),
                nsstring(name)
            );
        }
        call!(content, c"setUserInfo:", info);
        let request: Id = msg!(
            class(c"UNNotificationRequest"),
            c"requestWithIdentifier:content:trigger:",
            banner.identifier(),
            content,
            NIL
        );
        call!(
            center(),
            c"addNotificationRequest:withCompletionHandler:",
            request,
            NIL
        );
    }
}

/// Takes a banner a program asked for back.
pub fn remove_rich(banner: &Rich) {
    if !available() {
        return;
    }
    // SAFETY: main thread; documented signatures.
    unsafe {
        let ids: Id = msg!(class(c"NSArray"), c"arrayWithObject:", banner.identifier());
        call!(
            center(),
            c"removeDeliveredNotificationsWithIdentifiers:",
            ids
        );
        call!(
            center(),
            c"removePendingNotificationRequestsWithIdentifiers:",
            ids
        );
    }
}

/// The banner a program asked for that `response` (a
/// UNNotificationResponse) was about, and what was done to it.
pub fn rich_of(response: Id) -> Option<(Rich, Response)> {
    // SAFETY: documented signatures; userInfo is ours, made in post_rich.
    unsafe {
        let request: Id = msg!(msg!(response, c"notification"), c"request");
        let info: Id = msg!(msg!(request, c"content"), c"userInfo");
        let get = |key: &str| -> Id { msg!(info, c"objectForKey:", nsstring(key)) };
        let pane = get("rich-pane");
        if pane.is_null() {
            return None;
        }
        let flag = |key: &str| {
            let n = get(key);
            let value: u64 = if n.is_null() {
                0
            } else {
                msg!(n, c"unsignedLongLongValue")
            };
            value != 0
        };
        let string = |key: &str| {
            let s = get(key);
            (!s.is_null()).then(|| string_from_ns(s))
        };
        let banner = Rich {
            pane: msg!(pane, c"unsignedLongLongValue"),
            key: string("key")?,
            id: string("id"),
            focus: flag("focus"),
            report: flag("report"),
            report_close: flag("report-close"),
        };
        let action = string_from_ns(msg!(response, c"actionIdentifier"));
        let what = match action.as_str() {
            "com.apple.UNNotificationDismissActionIdentifier" => Response::Dismissed,
            _ => match action.strip_prefix("button-").and_then(|n| n.parse().ok()) {
                Some(n) => Response::Button(n),
                None => Response::Clicked,
            },
        };
        Some((banner, what))
    }
}

#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}
