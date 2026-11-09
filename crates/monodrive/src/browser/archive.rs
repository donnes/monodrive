//! A ZIP archive as the file list shows it: browsed like a folder, read
//! only, under a bar whose one button extracts what's selected, or all of
//! it, to the place it names. Extract on archives selected in a folder and
//! Compress are here too. The owner runs both as jobs, which the ring
//! follows.

use super::pane::{Pane, PaneEvent, Stop, level_name};
use super::{Extract, Places, format_bytes, with_owner};
use crate::session::Shows;
use crate::theme::{self, ButtonLook, content, icon};
use gpui::{App, Context, FontWeight, Window, div, prelude::*, px};
use monodrive_core::archives::{Archive, Refusal};
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::ops::{Conflict, Origin, Request};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// A file's length and modification time when it was read.
pub(super) type Stamp = (u64, Option<SystemTime>);

/// The archive on screen, as last read, and why its rows on screen can't be
/// extracted.
pub(super) struct Archived {
    pub(super) open: Arc<Archive>,
    pub(super) stamp: Stamp,
    /// By row path.
    pub(super) problems: HashMap<PathBuf, String>,
    /// Why all of it can't be extracted, and what it holds once extracted,
    /// worked out with the listing: each walks every entry.
    pub(super) refusal: Option<Refusal>,
    pub(super) claimed: u64,
}

/// Whether `path` is named as a ZIP archive, which is how Finder decides.
pub(super) fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

/// Whether opening `entry` goes into it here: a ZIP, or a link to one,
/// which another app would extract outside the owner.
pub(super) fn opens_here(entry: &Entry) -> bool {
    matches!(entry.kind, Kind::File | Kind::Link) && is_zip(&entry.path)
}

/// The archive at `path`, read again only if the file changed since
/// `cached` was. Off the UI thread: it reads the whole central directory.
pub(super) fn read(
    path: &Path,
    cached: Option<(Stamp, Arc<Archive>)>,
) -> Result<(Arc<Archive>, Stamp), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stamp = (meta.len(), meta.modified().ok());
    match cached {
        Some((was, open)) if was == stamp => Ok((open, stamp)),
        _ => Ok((Arc::new(Archive::open(path)?), stamp)),
    }
}

/// What Extract would do from where it was given, cheap enough to work out
/// each frame: it looks only at what's selected.
struct Extraction {
    /// "Extract 2 Items to Downloads".
    label: String,
    /// Why it can't run, for the bar.
    refused: Option<String>,
}

impl Pane {
    /// Whether the rows on screen are an archive's. That follows the rows,
    /// not where the pane is going, so they stay read only until the
    /// listing that replaces them lands.
    pub(super) fn showing_archive(&self) -> bool {
        self.archived.is_some()
    }

    /// Whether the pane asks for an archive's rows: what the next listing
    /// reads.
    pub(super) fn wants_archive(&self) -> bool {
        self.query.is_empty() && self.stop().shows == Shows::Archive
    }

    /// Goes into the archive at `path` as into a folder. Nothing is
    /// extracted, and nothing in it runs.
    pub(super) fn open_archive(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.push_stop(Stop::inside(path.clone(), path), cx);
    }

    /// The archive as last read, for a listing to keep if it hasn't changed.
    pub(super) fn cached_archive(&self, path: &Path) -> Option<(Stamp, Arc<Archive>)> {
        self.archived
            .as_ref()
            .filter(|archived| archived.open.path == path)
            .map(|archived| (archived.stamp, archived.open.clone()))
    }

    /// Where Extract puts things: the folder the other pane shows, when it
    /// takes files, or else `beside`.
    fn extract_to(&self, beside: &Path) -> PathBuf {
        self.chrome
            .extract_to
            .clone()
            .unwrap_or_else(|| beside.to_path_buf())
    }

    /// What Extract would do here: inside an archive, the selected entries
    /// or all of it; in a folder, each selected archive, whole. `None` when
    /// it has nothing to act on.
    fn extraction(&self, cx: &App) -> Option<Extraction> {
        if let Some(archived) = &self.archived {
            let archive = &archived.open.path;
            let into = self.extract_to(archive.parent().unwrap_or(archive));
            // An archive opened in the Trash has nowhere beside it to go.
            let into_trash = cx
                .try_global::<Places>()
                .is_some_and(|places| places.trash == into);
            let selected = self.selection.len();
            let what = match selected {
                0 => "Extract All".to_string(),
                1 => "Extract 1 Item".to_string(),
                n => format!("Extract {n} Items"),
            };
            // Too many entries or a zip bomb refuse any extraction; an entry
            // that can't be extracted refuses only what takes it.
            let refused = match archived.refusal {
                Some(Refusal::TooMany) => Some(format!(
                    "Shows {} of its {} entries, too many to extract.",
                    archived.open.entries.len(),
                    archived.open.total
                )),
                Some(Refusal::TooLarge) => {
                    Some("Claims to expand more than ZIP can, as zip bombs do.".to_string())
                }
                Some(Refusal::Unextractable(n)) if selected == 0 => Some(match n {
                    1 => "1 entry can't be extracted, so Extract All won't run.".to_string(),
                    n => format!("{n} entries can't be extracted, so Extract All won't run."),
                }),
                _ => self
                    .selection
                    .paths()
                    .find_map(|path| archived.problems.get(path))
                    .map(|why| format!("Can't extract what's selected: it's {why}.")),
            };
            let refused = refused
                .or_else(|| {
                    archived
                        .open
                        .entries
                        .is_empty()
                        .then(|| "Nothing to extract.".to_string())
                })
                .or_else(|| into_trash.then(|| "Nothing is extracted into the Trash.".to_string()));
            return Some(Extraction {
                label: format!("{what} to {}", level_name(&into)),
                refused,
            });
        }
        let archives = self.takes_files(cx)
            && !self.selection.is_empty()
            && self.selection.paths().all(is_zip);
        archives.then(|| Extraction {
            label: format!("Extract to {}", level_name(&self.extract_to(self.folder()))),
            refused: None,
        })
    }

    /// Extract can run here now.
    pub(super) fn can_extract(&self, cx: &App) -> bool {
        self.extraction(cx)
            .is_some_and(|extraction| extraction.refused.is_none())
    }

    /// Extract: queues each extraction as a job and follows it. A name taken
    /// where it goes stops the job, and the jobs popover asks.
    pub(super) fn extract(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.can_extract(cx) {
            return;
        }
        let requests = match &self.archived {
            Some(archived) => {
                let archive = archived.open.path.clone();
                let entries = self
                    .chosen()
                    .iter()
                    .filter_map(|path| path.strip_prefix(&archive).ok())
                    .map(|inside| inside.to_string_lossy().into_owned())
                    .collect();
                vec![Request::Extract {
                    into: self.extract_to(archive.parent().unwrap_or(&archive)),
                    archive,
                    entries,
                    conflict: Conflict::Stop,
                }]
            }
            _ => {
                let into = self.extract_to(self.folder());
                self.chosen()
                    .into_iter()
                    .map(|archive| Request::Extract {
                        archive,
                        entries: Vec::new(),
                        into: into.clone(),
                        conflict: Conflict::Stop,
                    })
                    .collect()
            }
        };
        self.queue(requests, window, cx);
    }

    /// Compress: the selection, all in this folder, into a new ZIP beside it.
    pub(super) fn compress(&mut self, window: &Window, cx: &mut Context<Self>) {
        let paths = self.chosen();
        if !paths.is_empty() {
            self.queue(vec![Request::Compress { paths }], window, cx);
        }
    }

    /// Compress can run here now: something's selected in a folder that
    /// takes files, where the new archive goes.
    /// Not while an archive's rows are still on screen, on the way back to
    /// the folder.
    pub(super) fn can_compress(&self, cx: &App) -> bool {
        self.takes_files(cx)
            && !self.showing_archive()
            && !self.selection.is_empty()
            && !self.unchecked()
    }

    /// Asks the owner for each job, and follows those it queued.
    fn queue(&mut self, requests: Vec<Request>, window: &Window, cx: &mut Context<Self>) {
        with_owner(
            window,
            cx,
            move |owner| {
                Ok(requests
                    .into_iter()
                    .map(|request| owner.submit(Origin::Window, request))
                    .collect::<Vec<_>>())
            },
            |this, queued, _, cx| {
                for outcome in queued.into_iter().flatten() {
                    match outcome {
                        Ok(outcome) => cx.emit(PaneEvent::Follow(outcome.op)),
                        Err(e) => this.toast(e, true, None, cx),
                    }
                }
            },
        );
    }

    /// The bar over an archive's list: that it's read only, how much it
    /// holds or why it can't all be extracted, and Extract.
    pub(super) fn archive_bar(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        if self.preview.is_some() {
            return None;
        }
        let archived = self.archived.as_ref()?;
        let extraction = self.extraction(cx)?;
        let note = extraction
            .refused
            .clone()
            .unwrap_or_else(|| format!("{} once extracted", format_bytes(archived.claimed)));
        let enabled = extraction.refused.is_none();
        Some(
            theme::pane_bar(theme::TITLE_BAR_HEIGHT)
                .debug_selector(|| "archive-bar".into())
                .gap_2()
                .border_b_1()
                .border_color(content(0.06))
                .child(icon("lock", 14.).text_color(content(0.55)))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(theme::TEXT_BODY))
                        .font_weight(FontWeight::MEDIUM)
                        .child("Read-only"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(note),
                )
                // The note gives way first, then the label is cut at its
                // end, so where it goes stays on screen as long as it can.
                .child(
                    theme::button(
                        "extract",
                        if enabled {
                            ButtonLook::Primary
                        } else {
                            ButtonLook::Disabled
                        },
                    )
                    .flex_shrink(1.)
                    .min_w_0()
                    .aria_label(extraction.label.clone())
                    .child(div().min_w_0().truncate().child(extraction.label))
                    .debug_selector(|| "extract".into())
                    .when(enabled, |d| {
                        d.on_click(|_, window, cx| window.dispatch_action(Box::new(Extract), cx))
                    }),
                ),
        )
    }
}
