//! Bounded control commands and owned networking processes.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub fn private_write(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    f.write_all(text.as_bytes())
}

// Short control commands are bounded. Supplicant and DHCP are separate owned children.
pub fn output(program: &str, args: &[&str]) -> Result<String, &'static str> {
    output_timeout(program, args, Duration::from_secs(2))
}

pub fn output_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, &'static str> {
    output_timeout_details(program, args, timeout).map_err(|e| e.code)
}

pub struct CommandError {
    pub code: &'static str,
    pub stderr: String,
}
impl CommandError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            stderr: String::new(),
        }
    }
}

pub fn output_timeout_details(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, CommandError> {
    output_timeout_cancellable(program, args, timeout, &AtomicBool::new(false))
}

pub fn output_timeout_cancellable(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<String, CommandError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(CommandError::new("SCAN_CANCELLED"));
    }
    let deadline = Instant::now() + timeout;
    let bundled = std::env::var_os("SLOT_ROOT")
        .map(PathBuf::from)
        .map(|root| root.join("System/slot-net"));
    let mut command =
        if program == "iw" && bundled.as_ref().is_some_and(|dir| dir.join("iw").is_file()) {
            let dir = bundled.unwrap();
            let mut command = Command::new("/lib/ld-linux-aarch64.so.1");
            command.arg("--library-path").arg(&dir).arg(dir.join("iw"));
            command
        } else {
            Command::new(program)
        };
    let mut child = command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| CommandError::new("TOOL_UNAVAILABLE"))?;
    // Drain concurrently: iw's capabilities can exceed a pipe buffer.
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        let _ = stdout.take(256 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        let _ = stderr.take(256 * 1024).read_to_end(&mut bytes);
        bytes
    });
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = errors.join();
            return Err(CommandError::new("SCAN_CANCELLED"));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = reader.join().unwrap_or_default();
                let stderr = errors.join().unwrap_or_default();
                return if status.success() {
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                } else {
                    Err(CommandError {
                        code: "COMMAND_FAILED",
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    })
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CommandError::new("COMMAND_TIMEOUT"));
            }
        }
    }
}

pub fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|s| s.strip_prefix('=')))
}

pub fn identity(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field is parenthesized and can itself contain spaces/parentheses.
    let tail = stat.rsplit_once(") ")?.1;
    Some(format!("{pid}:{}", tail.split_whitespace().nth(19)?))
}

pub fn alive(token: &str) -> bool {
    token
        .split_once(':')
        .and_then(|(pid, _)| pid.parse().ok())
        .and_then(identity)
        .as_deref()
        == Some(token)
}

pub struct Process {
    child: Option<Child>,
    pid: u32,
    token: String,
    record: PathBuf,
}
impl Process {
    pub fn spawn(program: &str, args: &[&str], record: PathBuf) -> Result<Self, &'static str> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "PROCESS_START")?;
        let result = identity(child.id())
            .ok_or("PROCESS_IDENTITY")
            .and_then(|token| {
                private_write(&record, &token)
                    .map(|_| token)
                    .map_err(|_| "PROCESS_RECORD")
            });
        match result {
            Ok(token) => Ok(Self {
                pid: child.id(),
                child: Some(child),
                token,
                record,
            }),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }
    pub fn adopt(record: PathBuf, run: &Path) -> Option<Self> {
        let token = fs::read_to_string(&record).ok()?;
        if !alive(&token) {
            let _ = fs::remove_file(&record);
            return None;
        }
        let pid = token.split(':').next()?.parse().ok()?;
        let cmd = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        if !String::from_utf8_lossy(&cmd).contains(run.to_string_lossy().as_ref()) {
            return None;
        }
        Some(Self {
            child: None,
            pid,
            token,
            record,
        })
    }
    pub fn running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => alive(&self.token),
        }
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        } else if alive(&self.token) {
            unsafe {
                libc::kill(self.pid as i32, libc::SIGKILL);
            }
            for _ in 0..20 {
                if !alive(&self.token) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = fs::remove_file(&self.record);
    }
}

pub fn external_radio_owner(ours: &[u32]) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return true;
    };
    entries.flatten().any(|e| {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            return false;
        };
        if ours.contains(&pid) {
            return false;
        }
        let cmd = fs::read(e.path().join("cmdline")).unwrap_or_default();
        let text = String::from_utf8_lossy(&cmd);
        (text.contains("wpa_supplicant") || text.contains("hostapd") || text.contains("udhcpc"))
            && (text.contains("wlan0") || text.contains("wlan1"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_scan_kills_and_reaps_its_process() {
        use std::sync::Arc;
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let worker = std::thread::spawn(move || {
            output_timeout_cancellable(
                "sh",
                &["-c", "exec sleep 5"],
                Duration::from_secs(10),
                &signal,
            )
        });
        std::thread::sleep(Duration::from_millis(30));
        let at = Instant::now();
        cancelled.store(true, Ordering::Release);
        let error = worker.join().unwrap().err().unwrap();
        assert_eq!(error.code, "SCAN_CANCELLED");
        assert!(at.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn status_matches_complete_keys_only() {
        let s = "old_freq=123\nfreq=2412\nwpa_state=COMPLETED\n";
        assert_eq!(field(s, "freq"), Some("2412"));
        assert_eq!(field(s, "state"), None);
    }

    #[test]
    fn failed_commands_keep_stderr_for_scan_busy_detection() {
        let result = output_timeout_details(
            "sh",
            &[
                "-c",
                "printf 'command failed: Device or resource busy (-16)\n' >&2; exit 1",
            ],
            Duration::from_secs(1),
        );
        let error = result.err().expect("the command succeeded");
        assert_eq!(error.code, "COMMAND_FAILED");
        assert_eq!(
            error.stderr,
            "command failed: Device or resource busy (-16)\n"
        );
    }
}
