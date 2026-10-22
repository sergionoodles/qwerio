//! Full exports: every row matching the current filters and sort (not only
//! the loaded rows), paged from the database and streamed to a writer, with
//! progress and cancellation shared with the view that started it.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use qwerio_core::driver::DatabaseSession;
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::export::{ExportFormat, ExportTarget, Exporter};
use qwerio_core::paging::PageRequest;

/// Rows a full export to a file writes at most.
pub const MAX_FILE_EXPORT_ROWS: u64 = 1_000_000;

/// Rows a full export to the clipboard copies at most; the whole text is
/// held in memory and handed to the system clipboard at once.
pub const MAX_CLIPBOARD_EXPORT_ROWS: u64 = 100_000;

/// What to export and how.
#[derive(Clone, Debug)]
pub struct TableExport {
    /// The first page of the query to export, with its filters and sort.
    pub request: PageRequest,
    pub format: ExportFormat,
    pub target: ExportTarget,
    pub max_rows: u64,
}

/// Progress of a running export, readable from another thread, and the
/// request to stop it.
#[derive(Debug, Default)]
pub struct ExportProgress {
    rows: AtomicU64,
    is_cancelled: AtomicBool,
}

impl ExportProgress {
    /// Rows written so far.
    pub fn rows(&self) -> u64 {
        self.rows.load(Ordering::Relaxed)
    }

    /// Stops the export before its next page.
    pub fn cancel(&self) {
        self.is_cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.is_cancelled.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportSummary {
    pub rows: u64,
    /// More rows matched than `max_rows`; only the first ones were written.
    pub hit_limit: bool,
}

/// Pages through the whole query and writes every row, up to
/// `export.max_rows`. Fails with [`ErrorKind::Cancelled`] once `progress` is
/// cancelled; what was written so far is then incomplete.
pub async fn export_table(
    session: &dyn DatabaseSession,
    export: TableExport,
    writer: &mut (dyn Write + Send),
    progress: &ExportProgress,
) -> DbResult<ExportSummary> {
    let TableExport {
        mut request,
        format,
        target,
        max_rows,
    } = export;
    let mut exporter: Option<Exporter> = None;

    loop {
        if progress.is_cancelled() {
            return Err(DatabaseError::new(ErrorKind::Cancelled, "Export cancelled"));
        }

        let page = session.fetch_page(request.clone()).await?;

        let exporter = match &mut exporter {
            Some(exporter) => exporter,
            None => {
                let mut started = Exporter::new(format, &target, page.columns.clone());
                started.write_start(writer).map_err(write_error)?;
                exporter.insert(started)
            }
        };

        let remaining = usize::try_from(max_rows - exporter.rows_written()).unwrap_or(usize::MAX);
        let rows = &page.rows[..page.rows.len().min(remaining)];
        exporter.write_rows(writer, rows).map_err(write_error)?;
        progress
            .rows
            .store(exporter.rows_written(), Ordering::Relaxed);

        let has_more = page.next.is_some() || rows.len() < page.rows.len();

        if exporter.rows_written() >= max_rows || !has_more {
            exporter.write_end(writer).map_err(write_error)?;
            return Ok(ExportSummary {
                rows: exporter.rows_written(),
                hit_limit: has_more,
            });
        }

        request.continuation = page.next;
    }
}

fn write_error(error: std::io::Error) -> DatabaseError {
    DatabaseError::new(
        ErrorKind::Other,
        format!("Could not write the export: {error}"),
    )
}
