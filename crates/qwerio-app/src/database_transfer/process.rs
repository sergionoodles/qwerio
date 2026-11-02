use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_DIAGNOSTIC_BYTES: usize = 8192;

fn read_output(mut reader: impl Read) -> Result<String, String> {
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(String::from_utf8_lossy(&output).trim().to_owned());
        }
        let remaining = MAX_DIAGNOSTIC_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
    }
}

pub(super) fn run(
    command: &mut Command,
    cancelled: &AtomicBool,
    timeout: Option<Duration>,
    capture_stdout: bool,
) -> Result<String, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Cancelled".to_owned());
    }
    command.stderr(Stdio::piped());
    if capture_stdout {
        command.stdout(Stdio::piped());
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stderr = child.stderr.take().ok_or("Missing client error output")?;
    let stdout = child.stdout.take();
    thread::scope(|scope| {
        let errors = scope.spawn(move || read_output(stderr));
        let output = scope.spawn(move || match stdout {
            Some(stdout) => read_output(stdout),
            None => Ok(String::new()),
        });
        let started = Instant::now();
        let status = loop {
            let stop_reason = if cancelled.load(Ordering::Relaxed) {
                Some("Cancelled; a restore may have applied some changes")
            } else if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
                Some("Tool check timed out")
            } else {
                None
            };
            if let Some(reason) = stop_reason {
                // The process may have exited between the poll and kill.
                if let Err(error) = child.kill()
                    && child
                        .try_wait()
                        .map_err(|error| error.to_string())?
                        .is_none()
                {
                    return Err(format!("Could not stop client: {error}"));
                }
                child.wait().map_err(|error| error.to_string())?;
                return Err(reason.to_owned());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Err(error) => {
                    child.kill().map_err(|error| error.to_string())?;
                    child.wait().map_err(|error| error.to_string())?;
                    return Err(error.to_string());
                }
            }
        };
        let errors = errors.join().map_err(|_| "Client output reader failed")??;
        let output = output.join().map_err(|_| "Client output reader failed")??;
        if !status.success() {
            return Err(if errors.is_empty() {
                format!("Client exited with {status}")
            } else {
                errors
            });
        }
        Ok(if errors.is_empty() {
            output
        } else {
            format!("{output}\n{errors}").trim().to_owned()
        })
    })
}
