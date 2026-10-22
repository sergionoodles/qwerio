//! Grid presentation shared by table browsing and query results: cell
//! previews with distinct NULL rendering, column sizing, and the delegate for
//! console result sets.

use std::sync::Arc;

use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Role, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, TextRun,
    Window, div, font, px,
};
use qwerio_core::display::{CellStyle, DEFAULT_CELL_PREVIEW_CHARS, preview_cell};
use qwerio_core::result::{ColumnDescriptor, Row};
use qwerio_core::value::Value;

/// Column widths are derived from the header text, within these bounds (in
/// logical pixels; the grid's resize handles adjust them).
const MIN_COLUMN_WIDTH: f32 = 72.;
const MAX_COLUMN_WIDTH: f32 = 320.;
const COLUMN_WIDTH_PER_CHARACTER: f32 = 8.;
const COLUMN_WIDTH_PADDING: f32 = 48.;

/// Grid text is drawn at the table's default size, `text_sm`.
const GRID_FONT_SIZE_REMS: f32 = 0.875;
/// A sortable header also holds the sort button: a 0.75rem icon with 2 px of
/// padding on each side.
const SORT_ICON_SIZE_REMS: f32 = 0.75;
const SORT_ICON_PADDING: Pixels = px(4.);

pub fn column_width(name: &str, type_name: &str) -> f32 {
    let characters = name.chars().count().max(type_name.chars().count().min(12)) as f32;
    (characters * COLUMN_WIDTH_PER_CHARACTER + COLUMN_WIDTH_PADDING)
        .clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH)
}

/// A grid column keyed by position, because result sets may repeat names.
pub fn grid_column(index: usize, descriptor: &ColumnDescriptor, sortable: bool) -> Column {
    let column = Column::new(format!("column-{index}"), descriptor.name.clone())
        .width(px(column_width(&descriptor.name, &descriptor.type_name)));

    if sortable { column.sortable() } else { column }
}

/// Renders one cell preview, labelled with its text for assistive
/// technology. Previews are display-only; copying uses the canonical value.
pub fn render_cell(row_ix: usize, col_ix: usize, value: &Value, cx: &App) -> AnyElement {
    let preview = preview_cell(value, DEFAULT_CELL_PREVIEW_CHARS);
    let cell = div()
        .id(SharedString::from(format!("cell-{row_ix}-{col_ix}")))
        .w_full()
        .truncate()
        .role(Role::Cell)
        .aria_label(preview.text.clone());

    let cell = match preview.style {
        CellStyle::Null => cell.text_color(cx.theme().muted_foreground).italic(),
        CellStyle::Number => cell
            .text_right()
            .font_family(cx.theme().mono_font_family.clone()),
        CellStyle::Binary | CellStyle::Unsupported => cell
            .text_color(cx.theme().muted_foreground)
            .font_family(cx.theme().mono_font_family.clone()),
        CellStyle::Boolean | CellStyle::Text => cell,
    };

    cell.child(preview.text).test_support().into_any_element()
}

/// One cell's text as the grid draws it, and the width drawn beside it.
pub struct DrawnCell {
    pub text: SharedString,
    /// Picks the font, as [`render_cell`] does.
    pub style: CellStyle,
    /// Width beside the text, such as a checkbox or a tint's padding.
    pub extra: Pixels,
}

impl DrawnCell {
    pub fn preview(value: &Value) -> Self {
        let preview = preview_cell(value, DEFAULT_CELL_PREVIEW_CHARS);

        Self {
            text: preview.text.into(),
            style: preview.style,
            extra: Pixels::ZERO,
        }
    }
}

/// The width a column needs to show its header and every cell whole, before
/// the table adds its cell padding.
pub fn fit_width(
    header: &str,
    is_sortable: bool,
    cells: impl IntoIterator<Item = DrawnCell>,
    window: &Window,
    cx: &App,
) -> Pixels {
    let rem_size = window.rem_size();
    let sort_icon = if is_sortable {
        rem_size * SORT_ICON_SIZE_REMS + SORT_ICON_PADDING
    } else {
        Pixels::ZERO
    };
    let header = DrawnCell {
        text: header.to_owned().into(),
        style: CellStyle::Text,
        extra: sort_icon,
    };

    std::iter::once(header)
        .chain(cells)
        .map(|cell| text_width(&cell.text, cell.style, window, cx) + cell.extra)
        .fold(Pixels::ZERO, Pixels::max)
}

fn text_width(text: &SharedString, style: CellStyle, window: &Window, cx: &App) -> Pixels {
    let theme = cx.theme();
    let family = match style {
        CellStyle::Number | CellStyle::Binary | CellStyle::Unsupported => {
            theme.mono_font_family.clone()
        }
        CellStyle::Null | CellStyle::Boolean | CellStyle::Text => theme.font_family.clone(),
    };
    let cell_font = if style == CellStyle::Null {
        font(family).italic()
    } else {
        font(family)
    };
    // A shaped line holds no line breaks; the grid draws them as spaces.
    let line: SharedString = text.replace(['\r', '\n'], " ").into();
    let run = TextRun {
        len: line.len(),
        font: cell_font,
        color: theme.foreground,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let font_size = window.rem_size() * GRID_FONT_SIZE_REMS;

    window
        .text_system()
        .shape_line(line, font_size, &[run], None)
        .width
}

/// The text copied for a cell: the full canonical value, or empty for NULL.
pub fn copy_text(value: &Value) -> String {
    value.canonical_text().unwrap_or_default()
}

/// Rows and columns of one console result set.
pub struct ResultGridDelegate {
    columns: Arc<[ColumnDescriptor]>,
    rows: Vec<Row>,
    empty_message: SharedString,
}

impl ResultGridDelegate {
    pub fn new() -> Self {
        Self {
            columns: Arc::from(Vec::new()),
            rows: Vec::new(),
            empty_message: "Run a statement to see results".into(),
        }
    }

    pub fn set_result(&mut self, columns: Arc<[ColumnDescriptor]>, rows: Vec<Row>) {
        self.columns = columns;
        self.rows = rows;
    }

    pub fn set_empty_message(&mut self, message: impl Into<SharedString>) {
        self.empty_message = message.into();
    }

    pub fn columns(&self) -> &Arc<[ColumnDescriptor]> {
        &self.columns
    }
}

impl TableDelegate for ResultGridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        grid_column(col_ix, &self.columns[col_ix], false)
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        match self.rows.get(row_ix).and_then(|row| row.get(col_ix)) {
            Some(value) => render_cell(row_ix, col_ix, value, cx),
            None => div().into_any_element(),
        }
    }

    fn fit_column(
        &mut self,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<Pixels> {
        let header = &self.columns.get(col_ix)?.name;
        let cells = self
            .rows
            .iter()
            .filter_map(|row| row.get(col_ix))
            .map(DrawnCell::preview);

        Some(fit_width(header, false, cells, window, cx))
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(self.empty_message.clone())
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.rows
            .get(row_ix)
            .and_then(|row| row.get(col_ix))
            .map(copy_text)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_widths_stay_within_bounds() {
        assert_eq!(column_width("id", "integer"), 104.);
        assert_eq!(column_width(&"x".repeat(200), "text"), MAX_COLUMN_WIDTH);
        assert_eq!(column_width("", ""), MIN_COLUMN_WIDTH);
    }

    #[test]
    fn copying_null_yields_empty_text_but_values_stay_whole() {
        assert_eq!(copy_text(&Value::Null), "");
        assert_eq!(copy_text(&Value::Text("x".repeat(500))).len(), 500);
    }
}
