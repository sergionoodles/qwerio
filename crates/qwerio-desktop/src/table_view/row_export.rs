//! Exporting rows as CSV, JSON, or SQL INSERT statements to the clipboard or
//! a file: the selected rows straight from memory, or every row matching the
//! filters paged from the database, with a progress strip and Cancel.

use std::future::Future;
use std::io::{BufWriter, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};
use qwerio_app::export::{
    ExportProgress, ExportSummary, MAX_CLIPBOARD_EXPORT_ROWS, MAX_FILE_EXPORT_ROWS, TableExport,
    export_table,
};
use qwerio_app::row_selection::SelectionGesture;
use qwerio_core::driver::DatabaseSession;
use qwerio_core::error::{DatabaseError, ErrorKind};
use qwerio_core::export::{ExportFormat, ExportTarget, export_string};
use qwerio_core::paging::PageRequest;

use super::{TableGridDelegate, TableView};

mod dialog;

const PROGRESS_BAR_WIDTH: f32 = 160.;
/// How often the progress strip shows the latest row count.
const PROGRESS_REFRESH: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportScope {
    /// The selected rows, already loaded.
    Selected,
    /// Every row matching the filters, paged from the database.
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportDestination {
    Clipboard,
    File,
}

/// A running export of every matching row. Dropping it, as closing the
/// table does, cancels the export.
pub(super) struct ExportJob {
    progress: Arc<ExportProgress>,
    format: ExportFormat,
    destination: ExportDestination,
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        self.progress.cancel();
    }
}

/// Where a finished full export went.
enum ExportOutput {
    Clipboard(String),
    File(PathBuf),
}

struct FinishedExport {
    summary: ExportSummary,
    output: ExportOutput,
}

impl TableGridDelegate {
    pub(super) fn export_target(&self) -> Option<ExportTarget> {
        let session = self.session.as_ref()?;
        let details = self.browser.details()?;

        Some(ExportTarget::new(session.info().engine, details))
    }
}

impl TableView {
    pub(super) fn is_exporting(&self) -> bool {
        self.export_job.is_some()
    }

    /// Opens the export dialog, offering the selected rows when there are any.
    pub(super) fn open_export_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_exporting() {
            return;
        }

        let delegate = self.grid.read(cx).delegate();
        let Some(target) = delegate.export_target() else {
            return;
        };
        let browser = &delegate.browser;
        let selected = browser.picked_row_count();
        let preview = dialog::PreviewSource {
            target,
            columns: browser.columns().to_vec(),
            selected_rows: browser
                .picked_rows()
                .into_iter()
                .take(dialog::PREVIEW_ROWS)
                .collect(),
            first_rows: browser
                .rows()
                .iter()
                .take(dialog::PREVIEW_ROWS)
                .cloned()
                .collect(),
        };

        let table = cx.entity().downgrade();
        dialog::open(table, selected, preview, window, cx);
    }

    /// Exports the right-clicked row, or all selected rows when it is one of
    /// them.
    pub(super) fn export_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| {
            let delegate = grid.delegate_mut();
            let loaded = delegate.loaded_row(row);

            if loaded.is_some_and(|index| !delegate.browser.is_row_picked(index)) {
                delegate.browser.pick_row(loaded, SelectionGesture::Replace);
                cx.notify();
            }
        });
        self.open_export_dialog(window, cx);
    }

    fn export(
        &mut self,
        scope: ExportScope,
        format: ExportFormat,
        destination: ExportDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match scope {
            ExportScope::Selected => self.export_selected(format, destination, window, cx),
            ExportScope::All => self.export_all(format, destination, window, cx),
        }
    }

    fn export_selected(
        &mut self,
        format: ExportFormat,
        destination: ExportDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delegate = self.grid.read(cx).delegate();
        let Some(target) = delegate.export_target() else {
            return;
        };
        let rows = delegate.browser.picked_rows();
        let text = export_string(format, &target, delegate.browser.columns(), &rows);
        let rows = rows_label(rows.len() as u64);

        if destination == ExportDestination::Clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            let message = format!("Copied {rows} as {}", format.display_name());
            window.push_notification(Notification::success(message), cx);
            return;
        }

        let path = self.prompt_for_export_path(format, cx);

        cx.spawn_in(window, async move |_, cx| {
            let Some(path) = path.await else {
                return;
            };

            let written = cx
                .background_spawn({
                    let path = path.clone();
                    async move { std::fs::write(&path, text) }
                })
                .await;

            cx.update(|window, cx| {
                let notification = match written {
                    Ok(()) => {
                        Notification::success(format!("Exported {rows} to {}", path.display()))
                    }
                    Err(error) => Notification::error(format!("Export failed: {error}")),
                };
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
    }

    fn export_all(
        &mut self,
        format: ExportFormat,
        destination: ExportDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delegate = self.grid.read(cx).delegate();
        let (Some(session), Some(details), Some(target)) = (
            delegate.session.clone(),
            delegate.browser.details().cloned(),
            delegate.export_target(),
        ) else {
            return;
        };

        let view = delegate.browser.view();
        let export = TableExport {
            request: PageRequest {
                filters: view.filters.clone(),
                sort: view.sort.clone(),
                ..PageRequest::first(details)
            },
            format,
            target,
            max_rows: match destination {
                ExportDestination::Clipboard => MAX_CLIPBOARD_EXPORT_ROWS,
                ExportDestination::File => MAX_FILE_EXPORT_ROWS,
            },
        };
        let path = match destination {
            ExportDestination::File => Some(self.prompt_for_export_path(format, cx)),
            ExportDestination::Clipboard => None,
        };

        cx.spawn_in(window, async move |this, cx| {
            let path = match path {
                Some(prompt) => match prompt.await {
                    Some(path) => Some(path),
                    None => return,
                },
                None => None,
            };

            let progress = this
                .update(cx, |this, cx| {
                    this.begin_export_job(format, destination, cx)
                })
                .ok()
                .flatten();
            let Some(progress) = progress else {
                return;
            };

            let result = cx
                .background_spawn(run_full_export(session, export, path, progress))
                .await;

            this.update_in(cx, |this, window, cx| {
                this.finish_export_job(result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Starts tracking a full export, unless one is already running.
    fn begin_export_job(
        &mut self,
        format: ExportFormat,
        destination: ExportDestination,
        cx: &mut Context<Self>,
    ) -> Option<Arc<ExportProgress>> {
        if self.is_exporting() {
            return None;
        }

        let progress = Arc::new(ExportProgress::default());
        self.export_job = Some(ExportJob {
            progress: Arc::clone(&progress),
            format,
            destination,
        });

        // Row counts change on a background thread; redraw while it runs.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_REFRESH).await;
                let is_running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.is_exporting()
                    })
                    .unwrap_or(false);

                if !is_running {
                    break;
                }
            }
        })
        .detach();
        cx.notify();

        Some(progress)
    }

    fn finish_export_job(
        &mut self,
        result: Result<FinishedExport, DatabaseError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = self.export_job.take() else {
            return;
        };
        cx.notify();

        let finished = match result {
            Ok(finished) => finished,
            Err(error) if error.kind == ErrorKind::Cancelled => {
                window.push_notification(Notification::info("Export cancelled"), cx);
                return;
            }
            Err(error) => {
                let message = format!("Export failed: {}", error.message);
                window.push_notification(Notification::error(message), cx);
                return;
            }
        };

        let rows = rows_label(finished.summary.rows);
        let message = match finished.output {
            ExportOutput::Clipboard(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                format!("Copied {rows} as {}", job.format.display_name())
            }
            ExportOutput::File(path) => format!("Exported {rows} to {}", path.display()),
        };

        let notification = if finished.summary.hit_limit {
            Notification::warning(format!(
                "{message}; stopped at the export limit, refine the filters to export the rest"
            ))
        } else {
            Notification::success(message)
        };
        window.push_notification(notification, cx);
    }

    fn cancel_export(&mut self) {
        if let Some(job) = &self.export_job {
            job.progress.cancel();
        }
    }

    fn prompt_for_export_path(
        &self,
        format: ExportFormat,
        cx: &mut Context<Self>,
    ) -> impl Future<Output = Option<PathBuf>> + use<> {
        let directory = self.model.read(cx).directory();
        let file_name = format!(
            "{}.{}",
            qwerio_app::templates::file_stem(&self.object.name),
            format.extension()
        );
        let prompt = cx.prompt_for_new_path(&directory, Some(&file_name));

        async move { prompt.await.ok()?.ok()? }
    }

    /// The strip under the toolbar while every row is being exported.
    pub(super) fn render_export_progress(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let job = self.export_job.as_ref()?;
        let destination = match job.destination {
            ExportDestination::Clipboard => "for the clipboard",
            ExportDestination::File => "to a file",
        };
        let status: SharedString = format!(
            "Exporting {} {destination}… {} so far",
            job.format.display_name(),
            rows_label(job.progress.rows())
        )
        .into();

        Some(
            h_flex()
                .px_2()
                .py_1()
                .gap_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    Progress::new("export-progress")
                        .loading(true)
                        .w(px(PROGRESS_BAR_WIDTH)),
                )
                .child(
                    div()
                        .id("export-status")
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .role(Role::Status)
                        .aria_label(status.clone())
                        .child(status)
                        .test_support(),
                )
                .child(
                    Button::new("export-cancel")
                        .ghost()
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, _| this.cancel_export())),
                ),
        )
    }
}

/// Runs a full export on a background thread. A file that was not written
/// completely is removed.
async fn run_full_export(
    session: Arc<dyn DatabaseSession>,
    export: TableExport,
    path: Option<PathBuf>,
    progress: Arc<ExportProgress>,
) -> Result<FinishedExport, DatabaseError> {
    let Some(path) = path else {
        let mut buffer = Vec::new();
        let summary = export_table(session.as_ref(), export, &mut buffer, &progress).await?;
        let text = String::from_utf8_lossy(&buffer).into_owned();

        return Ok(FinishedExport {
            summary,
            output: ExportOutput::Clipboard(text),
        });
    };

    let file = std::fs::File::create(&path).map_err(file_error)?;
    let mut writer = BufWriter::new(file);
    let written = match export_table(session.as_ref(), export, &mut writer, &progress).await {
        Ok(summary) => writer.flush().map(|()| summary).map_err(file_error),
        Err(error) => Err(error),
    };

    match written {
        Ok(summary) => Ok(FinishedExport {
            summary,
            output: ExportOutput::File(path),
        }),
        Err(error) => Err(remove_partial_file(error, path)),
    }
}

fn remove_partial_file(error: DatabaseError, path: PathBuf) -> DatabaseError {
    match std::fs::remove_file(&path) {
        Ok(()) => error,
        Err(removal) => DatabaseError::new(
            error.kind,
            format!(
                "{}; the incomplete file {} could not be removed: {removal}",
                error.message,
                path.display()
            ),
        ),
    }
}

fn file_error(error: std::io::Error) -> DatabaseError {
    DatabaseError::new(ErrorKind::Other, error.to_string())
}

fn rows_label(rows: u64) -> String {
    if rows == 1 {
        "1 row".to_owned()
    } else {
        format!("{rows} rows")
    }
}
