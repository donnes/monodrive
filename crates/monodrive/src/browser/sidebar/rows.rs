//! The sidebar's rows: favorites, cloud folders, drives, shares and servers,
//! the headers over them and the foot under them, each with where its presses
//! and drops land.

use super::*;

impl Browser {
    /// Where a row's presses land, the gap under it included. An edited
    /// favorite goes nowhere when clicked.
    pub(super) fn slot(
        ix: usize,
        selected: bool,
        clickable: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        theme::row_slot(("place", ix))
            .role(Role::Button)
            .aria_selected(selected)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.place_menu(ix, event.position, window, cx)
                }),
            )
            .map(|d| {
                if clickable {
                    d.on_click(cx.listener(move |this, _, window, cx| {
                        this.sidebar_cursor = ix;
                        this.open_place(window, cx);
                    }))
                } else {
                    d.cursor_default()
                }
            })
    }

    /// A row's frame: the keyboard cursor's border and its highlight.
    pub(super) fn row(
        ix: usize,
        selected: bool,
        clickable: bool,
        cursor: Option<usize>,
    ) -> gpui::Div {
        div()
            .debug_selector(move || format!("place-{ix}"))
            // The sidebar's keyboard cursor. The border is always there, so
            // showing it doesn't shift the row.
            .border_1()
            .border_color(if cursor == Some(ix) {
                accent().opacity(0.7)
            } else {
                gpui::transparent_black()
            })
            .flex_shrink_0()
            // 8 pt with the border, so icons sit under the section labels.
            .px(px(7.))
            .rounded(px(theme::RADIUS_ROW))
            .text_size(px(theme::TEXT_TITLE))
            .font_weight(FontWeight::MEDIUM)
            .text_color(content(if selected { 1. } else { 0.65 }))
            .when(selected, |d| d.bg(content(0.14)))
            .when(clickable && !selected, |d| {
                d.group_hover(theme::ROW_GROUP, |s| {
                    s.bg(content(0.08)).text_color(content(1.))
                })
            })
    }

    /// What a sidebar row needs while files are dragged: it takes them into
    /// `folder` if it has one, and shows whether a drop would land.
    pub(super) fn drop_row(
        &self,
        slot: Stateful<gpui::Div>,
        ix: usize,
        folder: Option<(PathBuf, String)>,
    ) -> Stateful<gpui::Div> {
        let spot = Spot::Place(ix);
        let target = folder.map(|(path, name)| Target::Folder { spot, path, name });
        self.drop_target(slot, spot, target)
    }

    /// `slot` taking a drop on `target`, outlined while a drop would land.
    pub(super) fn drop_target(
        &self,
        slot: Stateful<gpui::Div>,
        spot: Spot,
        target: Option<Target>,
    ) -> Stateful<gpui::Div> {
        let outlined = self.drag.outlines(&spot);
        slot.when_some(target.filter(|_| self.drag.active()), |d, target| {
            let drag = self.drag.clone();
            offers(d, move |_| drag.offer(target.clone()))
        })
        .when(outlined, |d| {
            d.relative().child(
                self.drag.pulsed(
                    theme::drop_outline()
                        .bottom(px(theme::ROW_GAP))
                        .rounded(px(theme::RADIUS_ROW)),
                ),
            )
        })
    }

    /// The edges of a row in Favorites while folders are dragged, each
    /// pinning at its position; the line shows in the gap under the row above.
    pub(super) fn pin_edge(&self, at: usize, top: bool) -> gpui::Div {
        let drag = self.drag.clone();
        let edge = div()
            .absolute()
            .left_0()
            .right_0()
            .h(px(PIN_EDGE))
            .map(|d| if top { d.top_0() } else { d.bottom_0() });
        offers(edge, move |_| drag.offer(Target::Pin(at)))
    }

    /// `slot`, the row at position `at` in Favorites, with its edges while
    /// folders are dragged.
    pub(super) fn pin_edges(&self, slot: Stateful<gpui::Div>, at: usize) -> Stateful<gpui::Div> {
        let pins = self.drag.folders();
        slot.when(pins, |d| {
            d.relative()
                .child(self.pin_edge(at, true))
                .child(self.pin_edge(at + 1, false))
        })
        .when(self.drag.pins_at() == Some(at + 1), |d| {
            d.relative().child(theme::insertion_line())
        })
    }

    /// A row at the top that opens a page.
    pub(super) fn page_row(
        ix: usize,
        name: &'static str,
        glyph: &'static str,
        selected: bool,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        Self::slot(ix, selected, true, cx).aria_label(name).child(
            Self::row(ix, selected, true, cursor)
                .h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .gap_2()
                .child(icon(glyph, 16.).text_color(content(if selected { 1. } else { 0.55 })))
                .child(div().flex_1().truncate().child(name)),
        )
    }

    pub(super) fn favorite_row(
        &self,
        fav: usize,
        here: &Here,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let place = &self.sidebar.places[fav];
        let ix = fav + self.first_favorite();
        let label = label_of(&place.favorite.path);
        let glyph = glyph_of(&place.favorite.path);
        let selected = self.is_open(Row::Favorite(fav), here);
        let reason = match &place.availability {
            Availability::Available(_) => None,
            Availability::Unavailable(why) => Some(why.clone()),
        };
        let unavailable = reason.is_some();
        let editing = self.sidebar.editing_favorites;
        let key =
            (self.sidebar.command_held && !editing && !self.sheet_open() && self.menu.is_none())
                .then(|| commands::favorite_shortcut(fav, cx))
                .flatten();
        let row = Self::row(ix, selected, !editing, cursor)
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .when(unavailable, |d| d.text_color(content(0.3)));
        let row = if editing {
            row.drag_over::<DraggedFavorite>(|s, _, _, _| s.bg(content(0.14)))
                .child(
                    drag_handle(("favorite-handle", fav), format!("favorite-handle-{fav}"))
                        .on_drag(
                            DraggedFavorite {
                                ix: fav,
                                label: label.clone(),
                            },
                            |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                        ),
                )
        } else {
            row
        };
        let row = row
            .child(icon(glyph, 16.).text_color(content(if unavailable {
                0.3
            } else if selected {
                1.
            } else {
                0.55
            })))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().line_height(px(16.)).child(label.clone()))
                    .children(reason.map(|why| {
                        div()
                            .truncate()
                            .line_height(px(13.))
                            .text_size(px(theme::TEXT_CAPTION))
                            .font_weight(FontWeight::NORMAL)
                            .child(why)
                    })),
            )
            .children(key.map(|key| {
                theme::key_cap(key).debug_selector(move || format!("favorite-key-{fav}"))
            }))
            .when(editing, |d| {
                d.child(
                    row_button(
                        ("favorite-remove", fav),
                        format!("favorite-remove-{fav}"),
                        "Remove from Sidebar",
                        "cancel-01",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_favorite(fav, cx))),
                )
            });
        let folder = place.at().map(|path| (path.to_path_buf(), label.clone()));
        Self::slot(ix, selected, !editing, cx)
            .aria_label(label)
            .when(editing, |d| {
                d.on_drop(cx.listener(move |this, dragged: &DraggedFavorite, _, cx| {
                    if dragged.ix != fav {
                        feedback::haptic(Haptic::Generic, cx);
                    }
                    this.move_favorite(dragged.ix, fav, cx);
                }))
            })
            .child(row)
            .map(|d| self.drop_row(d, ix, folder))
            .map(|d| self.pin_edges(d, fav))
    }

    pub(super) fn cloud_row(
        &self,
        at: usize,
        ix: usize,
        here: &Here,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let folder = &self.sidebar.cloud[at];
        let hidden = self.hidden(Row::Cloud(at), cx);
        let selected = self.is_open(Row::Cloud(at), here);
        let name = cloud_name(folder, cx).to_owned();
        let renaming = self
            .sidebar
            .renaming
            .as_ref()
            .filter(|renaming| renaming.path == folder.path);
        let space = self
            .sidebar
            .cloud_free
            .get(&folder.path)
            .map(|quota| match quota {
                Quota::Free(bytes) => ("free", format!("{} free", format_bytes(*bytes))),
                Quota::Failed => ("failed", "Couldn’t read free space".into()),
            });
        let described = space.as_ref().map(|(_, text)| text.clone());
        let reports = folder.reports_free_space();
        let row = self
            .location_frame(ix, selected, hidden, cursor)
            .h(px(if reports { DRIVE_HEIGHT } else { ROW_HEIGHT }))
            .flex()
            .flex_col()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(24.))
                    .children(self.location_handle(ix, &name))
                    .child(icon("cloud", 16.).text_color(glyph_ink(selected, hidden)))
                    .map(|d| match renaming {
                        Some(renaming) => d.child(Self::cloud_name_field(renaming, cx)),
                        None => d.child(div().flex_1().min_w_0().truncate().child(name.clone())),
                    })
                    .children(self.eye_button(Row::Cloud(at), ix, hidden, cx)),
            )
            // The line keeps its room until the provider answers.
            .when(reports, |d| {
                d.child(
                    self.detail_line()
                        .text_size(px(theme::TEXT_CAPTION))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(content(if hidden { 0.3 } else { 0.65 }))
                        .children(space.map(|(said, text)| {
                            div()
                                .debug_selector(move || format!("cloud-{said}-{at}"))
                                .min_w_0()
                                .truncate()
                                .child(text)
                        })),
                )
            });
        let target = (folder.path.clone(), name.clone());
        self.location_slot(ix, selected, cx)
            .aria_label(name)
            .when_some(described, |d, text| d.aria_description(text))
            .child(row)
            .map(|d| self.drop_row(d, ix, Some(target)))
    }

    /// The cloud row's name as a field, fitting inside the row.
    pub(super) fn cloud_name_field(renaming: &CloudRenaming, cx: &mut Context<Self>) -> gpui::Div {
        let mut fill = theme::pane_fill();
        fill.a = 1.;
        theme::field(accent())
            .debug_selector(|| "cloud-name-field".into())
            .key_context("Rename")
            .on_action(
                cx.listener(|this, _: &crate::browser::CommitRename, window, cx| {
                    this.commit_cloud_name(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::browser::CancelRename, window, cx| {
                    this.end_cloud_name(window, cx)
                }),
            )
            // A click in the name places the caret; it doesn't open the row.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex_1()
            .min_w_0()
            .px(px(6.))
            .bg(fill)
            .text_color(content(1.))
            .child(renaming.field.clone())
    }

    /// Rename Location: the cloud row the keys are on takes a field with its
    /// name, and an empty field goes back to the folder's own.
    pub(super) fn rename_cloud(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.sidebar.cloud.get(at) else {
            return;
        };
        let path = folder.path.clone();
        let own = folder.name.clone();
        let name = cloud_name(folder, cx).to_owned();
        let field = cx.new(|cx| {
            let mut field = TextField::new("Name", own, cx).with_font_size(theme::TEXT_TITLE);
            field.set_text(&name, 0..name.len(), cx);
            field
        });
        let focus = field.focus_handle(cx);
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            this.commit_cloud_name(window, cx)
        });
        window.focus(&focus, cx);
        self.sidebar.renaming = Some(CloudRenaming {
            path,
            field,
            _blur: blur,
        });
        cx.notify();
    }

    /// ↩ or clicking away keeps the name; an empty one, or the folder's own,
    /// forgets it.
    pub(super) fn commit_cloud_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = &self.sidebar.renaming else {
            return;
        };
        let name = renaming.field.read(cx).text().trim().to_owned();
        let path = renaming.path.clone();
        let own = self
            .sidebar
            .cloud
            .iter()
            .find(|folder| folder.path == path)
            .map(|folder| folder.name.clone());
        preferences::update(cx, |prefs| {
            if name.is_empty() || own.as_ref() == Some(&name) {
                prefs.cloud_names.remove(&path);
            } else {
                prefs.cloud_names.insert(path, name);
            }
        });
        self.end_cloud_name(window, cx);
    }

    pub(super) fn end_cloud_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(renaming) = self.sidebar.renaming.take()
            && renaming.field.focus_handle(cx).is_focused(window)
        {
            window.focus(&self.sidebar_focus, cx);
        }
        cx.notify();
    }

    pub(super) fn volume_row(
        &self,
        vol: usize,
        ix: usize,
        here: &Here,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let volume = &self.sidebar.volumes[vol];
        let editing = self.sidebar.editing_locations;
        let hidden = self.hidden(Row::Volume(vol), cx);
        let selected = self.is_open(Row::Volume(vol), here);
        let mount = volume.mount.clone();
        let glyph = if volume.network {
            "server"
        } else {
            "hard-drive"
        };
        let row = self
            .location_frame(ix, selected, hidden, cursor)
            .h(px(DRIVE_HEIGHT))
            .flex()
            .flex_col()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(24.))
                    .children(self.location_handle(ix, &volume.name))
                    .child(icon(glyph, 16.).text_color(glyph_ink(selected, hidden)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(volume.name.clone()),
                    )
                    .when(volume.read_only, |d| {
                        d.child(
                            div()
                                .debug_selector(move || format!("read-only-{vol}"))
                                .flex_shrink_0()
                                .text_size(px(theme::TEXT_CAPTION))
                                .font_weight(FontWeight::NORMAL)
                                .text_color(content(if hidden { 0.3 } else { 0.65 }))
                                .child("Read-only"),
                        )
                    })
                    .when(!volume.startup && !editing, |d| {
                        d.child(
                            row_button(
                                ("volume-eject", vol),
                                format!("volume-eject-{vol}"),
                                "Eject",
                                "eject",
                            )
                            .occlude()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.eject(mount.clone(), cx);
                                },
                            )),
                        )
                    })
                    .children(self.eye_button(Row::Volume(vol), ix, hidden, cx)),
            )
            .children(fullness(volume).map(|fullness| {
                self.detail_line().child(
                    theme::progress_track()
                        .debug_selector(move || format!("volume-bar-{vol}"))
                        .flex_1()
                        .child(
                            theme::progress_fill(fullness)
                                .debug_selector(move || format!("volume-used-{vol}"))
                                .bg(content(if hidden { 0.3 } else { 0.55 })),
                        ),
                )
            }));
        // A drive that can't be written to takes nothing.
        let folder = (!volume.read_only).then(|| (volume.mount.clone(), volume.name.clone()));
        self.location_slot(ix, selected, cx)
            .aria_label(volume.name.clone())
            .child(row)
            .map(|d| self.drop_row(d, ix, folder))
    }

    /// AirDrop or LocalSend: a page that opens on a click and takes what's
    /// dropped on it.
    pub(super) fn share_row(
        &self,
        ix: usize,
        name: &'static str,
        glyph: &'static str,
        selected: bool,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let frame = self
            .location_frame(ix, selected, false, cursor)
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .children(self.location_handle(ix, name))
            .child(icon(glyph, 16.).text_color(glyph_ink(selected, false)))
            .child(div().flex_1().truncate().child(name));
        self.location_slot(ix, selected, cx)
            .aria_label(name)
            .child(frame)
    }

    /// A saved SFTP server: Eject while it's connected, and why when it
    /// couldn't connect, on a second line.
    pub(super) fn server_row(
        &self,
        at: usize,
        ix: usize,
        here: &Here,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let source = &self.sidebar.remotes[at];
        let editing = self.sidebar.editing_locations;
        let selected = self.is_open(Row::Server(at), here);
        let link = crate::system::link(&source.id, cx).cloned();
        let failed = match &link {
            Some(crate::system::Link::Failed(error)) => Some(crate::system::short_reason(error)),
            _ => None,
        };
        let id = source.id.clone();
        let row = self
            .location_frame(ix, selected, false, cursor)
            .h(px(if failed.is_some() {
                DRIVE_HEIGHT
            } else {
                ROW_HEIGHT
            }))
            .flex()
            .items_center()
            .gap_2()
            .children(self.location_handle(ix, &source.id))
            .child(icon("server", 16.).text_color(glyph_ink(selected, false)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().child(source.id.clone()))
                    .children(failed.map(|why| {
                        div()
                            .debug_selector(move || format!("server-failed-{at}"))
                            .truncate()
                            .text_size(px(theme::TEXT_CAPTION))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme::danger())
                            .child(why)
                    })),
            )
            .when(link == Some(crate::system::Link::Connecting), |d| {
                d.child(
                    div()
                        .debug_selector(move || format!("server-connecting-{at}"))
                        .size(px(12.))
                        .flex_shrink_0()
                        .rounded_full()
                        .border_1()
                        .border_color(content(0.55)),
                )
            })
            .when(
                link == Some(crate::system::Link::Connected) && !editing,
                |d| {
                    d.child(
                        row_button(
                            ("server-disconnect", at),
                            format!("server-disconnect-{at}"),
                            "Disconnect",
                            "eject",
                        )
                        .occlude()
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.disconnect_server(&id, cx)),
                        ),
                    )
                },
            );
        self.location_slot(ix, selected, cx)
            .aria_label(source.id.clone())
            .child(row)
    }

    /// Locations' rows, in the order they're drawn.
    pub(super) fn location_rows(
        &self,
        here: &Here,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let first = self.first_location();
        (self.sidebar.locations.iter().enumerate())
            .filter_map(|(at, row)| {
                let ix = first + at;
                let spot = Spot::Place(ix);
                Some(match *row {
                    Row::Volume(vol) => self
                        .volume_row(vol, ix, here, cursor, cx)
                        .into_any_element(),
                    Row::Cloud(at) => self.cloud_row(at, ix, here, cursor, cx).into_any_element(),
                    Row::Server(at) => self.server_row(at, ix, here, cursor, cx).into_any_element(),
                    Row::AirDrop => {
                        let selected = self.is_open(*row, here);
                        let row = self.share_row(ix, "AirDrop", "airdrop", selected, cursor, cx);
                        self.drop_target(row, spot, Some(Target::AirDrop(spot)))
                            .into_any_element()
                    }
                    Row::LocalSend => {
                        let selected = self.is_open(*row, here);
                        let row =
                            self.share_row(ix, "LocalSend", "localsend", selected, cursor, cx);
                        self.drop_target(row, spot, Some(Target::LocalSendPage(spot)))
                            .into_any_element()
                    }
                    Row::Overview | Row::Recents | Row::Worktrees | Row::Favorite(_) => {
                        return None;
                    }
                })
            })
            .collect()
    }

    /// Whether `row` is a drive Locations shows only while it's edited.
    pub(super) fn hidden(&self, row: Row, cx: &App) -> bool {
        self.sidebar.editing_locations
            && self.hideable(row, cx).is_some_and(|drive| {
                let hidden = &cx.global::<Preferences>().hidden_drives;
                hidden.iter().any(|h| h.drive == drive.drive)
            })
    }

    /// A location's frame, which takes a dragged location while Locations
    /// is edited. A hidden drive's ink is dimmed, as an unavailable
    /// favorite's is.
    pub(super) fn location_frame(
        &self,
        ix: usize,
        selected: bool,
        hidden: bool,
        cursor: Option<usize>,
    ) -> gpui::Div {
        let editing = self.sidebar.editing_locations;
        Self::row(ix, selected, !editing, cursor)
            .when(hidden, |d| d.text_color(content(0.3)))
            .when(editing, |d| {
                d.drag_over::<DraggedLocation>(|s, _, _, _| s.bg(content(0.14)))
            })
    }

    /// A location's slot: it opens its place, or while Locations is edited,
    /// moves the location dropped on it to its position.
    pub(super) fn location_slot(
        &self,
        ix: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let editing = self.sidebar.editing_locations;
        let to = ix - self.first_location();
        Self::slot(ix, selected, !editing, cx).when(editing, |d| {
            d.on_drop(cx.listener(move |this, dragged: &DraggedLocation, _, cx| {
                if dragged.at != to {
                    feedback::haptic(Haptic::Generic, cx);
                }
                this.move_location(dragged.at, to, cx);
            }))
        })
    }

    /// The handle an edited location is dragged by.
    pub(super) fn location_handle(
        &self,
        ix: usize,
        label: &str,
    ) -> Option<impl IntoElement + use<>> {
        let at = ix - self.first_location();
        let label = label.to_owned();
        self.sidebar.editing_locations.then(|| {
            drag_handle(("location-handle", at), format!("location-handle-{at}"))
                .on_drag(DraggedLocation { at, label }, |dragged, _, _, cx| {
                    cx.new(|_| dragged.clone())
                })
        })
    }

    /// The eye an edited drive or cloud folder ends in, which hides it or
    /// shows it again.
    pub(super) fn eye_button(
        &self,
        row: Row,
        ix: usize,
        hidden: bool,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let at = ix - self.first_location();
        let (label, glyph) = if hidden {
            ("Show in Sidebar", "eye-off")
        } else {
            ("Hide from Sidebar", "eye")
        };
        self.sidebar.editing_locations.then(|| {
            row_button(
                ("location-eye", at),
                format!("location-eye-{at}"),
                label,
                glyph,
            )
            .occlude()
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_hidden(row, cx)))
        })
    }

    /// The 12 pt line under a location's name, which holds a drive's
    /// capacity bar or what's left in a cloud account.
    fn detail_line(&self) -> gpui::Div {
        div()
            .h(px(12.))
            .ml(px(self.under_name()))
            .flex_shrink_0()
            .flex()
            .items_center()
    }

    /// Where a drive's second line starts: under its name.
    pub(super) fn under_name(&self) -> f32 {
        if self.sidebar.editing_locations {
            24. + HANDLE_WIDTH
        } else {
            24.
        }
    }

    /// A section's name and its disclosure chevron, which fold its rows
    /// away or bring them back, as Finder's headings do.
    pub(super) fn section_toggle(
        label: &'static str,
        folded: bool,
        fold: fn(&mut Collapsed),
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id(label)
            .debug_selector(move || format!("{}-toggle", label.to_lowercase()))
            .role(Role::Button)
            .aria_label(label)
            .aria_expanded(!folded)
            .h_full()
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .hover(|s| s.text_color(content(1.)))
            .child(label)
            .child(
                icon(
                    if folded {
                        "chevron-right"
                    } else {
                        "chevron-down-hi"
                    },
                    12.,
                )
                .text_color(content(0.55)),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                preferences::update(cx, |prefs| fold(&mut prefs.collapsed))
            }))
    }

    pub(super) fn favorites_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let editing = self.sidebar.editing_favorites;
        let folded = self.sidebar.collapsed.favorites;
        let drag = self.drag.clone();
        // Pins first, so folders can land even with no favorite to drop beside.
        theme::section_line()
            .debug_selector(|| "favorites-header".into())
            .relative()
            .flex_shrink_0()
            .justify_between()
            .when(self.drag.folders(), |d| {
                offers(d, move |_| drag.offer(Target::Pin(0)))
            })
            .when(self.drag.pins_at() == Some(0), |d| {
                d.child(theme::insertion_line())
            })
            .child(Self::section_toggle(
                "Favorites",
                folded,
                |c| c.favorites = !c.favorites,
                cx,
            ))
            .when(!folded, |d| {
                d.child(
                    Self::edit_button("favorites-edit", editing)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_editing_favorites(cx))),
                )
            })
    }

    pub(super) fn locations_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let folded = self.sidebar.collapsed.locations;
        theme::section_line()
            .debug_selector(|| "locations-header".into())
            .flex_shrink_0()
            .justify_between()
            .child(Self::section_toggle(
                "Locations",
                folded,
                |c| c.locations = !c.locations,
                cx,
            ))
            .when(!folded, |d| {
                d.child(
                    Self::edit_button("locations-edit", self.sidebar.editing_locations)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_editing_locations(cx))),
                )
            })
    }

    /// A section's Edit, which says Done while its session lasts.
    pub(super) fn edit_button(id: &'static str, editing: bool) -> Stateful<gpui::Div> {
        let label = if editing { "Done" } else { "Edit" };
        theme::text_button(id, label, ButtonLook::Ghost)
            .debug_selector(move || id.into())
            // Its text, not its pill, ends on the rows' 8 pt line.
            .mr(px(-8.))
            .when(editing, |d| d.text_color(accent()))
    }

    /// The Trash and Settings at the sidebar's foot.
    pub(super) fn foot(&self, here: &Here, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let in_trash = here.in_trash;
        div()
            .h(px(40.))
            .flex_shrink_0()
            // Puts the buttons' glyphs on the rows' icon column.
            .px(px(3.))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                theme::tip(icon_button("trash", "Trash", "trash", true), "Trash")
                    .debug_selector(|| "open-trash".into())
                    .when(in_trash, |d| d.bg(content(0.14)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pane()
                            .update(cx, |pane, cx| pane.open_trash(window, cx));
                    })),
            )
            .children(self.index_bar(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(crate::updates::control(
                        cx.listener(|this, over: &bool, _, cx| this.peek_updates(false, *over, cx)),
                        cx,
                    ))
                    .child(
                        theme::tip(
                            icon_button("settings", "Settings", "settings", true),
                            "Settings",
                        )
                        .track_focus(&self.settings_focus)
                        .focus_visible(|s| s.shadow(theme::focus_ring()))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenSettings), cx)
                        }),
                    ),
            )
    }
}
