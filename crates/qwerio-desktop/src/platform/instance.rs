//! An OS-owned lock keeps one app process alive per desktop account. The
//! file stays in place; deleting it would let contenders lock different files.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

const LOCK_FILE: &str = "application.lock";

pub struct AppInstance {
    _lock: File,
}

impl AppInstance {
    pub fn acquire(state_directory: &Path) -> io::Result<Option<Self>> {
        std::fs::create_dir_all(state_directory)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let lock = options.open(state_directory.join(LOCK_FILE))?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(Self { _lock: lock })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const TEST_DIRECTORY: &str = "QWERIO_TEST_INSTANCE_DIRECTORY";
    const TEST_MODE: &str = "QWERIO_TEST_INSTANCE_MODE";
    const CHILD_TEST: &str =
        "platform::instance::tests::locks_across_processes_and_recovers_after_exit";

    #[test]
    fn locks_across_processes_and_recovers_after_exit() {
        if let Some(directory) = std::env::var_os(TEST_DIRECTORY) {
            let acquired = AppInstance::acquire(Path::new(&directory)).expect("instance lock");
            if std::env::var(TEST_MODE).as_deref() == Ok("exit") {
                assert!(acquired.is_some());
                // Simulate exit without running the lock guard's destructor.
                std::process::exit(0);
            }
            assert!(acquired.is_none(), "another process owns the instance lock");
            return;
        }

        let directory = tempfile::TempDir::new().expect("directory");
        let instance = AppInstance::acquire(directory.path())
            .expect("lock")
            .expect("first instance");
        assert!(
            AppInstance::acquire(directory.path())
                .expect("second attempt")
                .is_none()
        );
        let run_child = |mode: &str| {
            let output = Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", CHILD_TEST])
                .env(TEST_DIRECTORY, directory.path())
                .env(TEST_MODE, mode)
                .output()
                .expect("child process");
            assert!(
                output.status.success(),
                "child failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_child("busy");
        drop(instance);
        run_child("exit");
        assert!(
            AppInstance::acquire(directory.path())
                .expect("released lock")
                .is_some()
        );
    }

    #[test]
    fn lock_creation_errors_are_reported() {
        let directory = tempfile::TempDir::new().expect("directory");
        std::fs::write(directory.path().join(LOCK_FILE), "existing lock file").expect("file");
        assert!(AppInstance::acquire(&directory.path().join(LOCK_FILE)).is_err());
    }
}
