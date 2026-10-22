//! The export dialog: which rows, which format and its options, and where
//! to, with a preview of the output for the first few rows.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, WeakEntity, Window, div, px,
};
use qwerio_core::export::{CsvOptions, CsvSeparator, ExportFormat, ExportTarget, export_string};
use qwerio_core::result::{ColumnDescriptor, Row};

use super::{ExportDestination, ExportScope, rows_label};
use crate::table_view::TableView;
use crate::theme::glass;

const DIALOG_WIDTH: f32 = 640.;
const PREVIEW_HEIGHT: f32 = 220.;

/// Rows the preview shows at most.
pub(super) const PREVIEW_ROWS: usize = 10;

/// Characters the preview shows at most, so long values don't slow it down.
const PREVIEW_MAX_CHARS: usize = 4_000;

/// The formats offered for exporting; TSV stays the quick copy format. The
/// CSV entry stands for CSV with whatever options are chosen.
const FORMATS: [ExportFormat; 3] = [
    ExportFormat::Csv(CsvOptions {
        separator: CsvSeparator::Comma,
        include_header: true,
    }),
    ExportFormat::Json,
    ExportFormat::SqlInsert,
];

/// What the preview is built from: the first rows of each scope.
pub(super) struct PreviewSource {
    pub(super) target: ExportTarget,
    pub(super) columns: Vec<ColumnDescriptor>,
    /// The first selected rows, up to [`PREVIEW_ROWS`].
    pub(super) selected_rows: Vec<Row>,
    /// The first loaded rows, up to [`PREVIEW_ROWS`].
    pub(super) first_rows: Vec<Row>,
}

pub(super) fn open(
    table: WeakEntity<TableView>,
    selected: usize,
    preview: PreviewSource,
    window: &mut Window,
    cx: &mut App,
) {
    let scope = if selected > 0 {
        ExportScope::Selected
    } else {
        ExportScope::All
    };
    let options = cx.new(|_| ExportOptions {
        scope,
        format: FORMATS[0],
        csv: CsvOptions::default(),
        selected,
        preview,
    });

    glass::open_dialog(window, cx, move |dialog, _, _| {
        let start = |destination: ExportDestination| {
            let (table, options) = (table.clone(), options.clone());
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let options = options.read(cx);
                let (scope, format) = (options.scope, options.chosen_format());
                window.close_dialog(cx);
                table
                    .update(cx, |table, cx| {
                        table.export(scope, format, destination, window, cx)
                    })
                    .ok();
            }
        };

        dialog
            .title("Export Rows")
            .w(px(DIALOG_WIDTH))
            .child(options.clone())
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    // DialogClose fills its parent, so the wrapper keeps the
                    // button at its own width.
                    .child(
                        div().flex_none().child(
                            DialogClose::new()
                                .child(Button::new("export-close").outline().label("Cancel")),
                        ),
                    )
                    .child(
                        Button::new("export-copy")
                            .success()
                            .label("Copy to Clipboard")
                            .on_click(start(ExportDestination::Clipboard)),
                    )
                    .child(
                        Button::new("export-save")
                            .primary()
                            .label("Save to File…")
                            .on_click(start(ExportDestination::File)),
                    ),
            )
    });
}

/// The rows and format chosen in the export dialog.
struct ExportOptions {
    scope: ExportScope,
    /// One of [`FORMATS`].
    format: ExportFormat,
    /// Applied when CSV is chosen; kept while another format is tried.
    csv: CsvOptions,
    /// How many rows are selected in the table.
    selected: usize,
    preview: PreviewSource,
}

impl ExportOptions {
    fn chosen_format(&self) -> ExportFormat {
        match self.format {
            ExportFormat::Csv(_) => ExportFormat::Csv(self.csv),
            other => other,
        }
    }

    fn preview_rows(&self) -> &[Row] {
        match self.scope {
            ExportScope::Selected => &self.preview.selected_rows,
            ExportScope::All => &self.preview.first_rows,
        }
    }

    fn render_csv_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let separators = h_flex()
            .gap_2()
            .children(CsvSeparator::ALL.map(|separator| {
                Button::new(SharedString::from(format!(
                    "export-separator-{}",
                    separator.display_name().to_lowercase()
                )))
                .outline()
                .small()
                .label(separator.display_name())
                .selected(self.csv.separator == separator)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.csv.separator = separator;
                    cx.notify();
                }))
            }));

        let header = Checkbox::new("export-include-header")
            .label("Include header row")
            .checked(self.csv.include_header)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.csv.include_header = *checked;
                cx.notify();
            }));

        h_flex()
            .gap_6()
            .items_end()
            .child(option_group("Separator", separators, cx))
            .child(header)
    }

    fn render_preview(&self, cx: &App) -> impl IntoElement {
        let rows = self.preview_rows();
        let text: SharedString = preview_text(
            self.chosen_format(),
            &self.preview.target,
            &self.preview.columns,
            rows,
        )
        .into();
        let caption = match self.scope {
            ExportScope::Selected if self.selected > rows.len() => {
                format!(
                    "Preview of the first {} of {} selected rows",
                    rows.len(),
                    self.selected
                )
            }
            ExportScope::Selected => "Preview of the selected rows".to_owned(),
            ExportScope::All => format!("Preview of the first {}", rows_label(rows.len() as u64)),
        };

        let output = div()
            .id("export-preview")
            .h(px(PREVIEW_HEIGHT))
            .overflow_y_scroll()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .text_xs()
            .font_family(cx.theme().mono_font_family.clone())
            .aria_label(text.clone())
            .child(text)
            .test_support();

        option_group(caption, output, cx)
    }
}

/// The export of `rows`, cut short when long.
fn preview_text(
    format: ExportFormat,
    target: &ExportTarget,
    columns: &[ColumnDescriptor],
    rows: &[Row],
) -> String {
    let mut text = export_string(format, target, columns, rows);

    if let Some((cut, _)) = text.char_indices().nth(PREVIEW_MAX_CHARS) {
        text.truncate(cut);
        text.push('…');
    }

    text
}

impl Render for ExportOptions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scope_button = |id: &'static str, label: String, scope: ExportScope| {
            Button::new(id)
                .outline()
                .small()
                .label(label)
                .selected(self.scope == scope)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.scope = scope;
                    cx.notify();
                }))
        };
        let selected_label = format!("Selected ({})", rows_label(self.selected as u64));
        let scopes = h_flex()
            .gap_2()
            .child(
                scope_button(
                    "export-scope-selected",
                    selected_label,
                    ExportScope::Selected,
                )
                .disabled(self.selected == 0),
            )
            .child(scope_button(
                "export-scope-all",
                "All Matching Rows".to_owned(),
                ExportScope::All,
            ));

        let formats = h_flex().gap_2().children(FORMATS.map(|format| {
            Button::new(SharedString::from(format!(
                "export-format-{}",
                format.extension()
            )))
            .outline()
            .small()
            .label(format.display_name())
            .selected(self.format == format)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.format = format;
                cx.notify();
            }))
        }));

        v_flex()
            .gap_3()
            .child(option_group("Rows", scopes, cx))
            .child(option_group("Format", formats, cx))
            .when(self.scope == ExportScope::All, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Every row matching the current filters and sort, not only the loaded rows."),
                )
            })
            .when(matches!(self.format, ExportFormat::Csv(_)), |this| {
                this.child(self.render_csv_options(cx))
            })
            .child(self.render_preview(cx))
    }
}

fn option_group(
    label: impl Into<SharedString>,
    options: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        .child(options)
}
