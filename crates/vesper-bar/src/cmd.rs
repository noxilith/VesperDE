//! Run helper programs (`wpctl`, `nmcli`, `bluetoothctl`, `lsblk`, ...) without ever hanging the bar. The popups ask
//! for their data on the bar's own thread; a program that waits forever (`bluetoothctl` with no bluetooth daemon does)
//! would freeze the whole bar and no popup would open. A program that takes more than two seconds is stopped.

use std::{
    io::Read,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(2);

/// Like `Command::output`, but with no input, and stopped after two seconds (that counts as a failed run).
pub fn output(cmd: &mut Command) -> std::io::Result<Output> {
    let started = Instant::now();
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut v);
            }
            v
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    loop {
        if let Some(status) = child.try_wait()? {
            let took = started.elapsed();
            if took > Duration::from_millis(500) {
                tracing::warn!("{cmd:?} was slow: {} ms", took.as_millis());
            }
            return Ok(Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() });
        }
        if started.elapsed() >= LIMIT {
            tracing::warn!("{cmd:?} did not finish in {} s: stopped it", LIMIT.as_secs());
            let _ = child.kill();
            let status = child.wait()?;
            // the reader threads end with the pipes; do not wait for them (a grandchild may still hold one open)
            return Ok(Output { status, stdout: Vec::new(), stderr: Vec::new() });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_output_and_stops_a_hung_program() {
        let o = output(Command::new("sh").args(["-c", "echo hi"])).unwrap();
        assert!(o.status.success());
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "hi");
        let t = Instant::now();
        let o = output(Command::new("sh").args(["-c", "sleep 30"])).unwrap();
        assert!(!o.status.success());
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
