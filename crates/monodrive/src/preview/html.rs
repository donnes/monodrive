//! An HTML file in a `WKWebView`, treated as untrusted input: JavaScript off, no
//! website data kept, every request but `file:` blocked, read access limited to
//! its own folder, and no navigation but the first load.

use crate::native_view::Host;
use block2::RcBlock;
use gpui::Window;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSRect, NSString, NSURL};
use objc2_web_kit::{
    WKContentRuleList, WKContentRuleListStore, WKNavigationAction, WKNavigationActionPolicy,
    WKNavigationDelegate, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
};
use std::cell::Cell;
use std::path::Path;

/// Blocks every request, then lets `file:` back in: read access limits those.
const OFFLINE: &str = r#"[
    {"trigger": {"url-filter": ".*"}, "action": {"type": "block"}},
    {"trigger": {"url-filter": "^file:"}, "action": {"type": "ignore-previous-rules"}}
]"#;

/// The file allowed, and whether its one load has happened.
struct GuardState {
    file: String,
    loaded: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Guard` doesn't
    // implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "MonodrivePreviewNavigationGuard"]
    #[thread_kind = MainThreadOnly]
    #[ivars = GuardState]
    struct Guard;

    unsafe impl NSObjectProtocol for Guard {}

    unsafe impl WKNavigationDelegate for Guard {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        unsafe fn decide(
            &self,
            _: &WKWebView,
            action: &WKNavigationAction,
            reply: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: the action is valid during this call.
            let url = unsafe { action.request().URL() }.and_then(|url| url.absoluteString());
            // SAFETY: as above; a null target is a new window.
            let main_frame =
                unsafe { action.targetFrame() }.is_some_and(|frame| unsafe { frame.isMainFrame() });
            let state = self.ivars();
            let allowed = url.is_some_and(|url| {
                allowed(
                    &url.to_string(),
                    &state.file,
                    main_frame,
                    state.loaded.get(),
                )
            });
            if allowed {
                state.loaded.set(true);
            }
            reply.call((if allowed {
                WKNavigationActionPolicy::Allow
            } else {
                WKNavigationActionPolicy::Cancel
            },));
        }
    }
);

/// Only the first load of the file being previewed, in the main frame: a page
/// can't reload itself, embed itself or go anywhere else.
fn allowed(url: &str, file: &str, main_frame: bool, loaded: bool) -> bool {
    !loaded && main_frame && url == file
}

pub struct Page {
    pub host: Host<WKWebView>,
    _guard: Retained<Guard>,
}

impl Page {
    /// `None` off the main thread, as in tests. The page loads once the rules
    /// that keep it offline are in place, and never without them.
    pub fn new(path: &Path, window: &Window) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let folder = path.parent()?;
        let folder = NSURL::fileURLWithPath_isDirectory(
            &NSString::from_str(&folder.to_string_lossy()),
            true,
        );
        let address = file.absoluteString()?.to_string();
        let guard = Guard::alloc(mtm).set_ivars(GuardState {
            file: address,
            loaded: Cell::new(false),
        });
        // SAFETY: NSObject's designated initializer.
        let guard: Retained<Guard> = unsafe { msg_send![super(guard), init] };
        // SAFETY: on the main thread, and the block below keeps what it uses.
        unsafe {
            let config = WKWebViewConfiguration::new(mtm);
            config
                .defaultWebpagePreferences()
                .setAllowsContentJavaScript(false);
            config
                .preferences()
                .setJavaScriptCanOpenWindowsAutomatically(false);
            config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
            let web = WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::ZERO,
                &config,
            );
            web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*guard)));
            let loading = web.clone();
            let compiled = RcBlock::new(move |rules: *mut WKContentRuleList, _: *mut NSError| {
                // WebKit passes a valid list or null, on the main thread.
                let Some(rules) = rules.as_ref() else {
                    return;
                };
                loading
                    .configuration()
                    .userContentController()
                    .addContentRuleList(rules);
                loading.loadFileURL_allowingReadAccessToURL(&file, &folder);
            });
            WKContentRuleListStore::defaultStore(mtm)?
                .compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                    Some(&NSString::from_str("monodrive-preview-offline")),
                    Some(&NSString::from_str(OFFLINE)),
                    Some(&compiled),
                );
            Some(Self {
                host: Host::new(web, true, window)?,
                _guard: guard,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_load_of_the_file_itself_is_allowed() {
        let file = "file:///d/page.html";
        assert!(allowed(file, file, true, false));
        // A reload, or the page embedding itself, is refused.
        assert!(!allowed(file, file, true, true));
        assert!(!allowed(file, file, false, false));
        // So is anywhere else.
        for other in [
            "file:///d/other.html",
            "file:///etc/hosts",
            "https://example.com/",
            "about:blank",
        ] {
            assert!(!allowed(other, file, true, false), "{other}");
        }
    }

    #[test]
    fn the_rules_are_json_that_ends_on_letting_files_through() {
        let rules: serde_json::Value = serde_json::from_str(OFFLINE).unwrap();
        assert_eq!(rules[0]["action"]["type"], "block");
        assert_eq!(rules[1]["trigger"]["url-filter"], "^file:");
    }
}
