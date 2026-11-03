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
    unsafe { msg!(class(c"UNUserNotificationCenter"), c"currentNotificationCenter") }
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

/// Asks once for permission to show banners; macOS remembers the answer.
fn request_permission() {
    static ASKED: std::sync::Once = std::sync::Once::new();
    // UNAuthorizationOptionSound | UNAuthorizationOptionAlert.
    // SAFETY: documented signature; the handler is a global block.
    ASKED.call_once(|| unsafe { call!(center(), c"requestAuthorizationWithOptions:completionHandler:", 2usize | 4, &AUTHORIZED) });
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
        let info: Id = msg!(class(c"NSDictionary"), c"dictionaryWithObject:forKey:", number, nsstring("pane"));
        call!(content, c"setUserInfo:", info);
        let request: Id = msg!(
            class(c"UNNotificationRequest"),
            c"requestWithIdentifier:content:trigger:",
            identifier(pane),
            content,
            NIL
        );
        call!(center(), c"addNotificationRequest:withCompletionHandler:", request, NIL);
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
        call!(center(), c"removeDeliveredNotificationsWithIdentifiers:", ids);
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

#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}
