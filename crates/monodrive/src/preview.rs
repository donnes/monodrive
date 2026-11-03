//! The in-app preview: it takes over the pane's list, with a bar above. It knows
//! nothing of where it sits, so a preview column can use it later. Every read
//! runs off the UI thread under a generation, so moving on drops a stale result.

mod html;
mod markdown;
mod media;
mod tools;

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
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Beyond this, text stays plain: highlighting walks the whole file.
const HIGHLIGHT_LIMIT: usize = 512 * 1024;
/// Pixels asked for at 100%, as a picture's own size isn't known beforehand.
const FULL_SIDE: u32 = 8192;

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
    Picture {
        modified: u64,
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
    zoomed: bool,
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
            zoomed: false,
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
        self.position = position;
        if self.path == path {
            return cx.notify();
        }
        self.path = path.to_path_buf();
        self.folder = folder;
        self.generation += 1;
        self.edit = None;
        self.loading_ticks = 40;
        self.show_source = false;
        self.zoomed = false;
        self.body = Body::Loading;
        self.scroll = UniformListScrollHandle::new();
        self.release(cx);
        let generation = self.generation;
        let path = self.path.clone();
        let light = theme::is_light();
        let work = cx
            .background_executor()
            .spawn(async move { read(&path, light) });
        self._reading = Some(cx.spawn(async move |this, cx| {
            let body = work.await;
            let _ = this.update(cx, |this, cx| {
                // Moved on since: this is for a file no longer shown.
                if this.generation == generation {
                    this.blocks = match &body {
                        Body::Markdown { blocks, .. } => {
                            Some(ListState::new(blocks.len(), ListAlignment::Top, px(400.)))
                        }
                        _ => None,
                    };
                    this.body = body;
                    cx.notify();
                }
            });
        }));
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
        if let Some((sharp, image)) = self.held.take() {
            cx.remove_asset::<Sharp>(&sharp);
            if let Some(image) = image {
                cx.drop_image(image, None);
            }
        }
        self.native.borrow_mut().take();
        self.player.borrow_mut().take();
        self.page.borrow_mut().take();
        self._ticking = None;
        self.edit = None;
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
                let label = if self.zoomed { "Fit" } else { "Actual Size" };
                Some(
                    tools::tip(
                        theme::icon_button("preview-zoom", label, "tool-zoom", true),
                        label,
                    )
                    .debug_selector(|| "preview-zoom".into())
                    .when(self.zoomed, |d| d.bg(content(0.14)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.zoomed = !this.zoomed;
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
            tools::tip(
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
                tools::tip(
                    theme::icon_button("preview-close", "Close", "cancel-01", true),
                    "Close (Esc)",
                )
                .debug_selector(|| "preview-close".into())
                .on_click(|_, window, cx| window.dispatch_action(Box::new(QuickLook), cx)),
            )
    }

    fn picture(
        &mut self,
        modified: u64,
        poster: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let viewport = window.viewport_size();
        let fit =
            (viewport.width.max(viewport.height).as_f32() * window.scale_factor()).ceil() as u32;
        let sharp = Sharp {
            path: self.path.clone(),
            side: if self.zoomed { FULL_SIDE } else { fit },
            modified,
        };
        // A new size gives the last one back.
        let loaded = window.use_asset::<Sharp>(&sharp, cx);
        let image = loaded.clone().flatten();
        if self.held.as_ref().is_some_and(|(held, _)| *held != sharp)
            && let Some((old, old_image)) = self.held.take()
        {
            cx.remove_asset::<Sharp>(&old);
            if let Some(old_image) = old_image {
                cx.drop_image(old_image, Some(window));
            }
        }
        self.held = Some((sharp, image.clone()));
        let Some(image) = image else {
            // Audio without artwork has no poster, and that's no failure.
            return if loaded.is_some() && !poster {
                notice("This picture can't be shown.")
            } else {
                div().size_full().into_any_element()
            };
        };
        if self.zoomed {
            let scale = window.scale_factor();
            let size = image.size(0);
            div()
                .id("preview-picture")
                .size_full()
                .overflow_scroll()
                .child(
                    gpui::img(image.clone())
                        .w(px(size.width.0 as f32 / scale))
                        .h(px(size.height.0 as f32 / scale)),
                )
                .into_any_element()
        } else {
            fitted(image).into_any_element()
        }
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
        let poster =
            (!framed || self.menu_open()).then(|| self.picture(modified, true, window, cx));
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
        let fraction = duration.map_or(0., |total| (position / total).clamp(0., 1.)) as f32;
        let track = self.track.clone();
        let measured = self.track.clone();
        theme::pane_bar(theme::TITLE_BAR_HEIGHT)
            .gap_3()
            .flex_shrink_0()
            .border_t_1()
            .border_color(content(0.06))
            .child(
                text_button(
                    "preview-play",
                    if playing { "Pause" } else { "Play" },
                    ButtonLook::Secondary,
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
                text_button(
                    "preview-mute",
                    if muted { "Unmute" } else { "Mute" },
                    ButtonLook::Ghost,
                )
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

/// A picture centered in the space it has and never enlarged, painted where
/// `fit` says, so what's drawn and where a mark lands can't disagree. Left to
/// layout, a picture's own height overruns a box shorter than it is.
pub(crate) fn fitted(image: Arc<RenderImage>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            let source = image.size(0);
            let source = (source.width.0 as f32, source.height.0 as f32);
            let (origin, scale) = tools::fit(bounds, source);
            let frame = Bounds::new(
                origin,
                gpui::size(px(source.0 * scale), px(source.1 * scale)),
            );
            let _ = window.paint_image(
                frame,
                frame,
                gpui::Corners::default(),
                image.clone(),
                0,
                false,
            );
        },
    )
    .size_full()
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

fn read(path: &Path, light: bool) -> Body {
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
        Kind::Picture => Body::Picture {
            modified: modified(path),
        },
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
            Body::Picture { modified } => {
                let modified = *modified;
                self.picture(modified, false, window, cx)
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
