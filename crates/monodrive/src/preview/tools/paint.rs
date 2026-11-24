//! Painting the edit over the picture: the crop, each mark and its handles.

use super::*;

/// The crop: the picture dimmed outside it, its edge, and handles at its
/// corners and sides.
pub(super) fn paint_crop(
    rect: Rect,
    origin: Point<Pixels>,
    scale: f32,
    image: (f32, f32),
    window: &mut Window,
) {
    let whole = screen_rect(
        Rect {
            x: 0.,
            y: 0.,
            width: f64::from(image.0),
            height: f64::from(image.1),
        },
        origin,
        scale,
    );
    let frame = screen_rect(rect, origin, scale);
    // design: a scrim over the picture, which is the file's colors, not the app's.
    let dim = hsla([0., 0., 0., 0.5]);
    for part in [
        Bounds::from_corners(whole.origin, point(whole.right(), frame.top())),
        Bounds::from_corners(point(whole.left(), frame.bottom()), whole.bottom_right()),
        Bounds::from_corners(
            point(whole.left(), frame.top()),
            point(frame.left(), frame.bottom()),
        ),
        Bounds::from_corners(
            point(frame.right(), frame.top()),
            point(whole.right(), frame.bottom()),
        ),
    ] {
        window.paint_quad(gpui::fill(part, dim));
    }
    window.paint_quad(gpui::outline(
        frame,
        theme::accent(),
        gpui::BorderStyle::Solid,
    ));
    let handle = px(8.);
    let (left, right, top, bottom) = (frame.left(), frame.right(), frame.top(), frame.bottom());
    let (middle_x, middle_y) = (frame.center().x, frame.center().y);
    for (x, y) in [
        (left, top),
        (middle_x, top),
        (right, top),
        (left, middle_y),
        (right, middle_y),
        (left, bottom),
        (middle_x, bottom),
        (right, bottom),
    ] {
        let at = Bounds::new(
            point(x - handle / 2., y - handle / 2.),
            size(handle, handle),
        );
        window.paint_quad(gpui::fill(at, theme::accent()));
    }
}

/// The mark being drawn, in GPUI, over the picture.
pub(super) fn paint_mark(
    draft: &[[f64; 2]],
    tool: Option<Tool>,
    ink: [f64; 4],
    origin: Point<Pixels>,
    scale: f32,
    image: (f32, f32),
    window: &mut Window,
) {
    let (Some(tool), Some(first), Some(last)) = (tool, draft.first(), draft.last()) else {
        return;
    };
    let on_screen = |p: &[f64; 2]| {
        point(
            origin.x + px(p[0] as f32 * scale),
            origin.y + px(p[1] as f32 * scale),
        )
    };
    let long = image.0.max(image.1);
    let width = px((long / 300.).max(2.) * scale);
    let area = corners(*first, *last, image);
    let frame = Bounds::new(
        point(
            origin.x + px(area.x as f32 * scale),
            origin.y + px(area.y as f32 * scale),
        ),
        size(
            px(area.width as f32 * scale),
            px(area.height as f32 * scale),
        ),
    );
    // design: a mark's own color, part of the file and not the app.
    let color = hsla(ink);
    let stroke =
        |points: &[Point<Pixels>], width: Pixels, color: gpui::Hsla, window: &mut Window| {
            let mut path = PathBuilder::stroke(width);
            path.add_polygon(points, false);
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        };
    match tool {
        Tool::Pen | Tool::Highlighter => {
            let points: Vec<Point<Pixels>> = draft.iter().map(on_screen).collect();
            if points.len() > 1 {
                let (width, color) = if tool == Tool::Pen {
                    (width, color)
                } else {
                    // design: a mark's own color, part of the file and not the app.
                    (width * 4., hsla([ink[0], ink[1], ink[2], 0.4]))
                };
                stroke(&points, width, color, window);
            }
        }
        Tool::Arrow => {
            stroke(&[on_screen(first), on_screen(last)], width, color, window);
        }
        Tool::Ellipse => {
            let (cx_, cy_) = (area.x + area.width / 2., area.y + area.height / 2.);
            let points: Vec<Point<Pixels>> = (0..=48)
                .map(|n| {
                    let angle = f64::from(n) / 48. * std::f64::consts::TAU;
                    on_screen(&[
                        cx_ + area.width / 2. * angle.cos(),
                        cy_ + area.height / 2. * angle.sin(),
                    ])
                })
                .collect();
            stroke(&points, width, color, window);
        }
        Tool::Rectangle | Tool::Crop | Tool::Text => {
            let color = if tool == Tool::Crop {
                theme::accent()
            } else {
                color
            };
            window.paint_quad(gpui::outline(frame, color, gpui::BorderStyle::Solid));
        }
        Tool::Redact => {
            // design: a mark's own color, part of the file and not the app.
            window.paint_quad(gpui::fill(frame, hsla([0., 0., 0., 1.])));
        }
    }
}

/// The selected mark's frame and handles.
pub(super) fn paint_handles(
    step: &Step,
    picture: (f64, f64),
    origin: Point<Pixels>,
    scale: f32,
    window: &mut Window,
) {
    let on_screen = |at: [f64; 2]| {
        point(
            origin.x + px(at[0] as f32 * scale),
            origin.y + px(at[1] as f32 * scale),
        )
    };
    let handles = step.handles(picture, f64::from(TURN_HANDLE_OUT / scale));
    let line = |points: &[Point<Pixels>], window: &mut Window| {
        let mut path = PathBuilder::stroke(px(1.));
        path.add_polygon(points, false);
        if let Ok(path) = path.build() {
            window.paint_path(path, theme::accent());
        }
    };
    if let Some(frame) = step.frame(picture).filter(|_| handles.len() > 2) {
        let mut outline: Vec<_> = (0..4).map(|at| on_screen(frame.corner(at))).collect();
        outline.push(outline[0]);
        line(&outline, window);
        line(
            &[
                on_screen(frame.above_top(0.)),
                on_screen(frame.above_top(f64::from(TURN_HANDLE_OUT / scale))),
            ],
            window,
        );
    }
    for (_, at) in handles {
        let side = px(10.);
        let center = on_screen(at);
        window.paint_quad(
            gpui::fill(
                Bounds::new(center - point(side / 2., side / 2.), size(side, side)),
                theme::accent(),
            )
            .corner_radii(side / 2.)
            .border_widths(px(1.))
            .border_color(content(1.)),
        );
    }
}
