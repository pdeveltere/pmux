//! macOS notifications: an agent that needs you, or finished, while you look elsewhere posts one;
//! clicking it shows that pane. The Dock badge counts agents waiting for you.
//!
//! One notification per pane at most (its identifier is the pane's), replaced by the next and
//! taken away once you look at the pane. Only inside the app bundle: Notification Center needs
//! a bundle identifier (and throws without one).
use crate::app::{Ev, Tx};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{declare_class, msg_send, msg_send_id, mutability, ClassType, DeclaredClass};
use objc2_foundation::{MainThreadMarker, NSArray, NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use std::sync::OnceLock;

static PROXY: OnceLock<Tx> = OnceLock::new();

declare_class!(
    struct Delegate;

    unsafe impl ClassType for Delegate {
        type Super = NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "PmuxNotifyDelegate";
    }

    impl DeclaredClass for Delegate {}

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        // Show it even while pmux is the active app (you may be looking at another pane).
        #[method(userNotificationCenter:willPresentNotification:withCompletionHandler:)]
        unsafe fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _n: &UNNotification,
            done: &block2::Block<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            done.call((UNNotificationPresentationOptions::UNNotificationPresentationOptionBanner
                | UNNotificationPresentationOptions::UNNotificationPresentationOptionList
                | UNNotificationPresentationOptions::UNNotificationPresentationOptionSound,));
        }

        #[method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:)]
        unsafe fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            r: &UNNotificationResponse,
            done: &block2::Block<dyn Fn()>,
        ) {
            let ident = unsafe { r.notification().request().identifier() }.to_string();
            if let (Some(p), Some(id)) = (PROXY.get(), pane_of(&ident)) {
                let _ = p.send_event(Ev::ShowPane(id));
            }
            done.call(());
        }
    }
);

fn ident(pane: u32) -> Retained<NSString> {
    NSString::from_str(&format!("pmux-pane-{pane}"))
}

fn pane_of(ident: &str) -> Option<u32> {
    ident.strip_prefix("pmux-pane-")?.parse().ok()
}

fn center() -> Option<Retained<UNUserNotificationCenter>> {
    PROXY.get()?;
    Some(unsafe { UNUserNotificationCenter::currentNotificationCenter() })
}

/// Ask for permission (macOS asks the user once) and route clicks to the app. Call once launched.
pub fn install(proxy: Tx) {
    if unsafe { NSBundle::mainBundle().bundleIdentifier() }.is_none() || PROXY.set(proxy).is_err() {
        return;
    }
    let c = unsafe { UNUserNotificationCenter::currentNotificationCenter() };
    let d: Retained<Delegate> = unsafe { msg_send_id![Delegate::alloc(), init] };
    unsafe {
        c.setDelegate(Some(ProtocolObject::from_ref(&*d)));
        let opts =
            UNAuthorizationOptions::UNAuthorizationOptionAlert | UNAuthorizationOptions::UNAuthorizationOptionSound;
        let cb = block2::RcBlock::new(|_ok: Bool, _e: *mut NSError| {});
        c.requestAuthorizationWithOptions_completionHandler(opts, &cb);
    }
    // The center holds its delegate weakly.
    std::mem::forget(d);
}

/// Post (or replace) pane `pane`'s notification.
pub fn post(pane: u32, title: &str, body: &str) {
    let Some(c) = center() else { return };
    unsafe {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let req = UNNotificationRequest::requestWithIdentifier_content_trigger(&ident(pane), &content, None);
        c.addNotificationRequest_withCompletionHandler(&req, None);
    }
}

/// Take pane `pane`'s notification away (you looked at it, or it moved on).
pub fn remove(pane: u32) {
    let Some(c) = center() else { return };
    let ids = NSArray::from_vec(vec![ident(pane)]);
    unsafe { c.removeDeliveredNotificationsWithIdentifiers(&ids) };
}

/// The Dock badge: `n` agents need you (none: no badge).
pub fn badge(n: usize) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let label = (n > 0).then(|| NSString::from_str(&n.to_string()));
    unsafe {
        let tile: Retained<AnyObject> = msg_send_id![&app, dockTile];
        let _: () = msg_send![&tile, setBadgeLabel: label.as_deref()];
    }
}
