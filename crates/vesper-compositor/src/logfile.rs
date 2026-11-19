//! The session log. Everything the compositor and the programs it starts print goes to the screen as before and also to
//! `~/.local/state/vesper/vesper.log` (colours removed), so a problem on someone else's machine can be read afterwards
//! without redirecting anything. The file starts over once it passes 2 MB. `VESPER_NO_LOG_FILE=1` turns it off.

use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::PathBuf,
};

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("vesper/vesper.log")
}

/// Remove `ESC [ ... letter` colour codes.
pub fn strip_ansi(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == 0x1b && s.get(i + 1) == Some(&b'[') {
            i += 2;
            while i < s.len() && !(0x40..=0x7e).contains(&s[i]) {
                i += 1;
            }
            i += 1;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// Send stdout and stderr through a pipe that a thread copies to the original stderr and to the log file.
/// Does nothing (and returns false) if anything goes wrong: logging must never stop the desktop from starting.
pub fn start() -> bool {
    if std::env::var_os("VESPER_NO_LOG_FILE").is_some() {
        return false;
    }
    let file = path();
    let Some(dir) = file.parent() else { return false };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    if std::fs::metadata(&file).map(|m| m.len() > 2 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&file);
    }
    let Ok(mut log) = OpenOptions::new().create(true).append(true).open(&file) else { return false };
    let Ok((mut rx, tx)) = std::io::pipe() else { return false };
    // SAFETY: plain fd duplication of our own stdout/stderr; the copies are owned by the thread below.
    let screen = unsafe { libc::dup(2) };
    if screen < 0 {
        return false;
    }
    let mut screen = unsafe { std::fs::File::from_raw_fd(screen) };
    unsafe {
        if libc::dup2(tx.as_raw_fd(), 1) < 0 || libc::dup2(tx.as_raw_fd(), 2) < 0 {
            return false;
        }
    }
    drop(tx);
    let _ = writeln!(log, "---- vesper started ----");
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = rx.read(&mut buf) {
            if n == 0 {
                break;
            }
            let _ = screen.write_all(&buf[..n]);
            let _ = log.write_all(&strip_ansi(&buf[..n]));
        }
    });
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_colour_codes() {
        let s = super::strip_ansi(b"\x1b[2m2026\x1b[0m \x1b[32m INFO\x1b[0m hello");
        assert_eq!(String::from_utf8(s).unwrap(), "2026  INFO hello");
    }
}
