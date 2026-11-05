//! The in-app preview: it takes over the pane's list, with a bar above. It knows
//! nothing of where it sits, so a preview column can use it later. Every read
//! runs off the UI thread under a generation, so moving on drops a stale result.

mod html;
mod markdown;
mod media;
mod tools;
mod zoom;

pub use tools::Saved;

use crate::browser::{OpenSelected, QuickLook};
use crate::native_view::Host;
use crate::theme::{self, ButtonLook, content, text_button};
use gpui::{
    App, Asset, Bounds, Context, FontWeight, HighlightStyle, InteractiveText, ListAlignment,
    ListState, MouseButton, Pixels, RenderImage, SharedString, StyledText, Task,
    UniformListScrollHandle, Window, canvas, div, list, prelude::*, px, uniform_list,
};
use html::Page;
use markdown::{Block, Inline};
use media::{Player, clock};
use monodrive_core::archives::Archive;
use monodrive_core::cloud;
use monodrive_core::preview::{self, Kind, TEXT_LIMIT};
use monodrive_core::thumbnails;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSRect, NSString, NSURL};
use objc2_quick_look_ui::QLPreviewView;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use zoom::{Zoom, place};

/// Beyond this, text stays plain: highlighting walks the whole file.
const HIGHLIGHT_LIMIT: usize = 512 * 1024;
/// Pixels asked for at 100%, as a picture's own size isn't known beforehand.
const FULL_SIDE: u32 = 8192;
/// The side a picture decodes at to fill the window: the window's longer side
/// in pixels, from the last one drawn, or a guess from the screen before that.
static FIT_SIDE: AtomicU32 = AtomicU32::new(0);

/// One picture at one size, from ImageIO, which reads HEIC and RAW, or a video's
/// frame from the system's generator.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Sharp {
    path: PathBuf,
    side: u32,
    modified: u64,
}

impl Asset for Sharp {
    type Source = Sharp;
    type Output = Option<Arc<RenderImage>>;

    #[allow(clippy::manual_async_fn)]
    fn load(
        sharp: Sharp,
        _: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        async move {
            // A video or audio file has no picture of its own to decode.
            let bitmap = thumbnails::picture(&sharp.path, sharp.side)
                .or_else(|_| thumbnails::generate(&sharp.path, sharp.side, &|| false))
                .ok()?;
            crate::thumbnails::render_image(bitmap).map(Arc::new)
        }
    }
}

/// A line of text with its colors.
#[derive(Clone)]
struct Line {
    text: SharedString,
    spans: Vec<(std::ops::Range<usize>, HighlightStyle)>,
}

enum Body {
    Loading,
    /// Something to say in place of the file.
    Notice(String),
    Text {
        lines: Arc<Vec<Line>>,
        cut: bool,
    },
    Markdown {
        blocks: Arc<Vec<Block>>,
        source: Arc<Vec<Line>>,
    },
    /// Decoded to fill the window, along with the read, so it appears with
    /// the bar above it.
    Picture {
        modified: u64,
        image: Arc<RenderImage>,
    },
    Animated,
    Rows(Arc<Vec<String>>, Option<String>),
    System,
    Media {
        video: bool,
        modified: u64,
    },
    Html,
}

type SystemView = (Host<QLPreviewView>, PathBuf);

pub struct PreviewView {
    pub(crate) path: PathBuf,
    folder: bool,
    /// Where it is among what's selected: "3 of 12".
    position: (usize, usize),
    generation: u64,
    body: Body,
    show_source: bool,
    zoom: Zoom,
    /// Actual Size was asked for, and waits on the full decode to know it.
    want_actual: bool,
    /// Where the picture's box was drawn, to zoom about the pointer.
    frame: Rc<Cell<Bounds<Pixels>>>,
    /// A drag moving a zoomed picture: where the pointer was last.
    panning: Option<gpui::Point<Pixels>>,
    pinch: Option<(zoom::Pinch, Task<()>)>,
    /// The file being read, which takes over once it's ready, so the bar and
    /// what's under it change together. What's shown stays until then.
    loading: Option<(PathBuf, bool, (usize, usize))>,
    /// A sheet or menu is up, which a native view would draw over.
    covered: bool,
    /// The tools and what they've done so far.
    edit: Option<tools::Edit>,
    /// What the selection moved to while an edit held this file on screen.
    wanted: Option<(PathBuf, bool, (usize, usize))>,
    held: Option<(Sharp, Option<Arc<RenderImage>>)>,
    scroll: UniformListScrollHandle,
    /// The Markdown's scrolling, which can't cross threads with `Body`.
    blocks: Option<ListState>,
    /// The edit row's width, which decides what fits in it.
    row_width: Rc<Cell<f32>>,
    /// The system's view, and the file it's on.
    native: Rc<RefCell<Option<SystemView>>>,
    player: Rc<RefCell<Option<Player>>>,
    page: Rc<RefCell<Option<Page>>>,
    /// Where the scrub track was drawn, to turn a press into a time.
    track: Rc<Cell<Bounds<Pixels>>>,
    /// Redraws the strip while it plays.
    _ticking: Option<Task<()>>,
    /// How many more redraws to wait for a file's length. A broken file never
    /// has one, and mustn't repaint forever.
    loading_ticks: u32,
    _reading: Option<Task<()>>,
}

impl Default for PreviewView {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewView {
    pub fn new() -> Self {
        Self {
            path: PathBuf::new(),
            folder: false,
            position: (0, 0),
            generation: 0,
            body: Body::Loading,
            show_source: false,
            zoom: Zoom::default(),
            want_actual: false,
            frame: Rc::new(Cell::new(Bounds::default())),
            panning: None,
            pinch: None,
            loading: None,
            covered: false,
            edit: None,
            wanted: None,
            held: None,
            scroll: UniformListScrollHandle::new(),
            blocks: None,
            row_width: Rc::new(Cell::new(0.)),
            native: Rc::new(RefCell::new(None)),
            player: Rc::new(RefCell::new(None)),
            page: Rc::new(RefCell::new(None)),
            track: Rc::new(Cell::new(Bounds::default())),
            _ticking: None,
            loading_ticks: 0,
            _reading: None,
        }
    }

    /// Shows `path`; the same one again only updates where it is in the list.
    pub fn show(
        &mut self,
        path: &Path,
        folder: bool,
        position: (usize, usize),
        cx: &mut Context<Self>,
    ) {
        // Unsaved changes hold this file; the move lands when they're done with.
        if self.holding() && self.path != path {
            self.wanted = Some((path.to_path_buf(), folder, position));
            return;
        }
        self.wanted = None;
        if let Some(loading) = self.loading.as_mut().filter(|(at, ..)| at == path) {
            loading.2 = position;
            return;
        }
        // A save's reload must land even if the selection came back meanwhile,
        // as the file shown may have been replaced. While the save still writes,
        // it loads the file itself when it's done.
        if self.path == path && self.saving() && self.loading.is_some() {
            return self.load(path.to_path_buf(), folder, position, cx);
        }
        if self.path == path && !matches!(self.body, Body::Loading) {
            // Back on what's shown, nothing else is wanted.
            self.loading = None;
            self._reading = None;
            self.position = position;
            return cx.notify();
        }
        self.load(path.to_path_buf(), folder, position, cx);
    }

    /// Whether there's anything to show yet. Until then the list stays, so the
    /// preview never appears in pieces.
    pub fn ready(&self) -> bool {
        !matches!(self.body, Body::Loading)
    }

    /// Reads `path` off the UI thread; it takes over once it's ready.
    fn load(
        &mut self,
        path: PathBuf,
        folder: bool,
        position: (usize, usize),
        cx: &mut Context<Self>,
    ) {
        // Tools with nothing to lose close now, as they did before the next
        // file had to load, so no mark can be drawn on the way out and lost.
        if !self.holding()
            && let Some(edit) = self.edit.take()
        {
            tools::drop_images(edit, cx);
        }
        self.generation += 1;
        let generation = self.generation;
        self.loading = Some((path.clone(), folder, position));
        let light = theme::is_light();
        let side = fit_side(cx);
        let work = cx
            .background_executor()
            .spawn(async move { read(&path, light, side) });
        self._reading = Some(cx.spawn(async move |this, cx| {
            let body = work.await;
            let _ = this.update(cx, |this, cx| {
                // Moved on since: this is for a file no longer wanted.
                if this.generation == generation {
                    this.take_over(body, cx);
                } else {
                    drop_body(body, cx);
                }
            });
        }));
    }

    fn take_over(&mut self, body: Body, cx: &mut Context<Self>) {
        let Some((path, folder, position)) = self.loading.take() else {
            return drop_body(body, cx);
        };
        self.release(cx);
        self.path = path;
        self.folder = folder;
        self.position = position;
        self.loading_ticks = 40;
        self.show_source = false;
        self.zoom = Zoom::default();
        self.want_actual = false;
        self.panning = None;
        self.scroll = UniformListScrollHandle::new();
        self.blocks = match &body {
            Body::Markdown { blocks, .. } => {
                Some(ListState::new(blocks.len(), ListAlignment::Top, px(400.)))
            }
            _ => None,
        };
        self.body = body;
        // A save that replaced this file waited for it, and the selection may
        // have moved meanwhile.
        if let Some((path, folder, position)) = self.wanted.take() {
            self.show(&path, folder, position, cx);
        }
        cx.notify();
    }

    /// A sheet or menu opened or closed over this.
    pub fn set_covered(&mut self, covered: bool, cx: &mut Context<Self>) {
        if self.covered != covered {
            self.covered = covered;
            if covered {
                if let Some((host, _)) = self.native.borrow().as_ref() {
                    host.hide();
                }
                if let Some(player) = self.player.borrow().as_ref() {
                    player.hide();
                }
                if let Some(page) = self.page.borrow().as_ref() {
                    page.host.hide();
                }
            }
            cx.notify();
        }
    }

    /// Gives back what's held: the picture in memory and on the GPU, and the
    /// native view.
    pub fn release(&mut self, cx: &mut App) {
        if let Body::Picture { image, .. } = std::mem::replace(&mut self.body, Body::Loading) {
            give_back_image(image, cx);
        }
        if let Some(edit) = self.edit.take() {
            tools::drop_images(edit, cx);
        }
        self.pinch = None;
        if let Some((sharp, image)) = self.held.take() {
            cx.remove_asset::<Sharp>(&sharp);
            if let Some(image) = image {
                give_back_image(image, cx);
            }
        }
        self.native.borrow_mut().take();
        self.player.borrow_mut().take();
        self.page.borrow_mut().take();
        self._ticking = None;
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (at, of) = self.position;
        let toggle: Option<gpui::Stateful<gpui::Div>> = match &self.body {
            Body::Markdown { .. } => {
                let label = if self.show_source {
                    "Preview"
                } else {
                    "Source"
                };
                Some(
                    text_button("preview-source", label, ButtonLook::Ghost)
                        .debug_selector(|| "preview-source".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_source = !this.show_source;
                            cx.notify();
                        })),
                )
            }
            Body::Picture { .. } => {
                let zoomed = self.zoom.zoomed() || self.want_actual;
                let label = if zoomed { "Fit" } else { "Actual Size" };
                Some(
                    theme::tip(
                        theme::icon_button("preview-zoom", label, "tool-zoom", true),
                        label,
                    )
                    .debug_selector(|| "preview-zoom".into())
                    .when(zoomed, |d| d.bg(content(0.14)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.zoom = Zoom::default();
                        this.want_actual = !zoomed;
                        cx.notify();
                    })),
                )
            }
            _ => None,
        };
        let can_edit = matches!(
            self.body,
            Body::Picture { .. } | Body::Media { video: true, .. }
        );
        let edit_button = can_edit.then(|| {
            theme::tip(
                theme::icon_button("preview-edit", "Edit", "pencil", true),
                "Edit (E)",
            )
            .debug_selector(|| "preview-edit".into())
            .on_click(cx.listener(|this, _, window, cx| this.toggle_edit(window, cx)))
        });
        theme::pane_bar(theme::TITLE_BAR_HEIGHT)
            .gap_2()
            .flex_shrink_0()
            .border_b_1()
            .border_color(content(0.06))
            .child(
                div()
                    .debug_selector(|| "preview-name".into())
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_BODY))
                    .font_weight(FontWeight::MEDIUM)
                    .child(name),
            )
            .when(of > 1, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(format!("{} of {of}", at + 1)),
                )
            })
            .child(div().flex_1())
            .children(toggle)
            .children(edit_button)
            .child(
                text_button("preview-open", "Open", ButtonLook::Secondary)
                    .debug_selector(|| "preview-open".into())
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSelected), cx)),
            )
            .child(
                theme::tip(
                    theme::icon_button("preview-close", "Close", "cancel-01", true),
                    "Close (Esc)",
                )
                .debug_selector(|| "preview-close".into())
                .on_click(|_, window, cx| window.dispatch_action(Box::new(QuickLook), cx)),
            )
    }

    /// A video's or song's picture, fit, until its first frame is ready.
    fn poster(
        &mut self,
        modified: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let sharp = Sharp {
            path: self.path.clone(),
            side: fit_side(cx),
            modified,
        };
        let image = window.use_asset::<Sharp>(&sharp, cx).flatten();
        self.hold(sharp, image.clone(), window, cx);
        // Audio without artwork has no poster, and that's no failure.
        match image {
            Some(image) => placed(image, Zoom::default(), None).into_any_element(),
            None => div().size_full().into_any_element(),
        }
    }

    /// Keeps `image` until something else is asked for, which gives it back.
    fn hold(
        &mut self,
        sharp: Sharp,
        image: Option<Arc<RenderImage>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.held.as_ref().is_some_and(|(held, _)| *held != sharp) {
            self.give_back(window, cx);
        }
        self.held = Some((sharp, image));
    }

    fn give_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((old, old_image)) = self.held.take() {
            cx.remove_asset::<Sharp>(&old);
            if let Some(old_image) = old_image {
                cx.drop_image(old_image, Some(window));
            }
        }
    }

    /// The picture, fit or zoomed. Zoomed past what the fitted decode holds, it
    /// decodes in full, and the fitted one stands in until that's ready.
    fn picture(
        &mut self,
        image: Arc<RenderImage>,
        modified: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let bounds = self.frame.get();
        let source = tools::image_size(&image);
        let (_, scale) = place(bounds, source, self.zoom);
        let sharper =
            self.want_actual || (self.zoom.zoomed() && scale * window.scale_factor() > 1.);
        let full = if sharper {
            let sharp = Sharp {
                path: self.path.clone(),
                side: FULL_SIDE,
                modified,
            };
            let full = window.use_asset::<Sharp>(&sharp, cx).flatten();
            self.hold(sharp, full.clone(), window, cx);
            full
        } else {
            self.give_back(window, cx);
            None
        };
        if self.want_actual
            && let Some(full) = &full
            && bounds.size.width > px(0.)
        {
            // One of its pixels to each of the screen's.
            let (_, fit) = place(bounds, tools::image_size(full), Zoom::default());
            let factor = 1. / (fit * window.scale_factor());
            // Zooming stays at fit or closer; only this goes below it.
            self.zoom = if factor < 1. {
                Zoom {
                    level: factor,
                    ..Zoom::default()
                }
            } else {
                Zoom::default().around(factor, bounds.center(), bounds, source)
            };
            self.want_actual = false;
        }
        let movable = self.zoom.movable();
        let grabbing = self.panning.is_some();
        div()
            .id("preview-picture")
            .debug_selector(|| "preview-picture".into())
            .relative()
            .size_full()
            .overflow_hidden()
            .when(movable, |d| {
                d.cursor(if grabbing {
                    gpui::CursorStyle::ClosedHand
                } else {
                    gpui::CursorStyle::OpenHand
                })
            })
            .on_scroll_wheel(cx.listener(move |this, event, _, cx| {
                this.scrolled(event, source, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    if this.zoom.movable() {
                        this.panning = Some(event.position);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    let Some(last) = this.panning.filter(|_| event.dragging()) else {
                        return;
                    };
                    this.zoom = this
                        .zoom
                        .panned(event.position - last, this.frame.get(), source);
                    this.panning = Some(event.position);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.panning.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.panning.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(placed(
                full.unwrap_or(image),
                self.zoom,
                Some(self.frame.clone()),
            ))
            .child(legend(if movable {
                "Drag to move · ⌘ scroll or pinch to zoom"
            } else {
                "⌘ scroll or pinch to zoom"
            }))
            .into_any_element()
    }

    /// ⌘-scroll zooms what's shown, and scrolling moves it once zoomed.
    pub(super) fn scrolled(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        image: (f32, f32),
        cx: &mut Context<Self>,
    ) {
        if self.rezoom(|zoom, bounds| zoom.scrolled(event, bounds, image)) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Changes the zoom of what's shown, the edit's while it shows a picture,
    /// in the box it's drawn in. Says whether it changed.
    fn rezoom(&mut self, change: impl FnOnce(Zoom, Bounds<Pixels>) -> Option<Zoom>) -> bool {
        if self.editing_image() {
            let Some((bounds, zoom)) = self.edit_zoom() else {
                return false;
            };
            return change(*zoom, bounds).map(|next| *zoom = next).is_some();
        }
        let Some(next) = change(self.zoom, self.frame.get()) else {
            return false;
        };
        self.zoom = next;
        self.want_actual = false;
        true
    }

    /// The size of the picture on screen, the edit's or the preview's.
    fn shown_size(&self) -> Option<(f32, f32)> {
        if self.editing_image() {
            return self.edit_size();
        }
        match &self.body {
            Body::Picture { image, .. } => Some(tools::image_size(image)),
            _ => None,
        }
    }

    /// A pinch zooms the picture under it.
    fn listen_for_pinch(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.pinch.is_some() {
            return;
        }
        let Some(pinch) = zoom::pinch(window) else {
            return;
        };
        let events = pinch.events.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok((delta, at)) = events.recv().await {
                let pinched = this.update_in(cx, |this, window, cx| {
                    // AppKit's points, before the app's UI scale.
                    let at = at / window.content_zoom();
                    let Some(image) = this.shown_size().filter(|_| !this.covered) else {
                        return;
                    };
                    let zoomed = this.rezoom(|zoom, bounds| {
                        bounds
                            .contains(&at)
                            .then(|| zoom.around(1. + delta, at, bounds, image))
                    });
                    if zoomed {
                        cx.notify();
                    }
                });
                if pinched.is_err() {
                    break;
                }
            }
        });
        self.pinch = Some((pinch, task));
    }

    fn html(&self) -> gpui::AnyElement {
        let page = self.page.clone();
        let path = self.path.clone();
        let covered = self.covered;
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, _| {
                if covered {
                    return;
                }
                let mut page = page.borrow_mut();
                if page.is_none() {
                    *page = Page::new(&path, window);
                }
                if let Some(page) = page.as_ref() {
                    page.host.place(bounds, window);
                }
            },
        )
        .size_full()
        .into_any_element()
    }

    /// The picture, with the poster under it until its first frame, and the
    /// strip below. Audio has its artwork there instead.
    fn media(
        &mut self,
        video: bool,
        modified: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let framed = self
            .player
            .borrow()
            .as_ref()
            .is_some_and(Player::frame_ready);
        // The player steps aside for an open menu, and this stands in for it.
        let poster = (!framed || self.menu_open()).then(|| self.poster(modified, window, cx));
        let player = self.player.clone();
        let path = self.path.clone();
        // Nothing GPUI draws can sit over the player, so it steps aside for a menu.
        let covered = self.covered || self.menu_open();
        if self.menu_open()
            && let Some(player) = self.player.borrow().as_ref()
        {
            player.hide();
        }
        let surface = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, _| {
                if covered {
                    return;
                }
                let mut player = player.borrow_mut();
                if player.is_none() {
                    *player = Player::new(&path, video, window);
                }
                if let Some(player) = player.as_ref() {
                    player.place(bounds, window);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let strip = self.strip(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector(|| "preview-media".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // Laid out in the box, as a picture's own height would run
                    // over the strip below.
                    .overflow_hidden()
                    .children(
                        poster.map(|poster| {
                            div().absolute().top_0().left_0().size_full().child(poster)
                        }),
                    )
                    .child(surface),
            )
            .child(strip)
            .into_any_element()
    }

    /// Play, the scrub track, the time and mute, redrawn about 4 times a second
    /// while it plays and not at all otherwise.
    fn strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let held = self.player.borrow();
        let playing = held.as_ref().is_some_and(Player::playing);
        let muted = held.as_ref().is_some_and(Player::muted);
        let position = held.as_ref().map_or(0., Player::position);
        let duration = held.as_ref().and_then(Player::duration);
        drop(held);
        // Also while a file loads, as nothing else says when its length is known.
        let loaded = self
            .player
            .borrow()
            .as_ref()
            .is_some_and(|p| p.duration().is_some() && (p.frame_ready() || !p.is_video()));
        if self._ticking.is_none() && (playing || (!loaded && self.loading_ticks > 0)) {
            self._ticking = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(250))
                        .await;
                    let going = this.update(cx, |this, cx| {
                        cx.notify();
                        let player = this.player.borrow();
                        let playing = player.as_ref().is_some_and(Player::playing);
                        let known = player.as_ref().is_some_and(|p| {
                            p.duration().is_some() && (p.frame_ready() || !p.is_video())
                        });
                        drop(player);
                        this.loading_ticks = this.loading_ticks.saturating_sub(1);
                        let going = playing || (!known && this.loading_ticks > 0);
                        if !going {
                            this._ticking = None;
                        }
                        going
                    });
                    if !matches!(going, Ok(true)) {
                        break;
                    }
                }
            }));
        }
        // Each button's name and the icon that shows the state it's in.
        let play = if playing {
            ("Pause", "pause")
        } else {
            ("Play", "play")
        };
        let mute = if muted {
            ("Unmute", "tool-mute")
        } else {
            ("Mute", "volume")
        };
        let fraction = duration.map_or(0., |total| (position / total).clamp(0., 1.)) as f32;
        let track = self.track.clone();
        let measured = self.track.clone();
        theme::pane_bar(theme::TITLE_BAR_HEIGHT)
            .gap_3()
            .flex_shrink_0()
            .border_t_1()
            .border_color(content(0.06))
            .child(
                theme::tip_below(
                    theme::icon_button("preview-play", play.0, play.1, true),
                    play.0,
                )
                .debug_selector(|| "preview-play".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(player) = this.player.borrow().as_ref() {
                        player.toggle();
                    }
                    cx.notify();
                })),
            )
            .child(
                div()
                    .id("preview-track")
                    .flex_1()
                    .min_w_0()
                    .h(px(theme::BUTTON_HEIGHT))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            let bounds = track.get();
                            let width = bounds.size.width.as_f32();
                            if width <= 0. {
                                return;
                            }
                            let at = (event.position.x - bounds.origin.x).as_f32() / width;
                            if let Some(player) = this.player.borrow().as_ref()
                                && let Some(total) = player.duration()
                            {
                                player.seek(f64::from(at.clamp(0., 1.)) * total);
                            }
                            cx.notify();
                        }),
                    )
                    .child(
                        theme::progress_track()
                            .w_full()
                            .child(theme::progress_fill(fraction))
                            .child(
                                canvas(move |bounds, _, _| measured.set(bounds), |_, _, _, _| {})
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full(),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child(match duration {
                        Some(total) => format!("{} / {}", clock(position), clock(total)),
                        None => clock(position),
                    }),
            )
            .child(
                theme::tip_below(
                    theme::icon_button("preview-mute", mute.0, mute.1, true),
                    mute.0,
                )
                .debug_selector(|| "preview-mute".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(player) = this.player.borrow().as_ref() {
                        player.toggle_muted();
                    }
                    cx.notify();
                })),
            )
    }

    fn system(&self) -> gpui::AnyElement {
        let native = self.native.clone();
        let covered = self.covered;
        let path = self.path.clone();
        let folder = self.folder;
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, _| {
                if covered {
                    return;
                }
                let mut native = native.borrow_mut();
                if native.is_none() {
                    *native = quick_look_view(window).map(|host| (host, PathBuf::new()));
                }
                if let Some((host, shown)) = native.as_mut() {
                    // Paint runs often, and setting it reloads the file.
                    if *shown != path {
                        set_item(&host.view, &path, folder);
                        shown.clone_from(&path);
                    }
                    host.place(bounds, window);
                }
            },
        )
        .size_full()
        .into_any_element()
    }
}

/// Something to say in place of the file.
fn notice(why: impl Into<SharedString>) -> gpui::AnyElement {
    div()
        .debug_selector(|| "preview-notice".into())
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_BODY))
        .text_color(content(0.65))
        .child(why.into())
        .into_any_element()
}

/// A picture where `place` puts it, painted there rather than laid out, so what's
/// drawn and where a mark lands can't disagree. Left to layout, a picture's own
/// height overruns a box shorter than it is. `frame` learns the box it's in.
fn placed(
    image: Arc<RenderImage>,
    zoom: Zoom,
    frame: Option<Rc<Cell<Bounds<Pixels>>>>,
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            if let Some(frame) = &frame {
                frame.set(bounds);
            }
        },
        move |bounds: Bounds<Pixels>, _, window, _| {
            let source = tools::image_size(&image);
            let (origin, scale) = place(bounds, source, zoom);
            let drawn = Bounds::new(
                origin,
                gpui::size(px(source.0 * scale), px(source.1 * scale)),
            );
            let _ = window.paint_image(
                drawn,
                drawn,
                gpui::Corners::default(),
                image.clone(),
                0,
                false,
            );
        },
    )
    .size_full()
}

/// How to zoom, in a corner over the picture.
fn legend(text: &'static str) -> impl IntoElement {
    div().absolute().right_2().bottom_2().child(
        theme::floating(theme::Elevation::Popover)
            .debug_selector(|| "preview-legend".into())
            .px_2()
            .py_1()
            .text_size(px(theme::TEXT_SMALL))
            .text_color(content(0.65))
            .child(text),
    )
}

/// The side to decode a picture at to fill the window.
fn fit_side(cx: &App) -> u32 {
    match FIT_SIDE.load(Ordering::Relaxed) {
        0 => cx.primary_display().map_or(4096, |display| {
            let size = display.bounds().size;
            // Screens are Retina, and a bigger decode only costs a little.
            (size.width.max(size.height).as_f32() * 2.).ceil() as u32
        }),
        side => side,
    }
}

/// A read nobody wants any more gives its picture back.
fn drop_body(body: Body, cx: &mut App) {
    if let Body::Picture { image, .. } = body {
        give_back_image(image, cx);
    }
}

/// Frees `image` on the GPU once the window being updated is back among the
/// app's windows, as `drop_image` can't reach it during its own update.
pub(super) fn give_back_image(image: Arc<RenderImage>, cx: &mut App) {
    cx.defer(move |cx| cx.drop_image(image, None));
}

/// `None` without a window on the main thread, as in tests.
fn quick_look_view(window: &Window) -> Option<Host<QLPreviewView>> {
    let mtm = MainThreadMarker::new()?;
    let zero = NSRect::ZERO;
    // SAFETY: the designated initializer, on the main thread.
    let view = unsafe { QLPreviewView::initWithFrame(QLPreviewView::alloc(mtm), zero) }?;
    Host::new(view, true, window)
}

fn set_item(view: &Retained<QLPreviewView>, path: &Path, folder: bool) {
    let path = NSString::from_str(&path.to_string_lossy());
    let url = NSURL::fileURLWithPath_isDirectory(&path, folder);
    let item: Retained<ProtocolObject<dyn objc2_quick_look_ui::QLPreviewItem>> =
        ProtocolObject::from_retained(url);
    // SAFETY: paint runs on the main thread, and a URL is a preview item.
    unsafe { view.setPreviewItem(Some(&item)) };
}

fn read(path: &Path, light: bool, side: u32) -> Body {
    let notice = |why: &str| Body::Notice(why.to_string());
    match preview::classify(path) {
        Kind::NotDownloaded => notice("This file isn't downloaded. Download it to preview it."),
        Kind::NeedsAccess => notice("Needs Full Disk Access to preview."),
        Kind::Unplayable => notice("macOS can't play this format. Use Open With."),
        Kind::System => Body::System,
        Kind::Html => Body::Html,
        Kind::Video => Body::Media {
            video: true,
            modified: modified(path),
        },
        Kind::Audio => Body::Media {
            video: false,
            modified: modified(path),
        },
        Kind::Animated => Body::Animated,
        Kind::Picture => {
            let decoded = thumbnails::picture(path, side)
                .ok()
                .and_then(crate::thumbnails::render_image);
            match decoded {
                Some(image) => Body::Picture {
                    modified: modified(path),
                    image: Arc::new(image),
                },
                None => notice("This picture can't be shown."),
            }
        }
        Kind::Zip => zip_rows(path),
        Kind::Text => match preview::read_text(path) {
            Ok(file) => {
                let lines = highlighted(&file.text, path, light);
                Body::Text {
                    lines: Arc::new(lines),
                    cut: file.cut,
                }
            }
            Err(why) => Body::Notice(why),
        },
        Kind::Markdown => match preview::read_text(path) {
            Ok(file) => {
                let folder = path.parent().unwrap_or(path);
                let mut blocks = markdown::parse(&file.text, folder);
                // GPUI reads an image with no gate, so only what can't prompt
                // or download gets that far.
                blocks.retain(|block| match block {
                    Block::Image(path) => {
                        matches!(preview::classify(path), Kind::Picture | Kind::Animated)
                    }
                    _ => true,
                });
                Body::Markdown {
                    blocks: Arc::new(blocks),
                    source: Arc::new(highlighted(&file.text, path, light)),
                }
            }
            Err(why) => Body::Notice(why),
        },
    }
}

fn modified(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |at| at.as_secs())
}

fn zip_rows(path: &Path) -> Body {
    match cloud::without_downloads(|| Archive::open(path)) {
        Ok(archive) => {
            let names: Vec<String> = archive.entries.iter().map(|e| e.name.clone()).collect();
            let said = format!(
                "{} {}",
                archive.total,
                if archive.total == 1 { "item" } else { "items" }
            );
            Body::Rows(Arc::new(names), Some(said))
        }
        Err(why) => Body::Notice(why),
    }
}

fn plain_lines(text: &str) -> Vec<Line> {
    text.lines()
        .map(|line| Line {
            text: SharedString::from(line.to_string()),
            spans: Vec::new(),
        })
        .collect()
}

/// The text with colors, when it's small enough and its syntax is known.
fn highlighted(text: &str, path: &Path, light: bool) -> Vec<Line> {
    if text.len() > HIGHLIGHT_LIMIT {
        return plain_lines(text);
    }
    static SYNTAXES: std::sync::OnceLock<SyntaxSet> = std::sync::OnceLock::new();
    static THEMES: std::sync::OnceLock<ThemeSet> = std::sync::OnceLock::new();
    let syntaxes = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(syntax) = syntaxes.find_syntax_by_extension(&extension) else {
        return plain_lines(text);
    };
    let theme = &themes.themes[if light {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    }];
    let mut lines = HighlightLines::new(syntax, theme);
    syntect::util::LinesWithEndings::from(text)
        .map(|line| {
            let mut spans = Vec::new();
            let mut at = 0;
            let stripped = line.trim_end_matches(['\n', '\r']);
            for (style, piece) in lines.highlight_line(line, syntaxes).unwrap_or_default() {
                let end = (at + piece.len()).min(stripped.len());
                if at < end {
                    // design: a syntax theme's colors, not the app's.
                    let color = gpui::Rgba {
                        r: f32::from(style.foreground.r) / 255.,
                        g: f32::from(style.foreground.g) / 255.,
                        b: f32::from(style.foreground.b) / 255.,
                        a: 1.,
                    };
                    spans.push((
                        at..end,
                        HighlightStyle {
                            color: Some(color.into()),
                            ..Default::default()
                        },
                    ));
                }
                at += piece.len();
            }
            Line {
                text: SharedString::from(stripped.to_string()),
                spans,
            }
        })
        .collect()
}

fn text_rows(lines: Arc<Vec<Line>>, scroll: &UniformListScrollHandle) -> gpui::AnyElement {
    let digits = lines.len().max(1).ilog10() as usize + 1;
    uniform_list("preview-lines", lines.len(), move |range, _, _| {
        range
            .map(|ix| {
                let line = &lines[ix];
                div()
                    .h(px(theme::ROW_HEIGHT - 8.))
                    .flex()
                    .gap_3()
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(content(0.65))
                            .child(format!("{:>digits$}", ix + 1)),
                    )
                    .child(StyledText::new(line.text.clone()).with_highlights(line.spans.clone()))
            })
            .collect()
    })
    .track_scroll(scroll)
    .size_full()
    .p_3()
    .font_family(theme::MONO_FONT)
    .text_size(px(theme::TEXT_SMALL))
    .into_any_element()
}

fn inline_text(id: (&'static str, usize), inline: &Inline) -> gpui::AnyElement {
    let mut spans: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    for range in &inline.bold {
        spans.push((
            range.clone(),
            HighlightStyle {
                font_weight: Some(FontWeight::SEMIBOLD),
                ..Default::default()
            },
        ));
    }
    for range in &inline.italic {
        spans.push((
            range.clone(),
            HighlightStyle {
                font_style: Some(gpui::FontStyle::Italic),
                ..Default::default()
            },
        ));
    }
    for range in &inline.code {
        spans.push((
            range.clone(),
            HighlightStyle {
                background_color: Some(content(0.08)),
                ..Default::default()
            },
        ));
    }
    let links = inline.links.clone();
    for (range, _) in &links {
        spans.push((
            range.clone(),
            HighlightStyle {
                color: Some(theme::accent()),
                ..Default::default()
            },
        ));
    }
    spans.sort_by_key(|(range, _)| range.start);
    // Overlapping styles aren't allowed, so the first wins.
    let mut end = 0;
    spans.retain(|(range, _)| {
        let keep = range.start >= end;
        if keep {
            end = range.end;
        }
        keep
    });
    let styled = StyledText::new(inline.text.clone()).with_highlights(spans);
    if links.is_empty() {
        return styled.into_any_element();
    }
    let ranges: Vec<_> = links.iter().map(|(range, _)| range.clone()).collect();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |ix, _, cx| {
            let url = &links[ix].1;
            // Only out to the browser, and only when clicked.
            if url.starts_with("https://") || url.starts_with("http://") {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

fn block(ix: usize, block: &Block) -> gpui::AnyElement {
    let id = ("markdown", ix);
    match block {
        Block::Heading(level, inline) => div()
            .pt_3()
            .pb_1()
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(match level {
                1 => theme::TEXT_TITLE + 4.,
                2 => theme::TEXT_TITLE + 2.,
                _ => theme::TEXT_TITLE,
            }))
            .child(inline_text(id, inline))
            .into_any_element(),
        Block::Paragraph(inline) => div()
            .pb_2()
            .child(inline_text(id, inline))
            .into_any_element(),
        Block::Quote(inline) => div()
            .pb_2()
            .pl_3()
            .border_l_2()
            .border_color(content(0.12))
            .text_color(content(0.65))
            .child(inline_text(id, inline))
            .into_any_element(),
        Block::Item(marker, depth, inline) => div()
            .pb_1()
            .pl(px(16. * *depth as f32))
            .flex()
            .gap_2()
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(content(0.65))
                    .child(marker.clone()),
            )
            .child(div().min_w_0().child(inline_text(id, inline)))
            .into_any_element(),
        Block::Code(text) => div()
            .mb_2()
            .p_2()
            .rounded(px(theme::RADIUS_ROW))
            .bg(content(0.06))
            .font_family(theme::MONO_FONT)
            .text_size(px(theme::TEXT_SMALL))
            .child(text.clone())
            .into_any_element(),
        Block::Image(path) => div()
            .pb_2()
            .child(
                gpui::img(path.clone())
                    .max_w_full()
                    .max_h(px(480.))
                    .object_fit(gpui::ObjectFit::ScaleDown),
            )
            .into_any_element(),
        Block::Rule => div().my_2().h(px(1.)).bg(content(0.12)).into_any_element(),
    }
}

impl Render for PreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let side = viewport.width.max(viewport.height).as_f32() * window.scale_factor();
        FIT_SIDE.store(side.ceil() as u32, Ordering::Relaxed);
        if matches!(self.body, Body::Picture { .. }) || self.editing_image() {
            self.listen_for_pinch(window, cx);
        } else {
            self.pinch = None;
        }
        // Editing, the title row itself holds the tools.
        let header = match self.edit_row(cx) {
            Some(row) => row,
            None => self.header(cx).into_any_element(),
        };
        let edit_error = self.edit_error();
        if !matches!(self.body, Body::System) {
            self.native.borrow_mut().take();
        }
        if !matches!(self.body, Body::Media { .. }) {
            self.player.borrow_mut().take();
        }
        if !matches!(self.body, Body::Html) {
            self.page.borrow_mut().take();
        }
        let content_view: gpui::AnyElement = match &self.body {
            Body::Loading => div().size_full().into_any_element(),
            Body::Notice(why) => notice(why.clone()),
            Body::Text { lines, cut } => {
                let cut = *cut;
                let rows = text_rows(lines.clone(), &self.scroll);
                div()
                    .debug_selector(|| "preview-text".into())
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h_0().child(rows))
                    .when(cut, |d| {
                        d.child(
                            theme::pane_bar(theme::PATH_BAR_HEIGHT)
                                .flex_shrink_0()
                                .text_size(px(theme::TEXT_SMALL))
                                .text_color(content(0.65))
                                .child(format!(
                                    "Showing the first {} MB.",
                                    TEXT_LIMIT / 1024 / 1024
                                )),
                        )
                    })
                    .into_any_element()
            }
            Body::Markdown { blocks, source } => {
                if self.show_source || self.blocks.is_none() {
                    text_rows(source.clone(), &self.scroll)
                } else {
                    let blocks = blocks.clone();
                    let state = self
                        .blocks
                        .clone()
                        .unwrap_or_else(|| ListState::new(0, ListAlignment::Top, px(400.)));
                    div()
                        .debug_selector(|| "preview-markdown".into())
                        .size_full()
                        .text_size(px(theme::TEXT_BODY))
                        .child(
                            list(state, move |ix, _, _| {
                                div()
                                    .px_4()
                                    .child(block(ix, &blocks[ix]))
                                    .into_any_element()
                            })
                            .size_full()
                            .py_2(),
                        )
                        .into_any_element()
                }
            }
            Body::Picture { .. } if self.editing_image() => self.edit_surface(window, cx),
            Body::Picture { modified, image } => {
                let (modified, image) = (*modified, image.clone());
                self.picture(image, modified, window, cx)
            }
            // Out of flow, as a tall picture's height would otherwise run out of
            // the box.
            Body::Animated => div()
                .relative()
                .size_full()
                .overflow_hidden()
                .child(
                    gpui::img(self.path.clone())
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .object_fit(gpui::ObjectFit::Contain),
                )
                .into_any_element(),
            Body::Rows(names, said) => {
                let names = names.clone();
                let said = said.clone();
                div()
                    .debug_selector(|| "preview-rows".into())
                    .size_full()
                    .flex()
                    .flex_col()
                    .children(said.map(|said| {
                        theme::pane_bar(theme::PATH_BAR_HEIGHT)
                            .flex_shrink_0()
                            .text_size(px(theme::TEXT_SMALL))
                            .text_color(content(0.65))
                            .child(said)
                    }))
                    .child(
                        uniform_list("preview-rows", names.len(), move |range, _, _| {
                            range
                                .map(|ix| {
                                    div()
                                        .h(px(theme::ROW_HEIGHT))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .truncate()
                                        .text_size(px(theme::TEXT_BODY))
                                        .child(names[ix].clone())
                                })
                                .collect()
                        })
                        .track_scroll(&self.scroll)
                        .flex_1(),
                    )
                    .into_any_element()
            }
            Body::System => self.system(),
            Body::Html => self.html(),
            Body::Media { .. } if self.editing_image() => {
                // Under the crop's handles, in place of the player.
                if let Some(player) = self.player.borrow().as_ref() {
                    player.hide();
                }
                self.edit_surface(window, cx)
            }
            Body::Media { video, modified } => {
                let (video, modified) = (*video, *modified);
                self.media(video, modified, window, cx)
            }
        };
        div()
            .debug_selector(|| "preview".into())
            // Not a press on the list, which would let go of the selection.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .children(edit_error)
            .child(div().flex_1().min_h_0().child(content_view))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_cover_the_line_without_its_newline() {
        let lines = highlighted("fn a() {}\nlet b = 1;\n", Path::new("a.rs"), false);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text.as_ref(), "fn a() {}");
        assert!(lines[0].spans.len() > 1, "a known syntax gets colors");
        assert_eq!(lines[0].spans.last().unwrap().0.end, "fn a() {}".len());
        // An unknown syntax stays plain.
        let plain = highlighted("hello", Path::new("a.unknownext"), false);
        assert!(plain[0].spans.is_empty());
    }
}
