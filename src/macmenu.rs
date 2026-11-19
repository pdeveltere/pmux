//! macOS menu bar: adds "Settings…" (⌘,) to the pmux app menu winit builds.
use crate::app::{Ev, Tx};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{declare_class, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{NSApplication, NSMenuItem};
use objc2_foundation::{ns_string, MainThreadMarker};
use std::sync::OnceLock;

static PROXY: OnceLock<Tx> = OnceLock::new();

declare_class!(
    struct Target;

    unsafe impl ClassType for Target {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "PmuxMenuTarget";
    }

    impl DeclaredClass for Target {}

    unsafe impl Target {
        #[method(openSettings:)]
        fn open_settings(&self, _sender: *mut AnyObject) {
            if let Some(p) = PROXY.get() {
                let _ = p.send_event(Ev::OpenSettings);
            }
        }
    }
);

/// Insert the items into the app menu. Call once the app has launched (winit's menu exists then).
pub fn install(proxy: Tx) {
    if PROXY.set(proxy).is_err() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let Some(menu) = (unsafe { app.mainMenu() }) else { return };
    let Some(app_menu) = (unsafe { menu.itemAtIndex(0) }).and_then(|i| unsafe { i.submenu() }) else { return };
    let target: Retained<Target> = unsafe { msg_send_id![mtm.alloc::<Target>(), init] };
    unsafe {
        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            ns_string!("Settings…"),
            Some(sel!(openSettings:)),
            ns_string!(","),
        );
        item.setTarget(Some(&target));
        // After "About pmux" and its separator: About, —, Settings…, —, Services, …
        let at = app_menu.numberOfItems().min(2);
        app_menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), at);
        app_menu.insertItem_atIndex(&item, at);
    }
    // Menu items hold their target weakly.
    std::mem::forget(target);
}
