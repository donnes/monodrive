// A text field, single-line or a fixed number of rows, cut down from Monocode's input.rs, which adapts
// GPUI's Apache-2.0 examples/input.rs (Copyright Zed Industries). It keeps
// Monocode's IME handling, word movement and caret painting, and drops the
// composer and code editor parts.

use crate::theme::content;
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, InspectorElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Point, Role, ScrollWheelEvent, ShapedLine, SharedString, Style,
    TextAlign, TextRun, TextStyle, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions,
    div, fill, point, prelude::*, px, relative, size,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

const CONTEXT: &str = "TextField";
const AREA_CONTEXT: &str = "TextArea";
const FONT_SIZE: f32 = 14.;

actions!(
    text_field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteToHome,
        Paste,
        Copy,
        Cut,
        Up,
        Down,
        SelectUp,
        SelectDown,
        Newline,
    ]
);

pub fn bind(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("cmd-left", Home, context),
        KeyBinding::new("cmd-right", End, context),
        KeyBinding::new("cmd-shift-left", SelectHome, context),
        KeyBinding::new("cmd-shift-right", SelectEnd, context),
        KeyBinding::new("alt-left", WordLeft, context),
        KeyBinding::new("alt-right", WordRight, context),
        KeyBinding::new("alt-shift-left", SelectWordLeft, context),
        KeyBinding::new("alt-shift-right", SelectWordRight, context),
        KeyBinding::new("alt-backspace", DeleteWordLeft, context),
        KeyBinding::new("cmd-backspace", DeleteToHome, context),
        KeyBinding::new("up", Up, Some(AREA_CONTEXT)),
        KeyBinding::new("down", Down, Some(AREA_CONTEXT)),
        KeyBinding::new("shift-up", SelectUp, Some(AREA_CONTEXT)),
        KeyBinding::new("shift-down", SelectDown, Some(AREA_CONTEXT)),
        KeyBinding::new("shift-enter", Newline, Some(AREA_CONTEXT)),
    ]);
}

/// Emitted whenever the text changes, including IME composition.
pub struct Changed;

pub struct TextField {
    focus: FocusHandle,
    label: SharedString,
    placeholder: SharedString,
    font_size: f32,
    content: SharedString,
    /// Byte offsets into `content`, always on char boundaries.
    selection: Range<usize>,
    /// The caret sits at `selection.start` instead of `end`.
    reversed: bool,
    /// The IME's in-progress composition.
    marked: Option<Range<usize>>,
    line: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
    selecting: bool,
    /// How many rows it shows, when it's a text area.
    rows: Option<usize>,
    /// A text area's lines as last drawn, each with the byte it starts at.
    lines: Vec<(usize, WrappedLine)>,
    scroll_y: Pixels,
    /// Scrolling follows the caret only when the caret moved, so the wheel can
    /// look away from it.
    drawn_cursor: usize,
}

impl EventEmitter<Changed> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextField {
    pub fn new(
        label: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            label: label.into(),
            placeholder: placeholder.into(),
            font_size: FONT_SIZE,
            content: SharedString::default(),
            selection: 0..0,
            reversed: false,
            marked: None,
            line: None,
            bounds: None,
            scroll_x: px(0.),
            selecting: false,
            rows: None,
            lines: Vec::new(),
            scroll_y: px(0.),
            drawn_cursor: 0,
        }
    }

    /// A text area: this many rows, wrapping, and keeping newlines.
    pub fn with_rows(mut self, rows: usize) -> Self {
        self.rows = Some(rows);
        self
    }

    /// Matches the text it edits in place, as a renamed file's name.
    pub fn with_font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = text.into();
        cx.notify();
    }

    /// Sets the text with `selected` selected, as an edit starts.
    pub fn set_text(&mut self, text: &str, selected: Range<usize>, cx: &mut Context<Self>) {
        self.content = SharedString::from(text.to_owned());
        self.selection = self.clip(selected.start)..self.clip(selected.end);
        self.reversed = false;
        self.marked = None;
        cx.emit(Changed);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.content.is_empty() {
            return;
        }
        self.content = SharedString::default();
        self.selection = 0..0;
        self.reversed = false;
        self.marked = None;
        cx.emit(Changed);
        cx.notify();
    }

    /// Types `text` over the selection, as a key press would.
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let inserted = self.splice(self.selection.clone(), text);
        self.selection = inserted.end..inserted.end;
        self.reversed = false;
        self.marked = None;
        cx.emit(Changed);
        cx.notify();
    }

    pub fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    /// Native text callbacks can refer to a previous value, so every offset
    /// is clipped to the current text and a char boundary.
    fn clip(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.content.len());
        while !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clip(offset);
        self.selection = offset..offset;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clip(offset);
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.start > self.selection.end {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        cx.notify();
    }

    fn previous_grapheme(&self, at: usize) -> usize {
        self.content[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(ix, _)| ix)
    }

    fn next_grapheme(&self, at: usize) -> usize {
        self.content[at..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.content.len(), |(ix, _)| at + ix)
    }

    fn previous_word(&self) -> usize {
        let cursor = self.cursor();
        self.content
            .unicode_word_indices()
            .rev()
            .find_map(|(start, _)| (start < cursor).then_some(start))
            .unwrap_or(0)
    }

    fn next_word(&self) -> usize {
        let cursor = self.cursor();
        self.content
            .unicode_word_indices()
            .map(|(start, word)| start + word.len())
            .find(|end| *end > cursor)
            .unwrap_or(self.content.len())
    }

    fn to16(&self, offset: usize) -> usize {
        self.content[..self.clip(offset)].encode_utf16().count()
    }

    fn from16(&self, offset: usize) -> usize {
        from16(&self.content, offset)
    }

    fn range16(&self, range: &Range<usize>) -> Range<usize> {
        self.to16(range.start)..self.to16(range.end)
    }

    fn range8(&self, range: Range<usize>) -> Range<usize> {
        let (start, end) = (self.from16(range.start), self.from16(range.end));
        start.min(end)..start.max(end)
    }

    fn mouse_index(&self, position: Point<Pixels>) -> usize {
        if self.rows.is_some() {
            return self.bounds.map_or(0, |bounds| {
                self.index_at(position - bounds.origin + point(px(0.), self.scroll_y))
            });
        }
        match (&self.line, self.bounds) {
            (Some(line), Some(bounds)) if !self.content.is_empty() => {
                self.clip(line.closest_index_for_x(position.x - bounds.left() + self.scroll_x))
            }
            _ => 0,
        }
    }

    /// The offset nearest a point in a text area's whole text.
    fn index_at(&self, at: Point<Pixels>) -> usize {
        if self.content.is_empty() || at.y < px(0.) {
            return 0;
        }
        let height = px(line_height(self.font_size));
        let mut top = px(0.);
        for (start, line) in &self.lines {
            let bottom = top + line.size(height).height;
            if at.y < bottom {
                let (Ok(index) | Err(index)) =
                    line.closest_index_for_position(point(at.x, at.y - top), height);
                return self.clip(start + index);
            }
            top = bottom;
        }
        self.content.len()
    }

    /// The offset `rows` rows from the caret, at the same x. Past either end
    /// it's that end, as in any Mac text view.
    fn rows_away(&self, rows: f32) -> usize {
        let height = px(line_height(self.font_size));
        let caret = position_in(&self.lines, self.cursor(), height);
        self.index_at(point(caret.x, caret.y + height * (rows + 0.5)))
    }

    fn line_start(&self) -> usize {
        if self.rows.is_none() {
            return 0;
        }
        let height = px(line_height(self.font_size));
        let caret = position_in(&self.lines, self.cursor(), height);
        self.index_at(point(px(0.), caret.y + height * 0.5))
    }

    fn line_end(&self) -> usize {
        if self.rows.is_none() {
            return self.content.len();
        }
        let height = px(line_height(self.font_size));
        let caret = position_in(&self.lines, self.cursor(), height);
        self.index_at(point(px(1e6), caret.y + height * 0.5))
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.rows_away(-1.), cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.rows_away(1.), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.rows_away(-1.), cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.rows_away(1.), cx);
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.insert("\n", cx);
    }

    fn scrolled(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let height = px(line_height(self.font_size));
        self.scroll_y -= event.delta.pixel_delta(height).y;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let to = if self.selection.is_empty() {
            self.previous_grapheme(self.cursor())
        } else {
            self.selection.start
        };
        self.move_to(to, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let to = if self.selection.is_empty() {
            self.next_grapheme(self.cursor())
        } else {
            self.selection.end
        };
        self.move_to(to, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_grapheme(self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_grapheme(self.cursor()), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end(), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start(), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end(), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word(), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.select_to(self.previous_grapheme(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.select_to(self.next_grapheme(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selection.is_empty() {
            self.select_to(self.previous_word(), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_home(&mut self, _: &DeleteToHome, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.select_to(self.line_start(), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selection.clone()].into(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        self.replace_text_in_range(None, "", window, cx);
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.selecting = true;
        let ix = self.mouse_index(event.position);
        if event.modifiers.shift {
            self.select_to(ix, cx);
        } else if event.click_count >= 3 {
            self.move_to(0, cx);
            self.select_to(self.content.len(), cx);
        } else if event.click_count == 2 {
            let word = self
                .content
                .split_word_bound_indices()
                .find(|(start, word)| ix >= *start && ix < start + word.len())
                .map_or(ix..ix, |(start, word)| start..start + word.len());
            self.move_to(word.start, cx);
            self.select_to(word.end, cx);
        } else {
            self.move_to(ix, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            self.select_to(self.mouse_index(event.position), cx);
        }
    }

    fn splice(&mut self, range: Range<usize>, text: &str) -> Range<usize> {
        // A single line turns pasted newlines into spaces.
        let newline = if self.rows.is_some() { "\n" } else { " " };
        let text = text.replace("\r\n", newline).replace(['\r', '\n'], newline);
        self.content = format!(
            "{}{text}{}",
            &self.content[..range.start],
            &self.content[range.end..]
        )
        .into();
        range.start..range.start + text.len()
    }

    /// The text's runs, or the placeholder's: the IME composition is
    /// underlined, as native fields draw it.
    fn runs(&self, text: &str, style: &TextStyle) -> Vec<TextRun> {
        let empty = self.content.is_empty();
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color: if empty { content(0.65) } else { style.color },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        match self.marked.as_ref().filter(|_| !empty) {
            Some(marked) => [0..marked.start, marked.clone(), marked.end..text.len()]
                .into_iter()
                .filter(|range| !range.is_empty())
                .map(|range| TextRun {
                    len: range.len(),
                    underline: (range == *marked).then_some(UnderlineStyle {
                        color: Some(style.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                })
                .collect(),
            None => vec![run],
        }
    }
}

/// Where an offset sits in a text area's whole text. An offset where a line
/// wraps counts as the end of the upper row.
fn position_in(lines: &[(usize, WrappedLine)], offset: usize, height: Pixels) -> Point<Pixels> {
    let mut top = px(0.);
    for (start, line) in lines {
        if offset <= start + line.len() {
            let inside = line
                .position_for_index(offset - start, height)
                .unwrap_or_default();
            return point(inside.x, top + inside.y);
        }
        top += line.size(height).height;
    }
    point(px(0.), top)
}

/// 20 pt lines for the 14 pt default, as before sizes varied.
fn line_height(font_size: f32) -> f32 {
    font_size + 6.
}

fn from16(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units + ch.len_utf16() > offset {
            return byte;
        }
        units += ch.len_utf16();
    }
    text.len()
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range8(range);
        *actual = Some(self.range16(&range));
        Some(self.content[range].into())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range16(&self.selection),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|r| self.range8(r))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let inserted = self.splice(range, text);
        self.selection = inserted.end..inserted.end;
        self.reversed = false;
        self.marked = None;
        cx.emit(Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|r| self.range8(r))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let inserted = self.splice(range, text);
        let marked = &self.content[inserted.clone()];
        self.selection = match selection {
            Some(s) => {
                inserted.start + from16(marked, s.start)..inserted.start + from16(marked, s.end)
            }
            None => inserted.end..inserted.end,
        };
        self.marked = (!inserted.is_empty()).then_some(inserted);
        self.reversed = false;
        cx.emit(Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let start = self.range8(range).start;
        if self.rows.is_some() {
            let bounds = self.bounds?;
            let height = px(line_height(self.font_size));
            let at = position_in(&self.lines, start, height);
            return Some(Bounds::new(
                bounds.origin + point(at.x, at.y - self.scroll_y),
                size(px(2.), height),
            ));
        }
        let (line, bounds) = (self.line.as_ref()?, self.bounds?);
        let x = bounds.left() + line.x_for_index(start) - self.scroll_x;
        Some(Bounds::new(
            point(x, bounds.top()),
            size(px(2.), bounds.size.height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.to16(self.mouse_index(point)))
    }
}

struct FieldElement {
    field: Entity<TextField>,
}

struct Prepaint {
    line: ShapedLine,
    scroll_x: Pixels,
}

impl IntoElement for FieldElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for FieldElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(line_height(self.field.read(cx).font_size)).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let field = self.field.read(cx);
        let style = window.text_style();
        let empty = field.content.is_empty();
        let text = if empty {
            field.placeholder.clone()
        } else {
            field.content.clone()
        };
        let runs = field.runs(&text, &style);
        let line = window
            .text_system()
            .shape_line(text, px(field.font_size), &runs, None);
        // Keep the caret in view as the text outgrows the field.
        let caret = if empty {
            px(0.)
        } else {
            line.x_for_index(field.cursor())
        };
        let scroll_x = if caret < field.scroll_x {
            caret
        } else if caret + px(2.) > field.scroll_x + bounds.size.width {
            caret + px(2.) - bounds.size.width
        } else {
            field.scroll_x
        };
        Prepaint { line, scroll_x }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let field = self.field.read(cx);
        let focus = field.focus.clone();
        let (selection, cursor, empty) = (
            field.selection.clone(),
            field.cursor(),
            field.content.is_empty(),
        );
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        let line_height = bounds.size.height;
        let x = bounds.left() - state.scroll_x;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if !selection.is_empty() {
                let (start, end) = (
                    state.line.x_for_index(selection.start),
                    state.line.x_for_index(selection.end),
                );
                window.paint_quad(fill(
                    Bounds::new(
                        point(x + start, bounds.top()),
                        size(end - start, line_height),
                    ),
                    crate::theme::accent().opacity(0.35),
                ));
            }
            let _ = state.line.paint(
                point(x, bounds.top()),
                line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
            if focus.is_focused(window) && selection.is_empty() {
                let caret = if empty {
                    px(0.)
                } else {
                    state.line.x_for_index(cursor)
                };
                window.paint_quad(fill(
                    Bounds::new(
                        point(x + caret, bounds.top() + px(2.)),
                        size(px(1.5), line_height - px(4.)),
                    ),
                    content(1.),
                ));
            }
        });
        let line = state.line.clone();
        let scroll_x = state.scroll_x;
        self.field.update(cx, |field, _| {
            field.line = Some(line);
            field.bounds = Some(bounds);
            field.scroll_x = scroll_x;
        });
    }
}

/// A text area's text: wrapped lines in a box of fixed height, scrolled to keep
/// the caret in view.
struct AreaElement {
    field: Entity<TextField>,
}

struct AreaPrepaint {
    lines: Vec<(usize, WrappedLine)>,
    scroll_y: Pixels,
}

impl IntoElement for AreaElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for AreaElement {
    type RequestLayoutState = ();
    type PrepaintState = AreaPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let field = self.field.read(cx);
        let rows = field.rows.unwrap_or(1) as f32;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(line_height(field.font_size) * rows).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> AreaPrepaint {
        let field = self.field.read(cx);
        let style = window.text_style();
        let text = if field.content.is_empty() {
            field.placeholder.clone()
        } else {
            field.content.clone()
        };
        let runs = field.runs(&text, &style);
        let shaped = window
            .text_system()
            .shape_text(
                text,
                px(field.font_size),
                &runs,
                Some(bounds.size.width),
                None,
            )
            .unwrap_or_default();
        let mut next = 0;
        let lines: Vec<_> = shaped
            .into_iter()
            .map(|line| {
                let start = next;
                next += line.len() + 1;
                (start, line)
            })
            .collect();
        let height = px(line_height(field.font_size));
        let total = lines
            .iter()
            .fold(px(0.), |sum, (_, line)| sum + line.size(height).height);
        let cursor = field.cursor();
        let mut scroll_y = field.scroll_y;
        if cursor != field.drawn_cursor {
            let caret = position_in(&lines, cursor, height).y;
            if caret < scroll_y {
                scroll_y = caret;
            } else if caret + height > scroll_y + bounds.size.height {
                scroll_y = caret + height - bounds.size.height;
            }
        }
        let scroll_y = scroll_y.min(total - bounds.size.height).max(px(0.));
        AreaPrepaint { lines, scroll_y }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut AreaPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let field = self.field.read(cx);
        let focus = field.focus.clone();
        let (selection, cursor, font_size) =
            (field.selection.clone(), field.cursor(), field.font_size);
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        let height = px(line_height(font_size));
        let origin = bounds.origin - point(px(0.), state.scroll_y);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let mut top = px(0.);
            for (start, line) in &state.lines {
                // Selection, one wrapped row at a time.
                for row in 0..=line.wrap_boundaries().len() {
                    let row_top = height * row;
                    let at_x = |x: Pixels| {
                        let (Ok(index) | Err(index)) = line
                            .closest_index_for_position(point(x, row_top + height * 0.5), height);
                        index
                    };
                    let (from, to) = (at_x(px(0.)), at_x(px(1e6)));
                    let selected = selection.start.saturating_sub(*start).max(from)
                        ..selection.end.saturating_sub(*start).min(to);
                    if selection.is_empty()
                        || selection.end < *start
                        || selected.start >= selected.end
                    {
                        continue;
                    }
                    let x = |index: usize| {
                        line.position_for_index(index, height)
                            .map_or(px(0.), |at| at.x)
                    };
                    // A row's first offset also ends the row above.
                    let left = if selected.start == from {
                        px(0.)
                    } else {
                        x(selected.start)
                    };
                    window.paint_quad(fill(
                        Bounds::new(
                            origin + point(left, top + row_top),
                            size(x(selected.end) - left, height),
                        ),
                        crate::theme::accent().opacity(0.35),
                    ));
                }
                let _ = line.paint(
                    origin + point(px(0.), top),
                    height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                top += line.size(height).height;
            }
            if focus.is_focused(window) && selection.is_empty() {
                let caret = position_in(&state.lines, cursor, height);
                window.paint_quad(fill(
                    Bounds::new(
                        origin + caret + point(px(0.), px(2.)),
                        size(px(1.5), height - px(4.)),
                    ),
                    content(1.),
                ));
            }
        });
        let lines = std::mem::take(&mut state.lines);
        let scroll_y = state.scroll_y;
        self.field.update(cx, |field, _| {
            field.lines = lines;
            field.bounds = Some(bounds);
            field.scroll_y = scroll_y;
            field.drawn_cursor = cursor;
        });
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-field")
            .role(Role::TextInput)
            .aria_label(self.label.clone())
            .aria_value(self.content.clone())
            .key_context(if self.rows.is_some() {
                "TextField TextArea"
            } else {
                CONTEXT
            })
            .track_focus(&self.focus)
            .w_full()
            .min_w_0()
            .text_size(px(self.font_size))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_home))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::newline))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_move(cx.listener(Self::mouse_move))
            .map(|d| {
                let field = cx.entity();
                if self.rows.is_some() {
                    d.on_scroll_wheel(cx.listener(Self::scrolled))
                        .child(AreaElement { field })
                } else {
                    d.child(FieldElement { field })
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{TextField, bind, from16};
    use gpui::{Focusable, TestAppContext};

    /// Up and Down cross hard lines, Home and End stay on one, and a pasted
    /// newline stays a newline.
    #[gpui::test]
    fn a_text_area_moves_the_caret_between_its_lines(cx: &mut TestAppContext) {
        cx.update(bind);
        let (field, cx) = cx.add_window_view(|_, cx| TextField::new("Note", "", cx).with_rows(3));
        cx.update(|window, cx| window.focus(&field.focus_handle(cx), cx));
        field.update(cx, |field, cx| field.insert("ab\r\ncd", cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("up");
        cx.simulate_input("x");
        cx.simulate_keystrokes("end");
        cx.simulate_input("y");
        cx.simulate_keystrokes("down home");
        cx.simulate_input("z");
        cx.simulate_keystrokes("shift-enter");
        field.read_with(cx, |field, _| assert_eq!(field.text(), "abxy\nz\ncd"));
        cx.simulate_keystrokes("shift-up");
        cx.simulate_input("q");
        field.read_with(cx, |field, _| assert_eq!(field.text(), "abxy\nqcd"));
    }

    /// The IME speaks UTF-16 offsets; an emoji is two units, CJK one.
    #[test]
    fn utf16_offsets_land_on_char_boundaries() {
        assert_eq!(from16("a🙂漢", 1), 1);
        assert_eq!(from16("a🙂漢", 2), 1);
        assert_eq!(from16("a🙂漢", 3), 5);
        assert_eq!(from16("a🙂漢", 9), "a🙂漢".len());
    }
}
