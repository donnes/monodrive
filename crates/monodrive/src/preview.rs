//! The in-app preview: it takes over the pane's list, with a bar above. It knows
//! nothing of where it sits, so a preview column can use it later. Every read
//! runs off the UI thread under a generation, so moving on drops a stale result.

mod markdown;

use crate::browser::{OpenSelected, QuickLook};
use crate::native_view::Host;
use crate::theme::{self, ButtonLook, content, text_button};
use gpui::{
    App, Asset, Bounds, Context, FontWeight, HighlightStyle, InteractiveText, ListAlignment,
    ListState, MouseButton, Pixels, RenderImage, SharedString, StyledText, Task,
    UniformListScrollHandle, Window, canvas, div, list, prelude::*, px, uniform_list,
};
use markdown::{Block, Inline};
use monodrive_core::archives::Archive;
use monodrive_core::cloud;
use monodrive_core::preview::{self, Kind, TEXT_LIMIT};
use monodrive_core::thumbnails;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSRect, NSString, NSURL};
use objc2_quick_look_ui::QLPreviewView;
use std::cell::RefCell;
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

/// One picture at one size, from the system's generator, which reads HEIC and
/// RAW and never decodes past `side`.
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
            let bitmap = thumbnails::generate(&sharp.path, sharp.side, &|| false).ok()?;
            let pixels = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bytes)?;
            Some(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
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
}

type SystemView = (Host<QLPreviewView>, PathBuf);

pub struct PreviewView {
    path: PathBuf,
    folder: bool,
    /// Where it is among what's selected: "3 of 12".
    position: (usize, usize),
    generation: u64,
    body: Body,
    show_source: bool,
    zoomed: bool,
    /// A sheet or menu is up, which a native view would draw over.
    covered: bool,
    held: Option<(Sharp, Option<Arc<RenderImage>>)>,
    scroll: UniformListScrollHandle,
    /// The Markdown's scrolling, which can't cross threads with `Body`.
    blocks: Option<ListState>,
    /// The system's view, and the file it's on.
    native: Rc<RefCell<Option<SystemView>>>,
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
            held: None,
            scroll: UniformListScrollHandle::new(),
            blocks: None,
            native: Rc::new(RefCell::new(None)),
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
        self.position = position;
        if self.path == path {
            return cx.notify();
        }
        self.path = path.to_path_buf();
        self.folder = folder;
        self.generation += 1;
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
            if covered && let Some((host, _)) = self.native.borrow().as_ref() {
                host.hide();
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
                let label = if self.zoomed { "Fit" } else { "100%" };
                Some(
                    text_button("preview-zoom", label, ButtonLook::Ghost)
                        .debug_selector(|| "preview-zoom".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.zoomed = !this.zoomed;
                            cx.notify();
                        })),
                )
            }
            _ => None,
        };
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
            .child(
                text_button("preview-open", "Open", ButtonLook::Secondary)
                    .debug_selector(|| "preview-open".into())
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSelected), cx)),
            )
            .child(
                text_button("preview-close", "Close", ButtonLook::Ghost)
                    .debug_selector(|| "preview-close".into())
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(QuickLook), cx)),
            )
    }

    fn picture(
        &mut self,
        modified: u64,
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
        let image = window.use_asset::<Sharp>(&sharp, cx).flatten();
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
            return div().size_full().into_any_element();
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
            gpui::img(image)
                .size_full()
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element()
        }
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
        // Plan 28's next steps draw these themselves.
        Kind::Video | Kind::Audio | Kind::Html | Kind::System => Body::System,
        Kind::Animated => Body::Animated,
        Kind::Picture => Body::Picture {
            modified: std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |at| at.as_secs()),
        },
        Kind::Folder => folder_rows(path),
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

fn folder_rows(path: &Path) -> Body {
    let listed = cloud::without_downloads(|| {
        std::fs::read_dir(path).map(|listed| {
            listed
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| !name.starts_with('.'))
                .collect::<Vec<String>>()
        })
    });
    let Ok(mut names) = listed else {
        return Body::Notice("This folder can't be read.".into());
    };
    names.sort_by_key(|name| name.to_lowercase());
    let said = format!(
        "{} {}",
        names.len(),
        if names.len() == 1 { "item" } else { "items" }
    );
    Body::Rows(Arc::new(names), Some(said))
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
        let header = self.header(cx);
        if !matches!(self.body, Body::System) {
            self.native.borrow_mut().take();
        }
        let content_view: gpui::AnyElement = match &self.body {
            Body::Loading => div().size_full().into_any_element(),
            Body::Notice(why) => div()
                .debug_selector(|| "preview-notice".into())
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_BODY))
                .text_color(content(0.65))
                .child(why.clone())
                .into_any_element(),
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
            Body::Picture { modified } => {
                let modified = *modified;
                self.picture(modified, window, cx)
            }
            Body::Animated => gpui::img(self.path.clone())
                .size_full()
                .object_fit(gpui::ObjectFit::Contain)
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
