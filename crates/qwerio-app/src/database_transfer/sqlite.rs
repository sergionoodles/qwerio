use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use qwerio_core::connection::SqliteOptions;
use rusqlite::{Connection, OpenFlags};

use super::files::PrivateFile;
use super::{Operation, TransferRequest};

const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) fn run(
    request: &TransferRequest,
    options: &SqliteOptions,
    cancelled: &AtomicBool,
) -> Result<PathBuf, String> {
    let (source, destination) = match request.operation {
        Operation::Backup => (&options.path, &request.file),
        Operation::Restore => (
            &request.file,
            request
                .restored_file
                .as_ref()
                .ok_or("Choose a new database file")?,
        ),
    };
    if !destination.is_absolute() {
        return Err("Choose an absolute destination path".to_owned());
    }
    if destination.exists() {
        return Err("The destination already exists. Choose a new file name.".to_owned());
    }
    let connection = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(options.busy_timeout)
        .map_err(|error| error.to_string())?;
    let directory = destination.parent().ok_or("Choose a destination folder")?;
    let (output, file) = PrivateFile::create(directory)?;
    drop(file);
    let interrupt = connection.get_interrupt_handle();
    let finished = AtomicBool::new(false);
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::Acquire) {
                if cancelled.load(Ordering::Relaxed) {
                    interrupt.interrupt();
                }
                thread::sleep(CANCEL_POLL_INTERVAL);
            }
        });
        let result = copy_database(&connection, &output.path);
        finished.store(true, Ordering::Release);
        result
    });
    if cancelled.load(Ordering::Relaxed) {
        return Err("Cancelled".to_owned());
    }
    result?;
    output.publish(destination)?;
    Ok(destination.clone())
}

fn copy_database(connection: &Connection, path: &std::path::Path) -> Result<(), String> {
    let integrity: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if integrity != "ok" {
        return Err(format!(
            "The source database failed its integrity check: {integrity}"
        ));
    }
    connection
        .execute(
            "VACUUM INTO ?1",
            [path
                .to_str()
                .ok_or("The destination path must be valid UTF-8")?],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}
