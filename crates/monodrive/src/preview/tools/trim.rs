//! Trimming a video: the bar under it with its two handles, and the trim it sets.

use super::*;

impl PreviewView {
    pub(super) fn start_trim(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (position, length) = {
            let player = self.player.borrow();
            let Some(player) = player.as_ref() else {
                return;
            };
            let Some(length) = player.duration() else {
                return;
            };
            player.pause();
            (player.position(), length)
        };
        let row_width = self.row_width.get();
        let Some(edit) = &mut self.edit else {
            return;
        };
        let Subject::Video {
            start,
            end,
            filmstrip,
            filming,
            ..
        } = &mut edit.subject
        else {
            return;
        };
        let (start, end) = (start.unwrap_or(0.), end.unwrap_or(length));
        edit.trimming = Some(Trimming {
            start,
            end,
            length,
            grab: None,
        });
        if !(start..=end).contains(&position)
            && let Some(player) = self.player.borrow().as_ref()
        {
            player.seek(start);
        }
        if filmstrip.is_empty() && filming.is_none() {
            let path = self.path.clone();
            // About as wide as the strip will be, as it isn't laid out yet.
            let wide = f64::from((row_width - 300.).max(160.));
            let work = cx.background_executor().spawn(async move {
                let (width, height) = edits::video_size(&path)?;
                let shown = f64::from(FILMSTRIP_HEIGHT) * width / height;
                let count = (wide / shown).ceil().clamp(3., 40.) as usize;
                // Twice the points, for a Retina screen.
                edits::filmstrip(&path, count, f64::from(FILMSTRIP_HEIGHT) * 2.)?
                    .into_iter()
                    .map(|bitmap| crate::thumbnails::render_image(bitmap).map(Arc::new))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| "couldn't draw the filmstrip".to_string())
            });
            *filming = Some(cx.spawn_in(window, async move |this, cx| {
                let done = work.await;
                let _ = this.update(cx, |this, cx| {
                    let Some(edit) = &mut this.edit else {
                        return;
                    };
                    let Subject::Video {
                        filmstrip, filming, ..
                    } = &mut edit.subject
                    else {
                        return;
                    };
                    *filming = None;
                    match done {
                        Ok(frames) => *filmstrip = frames,
                        Err(why) => edit.error = Some(why),
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    /// The part of a `length`-second video a trim keeps, while the tools hold one.
    pub(in crate::preview) fn trimmed(&self, length: Option<f64>) -> Option<(f64, f64)> {
        let Subject::Video { start, end, .. } = &self.edit.as_ref()?.subject else {
            return None;
        };
        if start.is_none() && end.is_none() {
            return None;
        }
        Some((start.unwrap_or(0.), end.or(length)?))
    }

    /// Whether the trim bar is up, so ↩ trims.
    pub fn trimming(&self) -> bool {
        self.edit
            .as_ref()
            .is_some_and(|edit| edit.trimming.is_some())
    }

    /// ↩ or Trim: keeps the selection, as one change Undo takes back.
    pub fn apply_trim(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        let Some(trimming) = edit.trimming.take() else {
            return;
        };
        // An end left where the video ends is no trim at that end.
        let near = |a: f64, b: f64| (a - b).abs() < 0.01;
        let kept = (
            (!near(trimming.start, 0.)).then_some(trimming.start),
            (!near(trimming.end, trimming.length)).then_some(trimming.end),
        );
        if let Subject::Video { start, end, .. } = &edit.subject
            && (*start, *end) != kept
        {
            edit.remember();
            if let Subject::Video { start, end, .. } = &mut edit.subject {
                (*start, *end) = kept;
            }
        }
        if let Some(player) = self.player.borrow().as_ref() {
            player.pause();
            player.seek(trimming.start);
        }
        cx.notify();
    }

    pub(super) fn cancel_trim(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.trimming = None;
        }
        if let Some(player) = self.player.borrow().as_ref() {
            player.pause();
        }
        cx.notify();
    }

    /// The seconds under `x` on the filmstrip.
    pub(super) fn trim_time(&self, x: Pixels, length: f64) -> f64 {
        let bounds = self.track.get();
        let width = bounds.size.width.as_f32();
        if width <= 0. {
            return 0.;
        }
        f64::from(((x - bounds.origin.x).as_f32() / width).clamp(0., 1.)) * length
    }

    pub(super) fn trim_press(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let width = self.track.get().size.width.as_f32();
        let Some(trimming) = self.edit.as_ref().and_then(|edit| edit.trimming.as_ref()) else {
            return;
        };
        let at = self.trim_time(x, trimming.length);
        // A handle's reach, in seconds: its width, and a little past its outer side.
        let points = trimming.length / f64::from(width.max(1.));
        let (inner, outer) = (
            f64::from(TRIM_HANDLE + 2.) * points,
            f64::from(GRIP / 2.) * points,
        );
        let on_start = at >= trimming.start - outer && at <= trimming.start + inner;
        let on_end = at <= trimming.end + outer && at >= trimming.end - inner;
        let grab = match (on_start, on_end) {
            (true, true) if at - trimming.start > trimming.end - at => {
                (TrimGrab::End, trimming.end - at)
            }
            (true, _) => (TrimGrab::Start, trimming.start - at),
            (_, true) => (TrimGrab::End, trimming.end - at),
            _ => (TrimGrab::Playhead, 0.),
        };
        if let Some(player) = self.player.borrow().as_ref() {
            player.pause();
        }
        if let Some(trimming) = self.edit.as_mut().and_then(|edit| edit.trimming.as_mut()) {
            trimming.grab = Some(grab);
        }
        self.trim_drag(x, cx);
    }

    /// Moves what the press took, and shows its frame in the player.
    pub(super) fn trim_drag(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let width = f64::from(self.track.get().size.width.as_f32().max(1.));
        let Some(length) = self
            .edit
            .as_ref()
            .and_then(|edit| edit.trimming.as_ref())
            .map(|trimming| trimming.length)
        else {
            return;
        };
        let at = self.trim_time(x, length);
        let Some(trimming) = self.edit.as_mut().and_then(|edit| edit.trimming.as_mut()) else {
            return;
        };
        let Some((grab, offset)) = trimming.grab else {
            return;
        };
        // The frame never narrows past its two handles.
        let least = (length * f64::from(TRIM_HANDLE) * 2. / width).min(length);
        let at = at + offset;
        let shown = match grab {
            TrimGrab::Start => {
                trimming.start = at.min(trimming.end - least).max(0.);
                trimming.start
            }
            TrimGrab::End => {
                trimming.end = at.max(trimming.start + least).min(length);
                trimming.end
            }
            TrimGrab::Playhead => at.clamp(trimming.start, trimming.end),
        };
        if let Some(player) = self.player.borrow().as_ref() {
            player.seek(shown);
        }
        cx.notify();
    }

    pub(super) fn trim_release(&mut self, cx: &mut Context<Self>) {
        if let Some(trimming) = self.edit.as_mut().and_then(|edit| edit.trimming.as_mut()) {
            trimming.grab = None;
        }
        cx.notify();
    }

    /// The trim bar, in place of the player's strip while trimming: Play, the
    /// filmstrip with its frame and playhead, the times, Cancel and Trim.
    pub(in crate::preview) fn trim_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let edit = self.edit.as_ref()?;
        let trimming = edit.trimming.as_ref()?;
        let Subject::Video { filmstrip, .. } = &edit.subject else {
            return None;
        };
        let (start, end, length) = (trimming.start, trimming.end, trimming.length);
        let grab = trimming.grab.map(|(grab, _)| grab);
        let frames = filmstrip.clone();
        let (playing, position) = self
            .player
            .borrow()
            .as_ref()
            .map_or((false, start), |player| {
                (player.playing(), player.position())
            });
        self.keep_ticking(cx);
        let fraction = |seconds: f64| (seconds / length).clamp(0., 1.) as f32;
        let play = if playing {
            ("Pause", "pause")
        } else {
            ("Play", "play")
        };
        let measured = self.track.clone();
        let view = cx.entity().downgrade();
        // design: a scrim over the video's frames, which are the file's colors, not the app's.
        let dim = hsla([0., 0., 0., 0.55]);
        let radius = px(theme::RADIUS_ROW);
        let handle = |side: &'static str| {
            div()
                .id(side)
                .flex_shrink_0()
                .h_full()
                .w(px(TRIM_HANDLE))
                .bg(theme::accent())
                // design: the frame's outer corners, at RADIUS_ROW.
                .when(side == "trim-start", |d| d.rounded_l(radius))
                // design: the frame's outer corners, at RADIUS_ROW.
                .when(side == "trim-end", |d| d.rounded_r(radius))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(2.))
                .cursor(CursorStyle::ResizeLeftRight)
                .children((0..2).map(|_| {
                    div()
                        .w(px(2.))
                        .h(px(14.))
                        .rounded_full()
                        .bg(theme::on_accent())
                }))
        };
        // The strip's own ends are rounded as the frame is, so no frame's
        // corner shows past the frame's when it reaches them.
        let last = frames.len().saturating_sub(1);
        let strip = div()
            .id("trim-strip")
            .debug_selector(|| "trim-strip".into())
            .relative()
            .flex_1()
            .min_w_0()
            .h(px(FILMSTRIP_HEIGHT))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.trim_press(event.position.x, cx);
                }),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .rounded(px(theme::RADIUS_ROW))
                    .bg(content(0.06))
                    .children(frames.into_iter().enumerate().map(|(at, frame)| {
                        gpui::img(frame)
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .object_fit(gpui::ObjectFit::Cover)
                            // design: the strip's outer corners, at RADIUS_ROW.
                            .when(at == 0, |d| d.rounded_l(radius))
                            // design: the strip's outer corners, at RADIUS_ROW.
                            .when(at == last, |d| d.rounded_r(radius))
                    })),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    // On under the frame's rounded corners.
                    .right(gpui::relative(1. - fraction(start)))
                    .mr(-radius)
                    // design: the strip's outer corners, at RADIUS_ROW.
                    .rounded_l(radius)
                    .bg(dim),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .left(gpui::relative(fraction(end)))
                    .ml(-radius)
                    // design: the strip's outer corners, at RADIUS_ROW.
                    .rounded_r(radius)
                    .bg(dim),
            )
            // The frame's sides are its handles, inside the selection's ends.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .left(gpui::relative(fraction(start)))
                    .w(gpui::relative(fraction(end) - fraction(start)))
                    .flex()
                    .child(handle("trim-start"))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .border_color(theme::accent())
                            .border_t(px(3.))
                            .border_b(px(3.)),
                    )
                    .child(handle("trim-end")),
            )
            .child(
                div()
                    .absolute()
                    .top(px(-4.))
                    .bottom(px(-4.))
                    .left(gpui::relative(fraction(position.clamp(start, end))))
                    .ml(px(-1.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(theme::knob())
                    .shadow(theme::knob_shadow()),
            )
            .child(
                canvas(
                    move |bounds, _, _| measured.set(bounds),
                    // A drag goes on past the strip's edges, and over the player.
                    move |_, _, window, _| {
                        if grab.is_none() {
                            return;
                        }
                        let moved = view.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == gpui::DispatchPhase::Bubble {
                                let _ = moved
                                    .update(cx, |this, cx| this.trim_drag(event.position.x, cx));
                            }
                        });
                        let released = view.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase == gpui::DispatchPhase::Bubble {
                                let _ = released.update(cx, |this, cx| this.trim_release(cx));
                            }
                        });
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        let bar = theme::pane_bar(FILMSTRIP_HEIGHT + 16.)
            .debug_selector(|| "trim-bar".into())
            .gap_3()
            .border_t_1()
            .border_color(content(0.06))
            .child(
                // As tall as the filmstrip beside it, as QuickTime's is.
                theme::tip_below(
                    div()
                        .id("trim-play")
                        .role(gpui::Role::Button)
                        .aria_label(play.0)
                        .flex_shrink_0()
                        .size(px(FILMSTRIP_HEIGHT))
                        .rounded(px(theme::RADIUS_ROW))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .bg(content(0.08))
                        .hover(|s| s.bg(content(0.14)))
                        .active(|s| s.bg(content(0.2)))
                        .child(theme::icon(play.1, 16.).text_color(content(1.))),
                    play.0,
                )
                .debug_selector(|| "trim-play".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    let range = this
                        .edit
                        .as_ref()
                        .and_then(|edit| edit.trimming.as_ref())
                        .map(|trimming| (trimming.start, trimming.end));
                    if let Some(player) = this.player.borrow().as_ref() {
                        player.toggle(range);
                    }
                    cx.notify();
                })),
            )
            .child(strip);
        Some(bar.into_any_element())
    }
}
