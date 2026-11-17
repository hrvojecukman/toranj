//! Native macOS notifications (UserNotifications), posted as Toranj itself.
//!
//! The notification plugin goes through the deprecated NSUserNotification
//! API, which current macOS drops without a word. Here notifications carry
//! the tab they are about; clicking one emits `notification-click` with it.

#[cfg(target_os = "macos")]
mod mac {
    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AllocAnyThread};
    use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
        UNNotificationRequest, UNNotificationResponse, UNNotificationSettings, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };
    use std::sync::OnceLock;
    use tauri::{AppHandle, Emitter, Manager};

    static APP: OnceLock<AppHandle> = OnceLock::new();

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "ToranjNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            // Show it even while Toranj is in front: Toranj only notifies
            // about tabs that are not on screen. The app plays the sound.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                handler: &DynBlock<dyn Fn()>,
            ) {
                let clicked = response.actionIdentifier().isEqualToString(unsafe {
                    UNNotificationDefaultActionIdentifier
                });
                if clicked {
                    let info = response.notification().request().content().userInfo();
                    let tab = info
                        .objectForKey(&NSString::from_str("tab"))
                        .and_then(|v| v.downcast::<NSString>().ok())
                        .map(|s| s.to_string());
                    if let (Some(tab), Some(app)) = (tab, APP.get()) {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.unminimize();
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                        let _ = app.emit("notification-click", tab);
                    }
                }
                handler.call(());
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            unsafe { msg_send![super(this), init] }
        }
    }

    /// UNUserNotificationCenter raises an exception outside an app bundle
    /// (e.g. `tauri dev`, tests).
    fn bundled() -> bool {
        let b = NSBundle::mainBundle();
        b.bundleIdentifier().is_some() && b.bundlePath().to_string().ends_with(".app")
    }

    pub fn init(app: &AppHandle) -> bool {
        if !bundled() {
            return false;
        }
        let _ = APP.set(app.clone());
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = Delegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center keeps only a weak reference.
        std::mem::forget(delegate);
        let done = RcBlock::new(|ok: Bool, err: *mut NSError| {
            let why = unsafe { err.as_ref() }
                .map(|e| format!(": {}", e.localizedDescription()))
                .unwrap_or_default();
            crate::log_line(&format!("notifications: allowed={}{why}", ok.as_bool()));
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Badge,
            &done,
        );
        true
    }

    /// Logs what macOS says about Toranj's notifications: its settings and how
    /// many it currently shows.
    pub fn report() {
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let settings = RcBlock::new(|s: std::ptr::NonNull<UNNotificationSettings>| {
            let s = unsafe { s.as_ref() };
            crate::log_line(&format!(
                "notification posted; settings: auth={} alert={} center={} style={}",
                s.authorizationStatus().0,
                s.alertSetting().0,
                s.notificationCenterSetting().0,
                s.alertStyle().0
            ));
        });
        center.getNotificationSettingsWithCompletionHandler(&settings);
        let delivered = RcBlock::new(|list: std::ptr::NonNull<NSArray<UNNotification>>| {
            crate::log_line(&format!("notifications shown: {}", unsafe { list.as_ref() }.count()));
        });
        center.getDeliveredNotificationsWithCompletionHandler(&delivered);
    }

    pub fn post(tab: &str, title: &str, body: &str) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setThreadIdentifier(&NSString::from_str(tab));
        let key = NSString::from_str("tab");
        let val = NSString::from_str(tab);
        let info = NSDictionary::<NSString, AnyObject>::from_slices(
            &[&*key],
            &[val.as_ref() as &AnyObject],
        );
        unsafe { content.setUserInfo(info.cast_unchecked()) };
        // One notification per tab: a newer one replaces the older.
        let req = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&format!("tab:{tab}")),
            &content,
            None,
        );
        let done = RcBlock::new(|err: *mut NSError| {
            match unsafe { err.as_ref() } {
                Some(e) => crate::log_line(&format!("notification not posted: {}", e.localizedDescription())),
                None => report(),
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&req, Some(&done));
    }
}

use std::sync::atomic::{AtomicBool, Ordering};

static NATIVE: AtomicBool = AtomicBool::new(false);

pub fn init(_app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let ok = mac::init(_app);
        NATIVE.store(ok, Ordering::Relaxed);
        if !ok {
            crate::log_line("notifications: not an app bundle, using the plugin");
        }
    }
}

/// Posts a native notification; false when unavailable (then the frontend
/// falls back to the notification plugin).
#[tauri::command]
pub fn notify_native(tab: String, title: String, body: String) -> bool {
    if !NATIVE.load(Ordering::Relaxed) {
        return false;
    }
    #[cfg(target_os = "macos")]
    mac::post(&tab, &title, &body);
    let _ = (&tab, &title, &body);
    true
}
