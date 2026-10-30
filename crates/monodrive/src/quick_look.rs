//! Quick Look through AppKit's shared `QLPreviewPanel`, which previews
//! whatever the first responder accepting control of it hands over. A small
//! responder after the window does that: it gives the panel the files' URLs,
//! and passes back the keys the panel doesn't use, so ↑ and ↓ still move
//! the selection. The panel loads previews itself, off the main thread.

use crate::selection::Selection;
use crate::traffic_lights;
use monodrive_core::entries::{Entry, Kind};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSEventType, NSResponder, NSWindow, NSWindowDelegate};
use objc2_foundation::{NSInteger, NSString, NSURL};
use objc2_quick_look_ui::{
    QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource, QLPreviewPanelDelegate,
};
use std::cell::RefCell;
use std::path::PathBuf;

/// What the panel shows: the selection in the order shown, each path with
/// whether it's a folder, and the one in front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub items: Vec<(PathBuf, bool)>,
    pub index: usize,
}

impl Preview {
    /// The selection's preview, in front on the cursor's entry when that's
    /// selected; `None` with nothing selected.
    pub fn of(selection: &Selection, entries: &[Entry]) -> Option<Self> {
        let item = |entry: &Entry| (entry.path.clone(), entry.kind == Kind::Dir);
        // One entry is the usual case, and needn't look up every row.
        if let Some(only) = selection.only() {
            let entry = entries.iter().find(|entry| entry.path == only)?;
            return Some(Self {
                items: vec![item(entry)],
                index: 0,
            });
        }
        let selected = selection.in_order(entries);
        let index = selection
            .cursor()
            .and_then(|cursor| selected.iter().position(|entry| entry.path == cursor))
            .unwrap_or(0);
        (!selected.is_empty()).then(|| Self {
            items: selected.into_iter().map(item).collect(),
            index,
        })
    }
}

/// The panel, controlled from one window.
pub struct Panel {
    window: Retained<NSWindow>,
    controller: Retained<Controller>,
    mtm: MainThreadMarker,
}

impl Panel {
    /// `None` without an AppKit window, as in tests.
    pub fn new(window: &gpui::Window) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let ns_window = traffic_lights::ns_window(window)?;
        let controller = Controller::new(&ns_window, mtm);
        // SAFETY: `Drop` takes the controller out of the chain before it goes.
        unsafe {
            controller.setNextResponder(ns_window.nextResponder().as_deref());
            ns_window.setNextResponder(Some(&controller));
        }
        Some(Self {
            window: ns_window,
            controller,
            mtm,
        })
    }

    /// Shows `preview`, opening the panel if it's closed.
    pub fn show(&self, preview: &Preview) {
        let urls = preview
            .items
            .iter()
            .map(|(path, folder)| {
                let path = NSString::from_str(&path.to_string_lossy());
                // Saying whether it's a folder keeps NSURL from asking the disk.
                NSURL::fileURLWithPath_isDirectory(&path, *folder)
            })
            .collect();
        *self.controller.ivars().urls.borrow_mut() = urls;
        let open = self.is_open();
        // SAFETY: the controller is in the main window's chain, so it's the
        // one the panel finds, and it sets itself as data source and delegate.
        let Some(panel) = (unsafe { QLPreviewPanel::sharedPreviewPanel(self.mtm) }) else {
            return;
        };
        unsafe {
            if !open {
                panel.updateController();
            }
            panel.reloadData();
            panel.setCurrentPreviewItemIndex(preview.index as NSInteger);
        }
        if !open {
            panel.makeKeyAndOrderFront(None);
        }
    }

    /// Whether the panel is up on this window's files. It also closes
    /// itself, on Esc or its close button.
    pub fn is_open(&self) -> bool {
        self.shown().is_some()
    }

    pub fn close(&self) {
        if let Some(panel) = self.shown() {
            panel.orderOut(None);
        }
    }

    fn shown(&self) -> Option<Retained<QLPreviewPanel>> {
        // SAFETY: asking whether it exists doesn't make one.
        if !unsafe { QLPreviewPanel::sharedPreviewPanelExists(self.mtm) } {
            return None;
        }
        let panel = unsafe { QLPreviewPanel::sharedPreviewPanel(self.mtm) }?;
        let ours = Retained::as_ptr(&self.controller).cast::<AnyObject>();
        let controlled = unsafe { panel.currentController() }
            .is_some_and(|controller| std::ptr::eq(Retained::as_ptr(&controller), ours));
        (controlled && panel.isVisible()).then_some(panel)
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.close();
        // SAFETY: the window and controller are still alive; the panel holds
        // the controller unretained, so it lets go once it's out of the chain.
        unsafe {
            let ours = Retained::as_ptr(&self.controller).cast::<NSResponder>();
            if self
                .window
                .nextResponder()
                .is_some_and(|next| std::ptr::eq(Retained::as_ptr(&next), ours))
            {
                self.window
                    .setNextResponder(self.controller.nextResponder().as_deref());
            }
            if QLPreviewPanel::sharedPreviewPanelExists(self.mtm)
                && let Some(panel) = QLPreviewPanel::sharedPreviewPanel(self.mtm)
            {
                panel.updateController();
            }
        }
    }
}

struct Ivars {
    urls: RefCell<Vec<Retained<NSURL>>>,
    window: Weak<NSWindow>,
}

define_class!(
    // SAFETY: NSResponder has no subclassing requirements, and `Controller`
    // doesn't implement `Drop`.
    #[unsafe(super(NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[name = "MonodriveQuickLookController"]
    #[ivars = Ivars]
    struct Controller;

    impl Controller {
        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_control(&self, _panel: Option<&QLPreviewPanel>) -> bool {
            true
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin_control(&self, panel: Option<&QLPreviewPanel>) {
            if let Some(panel) = panel {
                let delegate: &AnyObject = self.as_ref();
                // SAFETY: `Panel` keeps the controller alive while it's in
                // the chain, and the panel ends control before it leaves.
                unsafe {
                    panel.setDataSource(Some(ProtocolObject::from_ref(self)));
                    panel.setDelegate(Some(delegate));
                }
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end_control(&self, panel: Option<&QLPreviewPanel>) {
            if let Some(panel) = panel {
                // SAFETY: clearing them is always allowed.
                unsafe {
                    panel.setDataSource(None);
                    panel.setDelegate(None);
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSWindowDelegate for Controller {}

    unsafe impl QLPreviewPanelDataSource for Controller {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn count(&self, _panel: Option<&QLPreviewPanel>) -> NSInteger {
            self.ivars().urls.borrow().len() as NSInteger
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        fn item_at(
            &self,
            _panel: Option<&QLPreviewPanel>,
            index: NSInteger,
        ) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
            self.item(index)
        }
    }

    unsafe impl QLPreviewPanelDelegate for Controller {
        // The keys the panel leaves, ↑ and ↓ among them, go to the window
        // as if typed there, as Finder's do.
        #[unsafe(method(previewPanel:handleEvent:))]
        fn handle_event(
            &self,
            _panel: Option<&QLPreviewPanel>,
            event: Option<&NSEvent>,
        ) -> bool {
            event.is_some_and(|event| self.forward(event))
        }
    }
);

impl Controller {
    fn new(window: &NSWindow, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            urls: RefCell::default(),
            window: Weak::from(window),
        });
        // SAFETY: NSResponder's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn item(&self, index: NSInteger) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
        let urls = self.ivars().urls.borrow();
        let url = urls.get(usize::try_from(index).ok()?)?.clone();
        Some(ProtocolObject::from_retained(url))
    }

    fn forward(&self, event: &NSEvent) -> bool {
        if event.r#type() != NSEventType::KeyDown {
            return false;
        }
        let Some(responder) = self.ivars().window.load().and_then(|w| w.firstResponder()) else {
            return false;
        };
        responder.keyDown(event);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::ClassType;
    use objc2::runtime::AnyProtocol;

    fn listing(names: &[&str]) -> Vec<Entry> {
        names
            .iter()
            .map(|name| Entry {
                path: PathBuf::from("/x").join(name),
                name: (*name).into(),
                kind: if name.contains('.') {
                    Kind::File
                } else {
                    Kind::Dir
                },
                size: 0,
                modified: 0,
            })
            .collect()
    }

    fn names(preview: Option<Preview>) -> Option<(Vec<String>, usize)> {
        let preview = preview?;
        let names = preview
            .items
            .iter()
            .map(|(path, folder)| {
                let name = path.file_name().unwrap().to_string_lossy();
                if *folder {
                    format!("{name}/")
                } else {
                    name.into()
                }
            })
            .collect();
        Some((names, preview.index))
    }

    #[test]
    fn the_controller_answers_quick_look_uis_selectors() {
        // Registering the class checks, in debug builds, that each selector
        // is one the protocol declares and none it requires is missing.
        let class = Controller::class();
        for name in [c"QLPreviewPanelDataSource", c"QLPreviewPanelDelegate"] {
            assert!(
                class.conforms_to(AnyProtocol::get(name).unwrap()),
                "{name:?}"
            );
        }
    }

    #[test]
    fn it_previews_the_selection_in_order_from_the_cursor() {
        let entries = listing(&["docs", "a.txt", "b.txt", "c.txt"]);
        let mut selection = Selection::default();
        let preview = |selection: &Selection| names(Preview::of(selection, &entries));
        assert_eq!(preview(&selection), None);
        selection.select(&entries[0]);
        assert_eq!(preview(&selection), Some((vec!["docs/".into()], 0)));
        // Several page in the order shown, starting where the cursor is.
        selection.select(&entries[1]);
        selection.extend(&entries, 3);
        let several = || vec!["a.txt".into(), "b.txt".into(), "c.txt".into()];
        assert_eq!(preview(&selection), Some((several(), 2)));
        // ⌘-clicking one out leaves the cursor on it, so the first is in front.
        selection.toggle(&entries[2]);
        assert_eq!(
            preview(&selection),
            Some((vec!["a.txt".into(), "c.txt".into()], 0))
        );
        selection.toggle(&entries[2]);
        assert_eq!(preview(&selection), Some((several(), 1)));
    }
}
