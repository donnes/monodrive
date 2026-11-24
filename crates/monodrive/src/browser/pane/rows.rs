//! A row of a pane's list as drawn, and the arithmetic and wording rows
//! share: where the keys move, how the grid lays out, and how a name, a date
//! and a size read.

use super::drag::Spot;
use super::*;
use crate::downloads::State;
use crate::file_icons;
use crate::format::format_bytes;
use crate::preferences::{Preferences, View};
use crate::theme::icon;
use crate::theme::{self, accent, content, danger, on_accent};
use crate::thumbnails::{self, Thumb};
use chrono::{DateTime, Local};
use gpui::prelude::*;
use gpui::{AnyElement, Hsla};
use gpui::{
    App, ClickEvent, Context, FontWeight, Keystroke, RenderImage, TextOverflow, Window, div, px,
};
use monodrive_core::entries::{Entry, Kind};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// `entry`'s picture at `pixels`, if it has one and it's ready. A pane's
/// `thumbs` hold it while each frame draws it, which `drawn` records, and give
/// it back once a frame doesn't.
pub(in crate::browser) fn thumbnail(
    (thumbs, drawn): (
        &mut HashMap<Thumb, Option<Arc<RenderImage>>>,
        &mut HashSet<Thumb>,
    ),
    entry: &Entry,
    pixels: u32,
    window: &mut Window,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    if !(cx.global::<Preferences>().thumbnails && thumbnails::wanted(entry)) {
        return None;
    }
    let thumb = Thumb {
        path: entry.path.clone(),
        side: pixels,
        modified: entry.modified,
    };
    if !thumbs.contains_key(&thumb) {
        thumbnails::hold(&thumb, cx);
    }
    let image = thumbnails::get(&thumb, window, cx);
    drawn.insert(thumb.clone());
    thumbs.insert(thumb, image.clone());
    image
}
/// A grid cell's name in at most two lines, as Finder sets it: the first
/// breaks where it would wrap, at a word when it can, and the rest is cut
/// in the middle, so the start and the extension both show.
pub(in crate::browser) fn grid_name(
    name: &str,
    width: gpui::Pixels,
    size: gpui::Pixels,
    badge: Option<gpui::AnyElement>,
    window: &Window,
) -> Vec<gpui::Div> {
    let line = |text: &str| {
        div()
            .whitespace_nowrap()
            .overflow_hidden()
            .child(text.to_owned())
    };
    let mut wrapper = window
        .text_system()
        .line_wrapper(gpui::font(theme::UI_FONT), size);
    // Where the name's first line ends, if it needs a second.
    let mut wraps_at = |width| {
        wrapper
            .wrap_line(
                &[gpui::LineFragment::text(name)],
                width,
                gpui::IndentAdjustment::NoIndent,
            )
            .next()
            .map(|boundary| boundary.ix)
            .filter(|ix| *ix < name.len())
    };
    let Some(badge) = badge else {
        return match wraps_at(width) {
            Some(ix) => vec![
                line(name[..ix].trim_end()),
                line(&name[ix..]).text_ellipsis_middle(),
            ],
            None => vec![line(name)],
        };
    };
    // The badge always ends the name's last line. A one-line name with no
    // room for it breaks before its extension, so no part of the name hides.
    let with_badge = |text: gpui::Div| {
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .child(text.min_w_0())
            .child(badge)
    };
    let room = width - px(GRID_BADGE + 4.);
    if wraps_at(room).is_none() {
        return vec![with_badge(line(name))];
    }
    let ix = wraps_at(width).or_else(|| {
        kept_extension(name)
            .map(|extension| name.len() - extension.len())
            .or_else(|| wraps_at(room))
    });
    match ix {
        Some(ix) => vec![
            line(name[..ix].trim_end()),
            with_badge(line(&name[ix..]).text_ellipsis_middle()),
        ],
        None => vec![with_badge(line(name).text_ellipsis_middle())],
    }
}
/// How wide the cloud badge is beside a grid label, and beside a list row's
/// name.
pub(in crate::browser) const GRID_BADGE: f32 = 12.;
pub(in crate::browser) const LIST_BADGE: f32 = 14.;
/// The extension a cut name still shows. Past this many characters, or with
/// a space, what follows the last dot is just more name, as in
/// `Report v1.2 for the board`.
pub(in crate::browser) fn kept_extension(name: &str) -> Option<&str> {
    const LONGEST: usize = 16;
    let dot = name.rfind('.').filter(|dot| *dot > 0)?;
    let extension = &name[dot..];
    (extension.chars().count() <= LONGEST && !extension.contains(' ')).then_some(extension)
}
/// Cuts a name before its extension: `file-with-a-big-n….pdf`.
pub(in crate::browser) fn name_overflow(name: &str) -> TextOverflow {
    TextOverflow::Truncate(match kept_extension(name) {
        Some(extension) => format!("…{extension}").into(),
        None => "…".into(),
    })
}
/// The grid's padding across: the list's and each row's, both sides.
pub(in crate::browser) const GRID_PADDING: f32 = 16.;
/// How many cells fit across a grid `width` wide, and how wide each is. The
/// room left over is shared out, as in Finder, rather than left at the right.
pub(in crate::browser) fn grid_layout(width: f32, icon: u32) -> (usize, f32) {
    let least = grid_cell(icon).0;
    let room = (width - GRID_PADDING).max(0.);
    let across = (room / least).floor().max(1.) as usize;
    (across, (room / across as f32).max(least))
}
/// A grid cell's width and height for icons `icon` points across: room for
/// the icon and two lines of name.
pub(in crate::browser) fn grid_cell(icon: u32) -> (f32, f32) {
    let icon = icon as f32;
    (icon + 48., icon + 60.)
}
pub(in crate::browser) fn text_width(
    text: &str,
    size: f32,
    weight: FontWeight,
    window: &Window,
) -> f32 {
    let run = gpui::TextRun {
        len: text.len(),
        font: gpui::Font {
            weight,
            ..gpui::font(theme::UI_FONT)
        },
        color: content(1.),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let text = gpui::SharedString::from(text.to_owned());
    window
        .text_system()
        .shape_line(text, px(size), &[run], None)
        .width
        .as_f32()
}
/// How many of the path's leading levels collapse into one "…" crumb. `widths`
/// runs from the disk to the current folder, each with its chevron. Levels
/// are kept from the right at full width while they fit in `room`; the current
/// folder is always kept.
pub(in crate::browser) fn hidden_levels(widths: &[f32], ellipsis: f32, room: f32) -> usize {
    if widths.iter().sum::<f32>() <= room {
        return 0;
    }
    let mut used = ellipsis;
    let mut shown = 0;
    for width in widths.iter().rev() {
        if shown > 0 && used + width > room {
            break;
        }
        used += width;
        shown += 1;
    }
    widths.len().saturating_sub(shown.max(1))
}
/// Which of Date Modified, Size and Kind a list `width` wide shows. Names keep
/// at least 160 pt: Kind goes first, then the date, then the size.
pub(in crate::browser) fn columns(width: f32) -> (bool, bool, bool) {
    // Each column with its gap, after the list's and the row's padding.
    let (name, date, size, kind) = (32. + 160., 192., 92., 122.);
    (
        width >= name + size + date,
        width >= name + size,
        width >= name + size + date + kind,
    )
}
pub(in crate::browser) fn column(width: f32, right: bool) -> gpui::Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .truncate()
        .text_color(content(0.65))
        .when(right, |d| d.text_right())
}
#[derive(Clone, Copy, Debug)]
pub(in crate::browser) enum Arrow {
    Left,
    Right,
}
#[derive(Debug, PartialEq, Eq)]
pub(in crate::browser) enum TreeStep {
    /// Open or close the folder in place.
    Toggle,
    Select(usize),
    /// Go up to the enclosing folder.
    Up,
    Stay,
}
/// What ← or → does on row `ix` of a tree whose rows have `depths`. `open`
/// is `None` for a file, else whether the folder is open.
pub(in crate::browser) fn tree_step(
    depths: &[usize],
    ix: usize,
    arrow: Arrow,
    open: Option<bool>,
) -> TreeStep {
    let depth = depths.get(ix).copied().unwrap_or(0);
    match (arrow, open) {
        (Arrow::Right, Some(false)) | (Arrow::Left, Some(true)) => TreeStep::Toggle,
        (Arrow::Right, Some(true)) if depths.get(ix + 1) == Some(&(depth + 1)) => {
            TreeStep::Select(ix + 1)
        }
        (Arrow::Right, _) => TreeStep::Stay,
        (Arrow::Left, _) if depth == 0 => TreeStep::Up,
        (Arrow::Left, _) => depths[..ix]
            .iter()
            .rposition(|d| *d + 1 == depth)
            .map_or(TreeStep::Stay, TreeStep::Select),
    }
}
#[derive(Clone, Copy, Debug)]
pub(in crate::browser) enum Move {
    Previous,
    Next,
    First,
    Last,
    /// So many entries on: a row of the grid, or one across.
    By(isize),
}
/// Where a key moves the selection in a list of `len`. With nothing
/// selected, ↓ starts at the top and ↑ at the bottom, as in Finder.
pub(in crate::browser) fn moved(selected: Option<usize>, len: usize, step: Move) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match (step, selected) {
        (Move::First, _) | (Move::Next, None) => 0,
        (Move::Last, _) | (Move::Previous, None) => last,
        (Move::Previous, Some(ix)) => ix.min(last).saturating_sub(1),
        (Move::Next, Some(ix)) => (ix + 1).min(last),
        (Move::By(step), None) => {
            if step > 0 {
                0
            } else {
                last
            }
        }
        (Move::By(step), Some(ix)) => ix.min(last).saturating_add_signed(step).min(last),
    })
}
/// The text a key types, when it's one that should start a search: no ⌘,
/// ⌃ or fn, and not space (kept for Quick Look) or a control key. Option
/// and Shift characters count. A dead key or IME composition has no text
/// yet, so its first character is lost.
pub(in crate::browser) fn typed(keystroke: &Keystroke) -> Option<&str> {
    let modifiers = keystroke.modifiers;
    if modifiers.platform || modifiers.control || modifiers.function {
        return None;
    }
    keystroke.key_char.as_deref().filter(|text| {
        !text.is_empty() && !text.chars().any(|c| c.is_control() || c.is_whitespace())
    })
}
pub(in crate::browser) fn format_date(secs: u64, today: chrono::NaiveDate) -> String {
    let Some(time) = i64::try_from(secs)
        .ok()
        .and_then(|s| DateTime::from_timestamp(s, 0))
    else {
        return "--".into();
    };
    let time = time.with_timezone(&Local);
    if time.date_naive() == today {
        time.format("Today at %H:%M").to_string()
    } else {
        time.format("%-d %b %Y at %H:%M").to_string()
    }
}
/// Finder's decimal units: 1 KB is 1,000 bytes.
pub(in crate::browser) fn format_size(entry: &Entry) -> String {
    if entry.kind != Kind::File {
        return "--".into();
    }
    format_bytes(entry.size)
}

/// Every column but the last.
pub(in crate::browser) const COLUMN_WIDTH: f32 = 220.;

/// A folder row's chevron, which says the next column shows its files.
pub(in crate::browser) fn chevron(ink: Hsla) -> gpui::Svg {
    icon("chevron-right", 12.).flex_shrink_0().text_color(ink)
}

/// A rectangle dragged over the listing to select what it touches. Its
/// corners are in listing coordinates, so scrolling mid-drag keeps the
/// start where it was.
pub(in crate::browser) struct Marquee {
    pub(in crate::browser) start: Point<Pixels>,
    pub(in crate::browser) end: Point<Pixels>,
    /// The pointer in the window, to sweep again when the listing scrolls.
    pub(in crate::browser) pointer: Point<Pixels>,
    /// ⌘ or ⇧ was held: the drag adds to what's selected.
    pub(in crate::browser) additive: bool,
    /// Past a few points of travel; a click until then.
    pub(in crate::browser) dragging: bool,
    /// An auto-scroll step waits on the next frame.
    pub(in crate::browser) scrolling: bool,
}

impl Pane {
    /// Whether the keys go to the list, the search over it, or a context
    /// menu opened on it, which leaves the selection looking as it was.
    pub(in crate::browser) fn list_active(&self, window: &Window, cx: &App) -> bool {
        // A batch's rows stay grey, where its problems read in red.
        self.batch.is_none()
            && (self.list_focus.contains_focused(window, cx)
                || self.search.focus_handle(cx).is_focused(window)
                || self.chrome.menu_here)
    }

    /// The indent for a row's depth, then its disclosure triangle if it's a
    /// folder: pressing it opens or closes the folder in place.
    pub(in crate::browser) fn disclosure(
        &self,
        ix: usize,
        depth: usize,
        open: Option<bool>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id(("disclosure", ix))
            .ml(px(depth as f32 * 16.))
            .size(px(14.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .when_some(open, |d, open| {
                d.cursor_pointer()
                    .child(
                        icon("chevron-right", 12.)
                            .text_color(content(0.65))
                            .when(open, |i| {
                                i.with_transformation(gpui::Transformation::rotate(
                                    gpui::percentage(0.25),
                                ))
                            }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.focus_list(window, cx);
                            if let Some(entry) = this.entries.get(ix) {
                                this.toggle_folder(entry.path.clone(), cx);
                            }
                        }),
                    )
            })
    }

    /// The badge after `entry`'s name, `side` across in `ink`: what a download
    /// of it is doing, or a button that downloads what isn't all here.
    pub(in crate::browser) fn cloud_badge(
        &self,
        entry: &Entry,
        ix: usize,
        side: f32,
        ink: Hsla,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.downloads.read(cx).state(&entry.path).cloned();
        let (glyph, label, ink): (_, &'static str, _) = match &state {
            Some(State::Running) => {
                return Some(
                    div()
                        .id(("cloud-badge", ix))
                        .debug_selector(|| "cloud-downloading".into())
                        .role(gpui::Role::ProgressIndicator)
                        .aria_label("Downloading")
                        .flex_shrink_0()
                        .child(icon("cloud-downloading", side).text_color(ink))
                        .into_any_element(),
                );
            }
            Some(State::Failed(_)) => ("cloud-alert", "Try Again", danger()),
            None if self.in_cloud(entry) => ("cloud-download", "Download", ink),
            None => return None,
        };
        // A 20 pt circle around the glyph, as a row's icon button has, laid
        // over it so it takes no room: the name doesn't move.
        let pad = (20. - side) / 2.;
        let path = entry.path.clone();
        let button = div()
            .id(("cloud-badge", ix))
            .debug_selector(move || format!("cloud-badge-{ix}"))
            .role(gpui::Role::Button)
            .aria_label(label)
            .absolute()
            .top(px(-pad))
            .left(px(-pad))
            .size(px(20.))
            .rounded_full()
            .cursor_pointer()
            .hover(|d| d.bg(content(0.14)))
            .active(|d| d.bg(content(0.2)))
            // The press is the badge's, so it doesn't select, open or drag.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |pane, _, _, cx| {
                cx.stop_propagation();
                pane.download(vec![path.clone()], cx);
            }));
        Some(
            div()
                .relative()
                .flex_shrink_0()
                .size(px(side))
                .child(theme::tip(button, label))
                .child(icon(glyph, side).text_color(ink))
                .into_any_element(),
        )
    }

    pub(in crate::browser) fn rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let archived = self.archived.as_ref();
        let searching = !self.showing_folder() && archived.is_none();
        // A grid pane's batch shows its rows as they are, flat. An archive's
        // folders open as Back can leave them, not in place.
        let tree = self.showing_folder() && self.view == View::List;
        let in_columns = self.layout() == View::Columns;
        let today = Local::now().date_naive();
        let (date, size, kind) = if in_columns {
            (false, false, false)
        } else {
            columns(self.chrome.width)
        };
        let name_room = if in_columns { COLUMN_WIDTH - 64. } else { 260. };
        let (row_height, icon_size, text_size) = cx.global::<Preferences>().row_size.metrics();
        self.list_row_height = row_height;
        // Finder greys the selection when the list isn't where keys go.
        let active = self.list_active(window, cx);
        let drops = self.drag.active() && !self.in_trash(cx) && archived.is_none();
        let id = cx.entity_id();
        range
            .filter_map(|row| {
                let (ix, line) = self.matches.at(row);
                let entry = self.entries.get(ix)?;
                let selected = self.selection.contains(&entry.path);
                if let Some(line) = line {
                    return self
                        .line_row(row, ix, line, selected, active, row_height, icon_size, cx)
                        .map(IntoElement::into_any_element);
                }
                let has_lines = self.matches.last_row_of(ix) != row;
                let missing = self.missing.contains(&entry.path);
                let depth = self.depths.get(ix).copied().unwrap_or(0);
                let open = self.is_open_folder(ix);
                let parent = searching
                    .then(|| entry.path.parent().map(|p| p.display().to_string()))
                    .flatten();
                // Finder's list: alternate rows striped, the selection in the
                // accent with its ink.
                let selected_ink = if active { on_accent() } else { content(1.) };
                let muted = if selected {
                    selected_ink
                } else {
                    content(0.65)
                };
                let rename = self
                    .rename_field(&entry.path, name_room, cx)
                    .or_else(|| self.suggestion_field(&entry.path, name_room, cx));
                let chevron_here = in_columns && open.is_some();
                let grabs =
                    !missing && rename.is_none() && self.batch.is_none() && archived.is_none();
                let problem = archived.and_then(|archived| archived.problems.get(&entry.path));
                let new = self
                    .batch
                    .as_ref()
                    .filter(|_| selected)
                    .and_then(|batch| batch.row(&entry.path, &entry.name, self.folder()));
                let fill = div()
                    .id(("fill", row))
                    .w_full()
                    .h(px(row_height))
                    // The text stays on the bars' inset.
                    .px(px(theme::PANE_INSET - theme::CARD_PADDING))
                    // A file and its lines read as one block, and so do
                    // selected files next to each other, as in Finder.
                    // Column view's rows stand apart, as the sidebar's do.
                    .when(in_columns || !self.selected_beside(ix, -1), |d| {
                        // design: the row radius, on the block's top corners.
                        d.rounded_t(px(theme::RADIUS_ROW))
                    })
                    .when(
                        in_columns || !has_lines && !self.selected_beside(ix, 1),
                        |d| {
                            // design: the row radius, on the block's bottom corners.
                            d.rounded_b(px(theme::RADIUS_ROW))
                        },
                    )
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_size(px(text_size))
                    .map(|d| self.row_fill(d, ix, selected, active))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(tree, |d| d.child(self.disclosure(ix, depth, open, cx)))
                            // As in Finder, the rest of a row that isn't
                            // selected starts a rectangle. The name gives
                            // way only once its note or parent has none
                            // left, since those two have no width of their own.
                            .child(
                                div()
                                    .id(("grab", ix))
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .when(grabs && !selected, |d| Self::grab(d, ix, cx))
                                    .child(if archived.is_some() {
                                        file_icons::kind_icon(
                                            entry.kind,
                                            &entry.path,
                                            icon_size,
                                            window,
                                            cx,
                                        )
                                    } else {
                                        file_icons::icon(entry, icon_size, window, cx)
                                    })
                                    // The badge sits closer to the name than the
                                    // name to its icon.
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .child(match rename {
                                                // The name stays put; its outline grows around it.
                                                Some(field) => field.ml(px(-7.)).into_any_element(),
                                                None => {
                                                    let name = new
                                                        .as_ref()
                                                        .map_or(&entry.name, |new| &new.name);
                                                    div()
                                                        .min_w_0()
                                                        .max_w(px(360.))
                                                        .overflow_hidden()
                                                        .whitespace_nowrap()
                                                        .text_overflow(name_overflow(name))
                                                        .map(|d| match &new {
                                                            Some(new) if new.problem => {
                                                                d.text_color(danger())
                                                            }
                                                            Some(new) if new.renamed => {
                                                                d.font_weight(FontWeight::MEDIUM)
                                                            }
                                                            _ => d,
                                                        })
                                                        .child(name.clone())
                                                        .map(|d| match selected && new.is_some() {
                                                            true => self.suggested_name(
                                                                d,
                                                                &entry.path,
                                                                name,
                                                                cx,
                                                            ),
                                                            false => d,
                                                        })
                                                        .into_any_element()
                                                }
                                            })
                                            .children(self.cloud_badge(
                                                entry,
                                                ix,
                                                LIST_BADGE,
                                                if selected {
                                                    selected_ink
                                                } else {
                                                    content(0.55)
                                                },
                                                cx,
                                            )),
                                    ),
                            )
                            .children(new.and_then(|new| {
                                let ink = if new.problem { danger() } else { content(0.65) };
                                let note = new.note?;
                                Some(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(ink)
                                        .child(note),
                                )
                            }))
                            .children(parent.map(|parent| {
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(muted)
                                    .child(parent)
                            })),
                    )
                    .when(chevron_here, |d| {
                        d.child(chevron(if selected {
                            selected_ink
                        } else {
                            content(0.55)
                        }))
                    })
                    .when(date, |d| {
                        d.child(column(180., false).text_color(muted).child(if missing {
                            "--".to_string()
                        } else {
                            format_date(entry.modified, today)
                        }))
                    })
                    .when(size, |d| {
                        d.child(
                            column(80., true)
                                .text_color(muted)
                                .child(format_size(entry)),
                        )
                    })
                    .when(kind, |d| {
                        d.child(match problem {
                            // Selecting it shows why in the archive's bar.
                            Some(_) => column(110., false)
                                .text_color(if selected { selected_ink } else { danger() })
                                .child("Can't extract"),
                            None => column(110., false).text_color(muted).child(if missing {
                                "Missing".to_string()
                            } else {
                                entry.kind_label()
                            }),
                        })
                    })
                    .when(missing, |d| d.opacity(0.5))
                    .when(self.drag.outlines(&Spot::Row(id, ix)), |d| {
                        d.relative().child(
                            self.drag
                                .pulsed(theme::drop_outline().rounded(px(theme::RADIUS_ROW))),
                        )
                    });
                // The row's handlers take its gap too, so a press or a drop
                // there lands on it.
                Some(
                    div()
                        .id(row)
                        .role(gpui::Role::ListItem)
                        .aria_label(entry.name.clone())
                        .aria_selected(selected)
                        .w_full()
                        .when(in_columns, |d| d.pb(px(theme::ROW_GAP)))
                        .child(fill)
                        .when(grabs && selected, |d| Self::grab(d, ix, cx))
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.click_entry(ix, event, window, cx)
                        }))
                        .map(|d| Self::context_target(d, ix, cx))
                        .when(drops, |d| Self::takes_drops(d, ix, cx))
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// The rectangle, drawn over the listing where it is now.
    pub(in crate::browser) fn marquee_overlay(&self) -> Option<impl IntoElement + use<>> {
        let marquee = self.marquee.as_ref().filter(|m| m.dragging)?;
        let offset = self.scroll.0.borrow().base_handle.offset();
        let corner = marquee.start.min(&marquee.end) + offset;
        let size = marquee.end - marquee.start;
        Some(
            div()
                .absolute()
                .left(corner.x)
                .top(corner.y)
                .w(size.x.abs())
                .h(size.y.abs())
                .bg(accent().opacity(0.15))
                .border_1()
                .border_color(accent().opacity(0.6)),
        )
    }
}
