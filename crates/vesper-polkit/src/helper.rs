//! polkit's setuid `polkit-agent-helper-1`: it does the PAM conversation for us.
//!
//! Protocol (see polkit's `polkitagenthelperprivate.h`): we start `polkit-agent-helper-1 <user>` and write the
//! cookie on the first line of its stdin. It then prints one line at a time:
//! `PAM_PROMPT_ECHO_OFF <text>` / `PAM_PROMPT_ECHO_ON <text>` (answer with one line on stdin),
//! `PAM_ERROR_MSG <text>`, `PAM_TEXT_INFO <text>`, and finally `SUCCESS` or `FAILURE`.
//! On `SUCCESS` the helper has already told polkitd that the cookie is authenticated.

use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A question; the answer must not be echoed (a password).
    PromptHidden(String),
    /// A question whose answer may be shown.
    PromptVisible(String),
    Error(String),
    Info(String),
    Success,
    Failure,
    /// The helper closed its output (it exited).
    Eof,
    /// Anything we do not know; kept for the log.
    Other(String),
}

/// Parse one line the helper printed.
pub fn parse_line(raw: &str) -> Line {
    let s = raw.trim_end_matches(['\r', '\n']);
    let (head, rest) = match s.split_once(' ') {
        Some((h, r)) => (h, r),
        None => (s, ""),
    };
    match head {
        "PAM_PROMPT_ECHO_OFF" => Line::PromptHidden(rest.to_string()),
        "PAM_PROMPT_ECHO_ON" => Line::PromptVisible(rest.to_string()),
        "PAM_ERROR_MSG" => Line::Error(rest.to_string()),
        "PAM_TEXT_INFO" => Line::Info(rest.to_string()),
        "SUCCESS" if rest.is_empty() => Line::Success,
        "FAILURE" if rest.is_empty() => Line::Failure,
        _ => Line::Other(s.to_string()),
    }
}

/// Where the helper lives on this system.
pub fn find_helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VESPER_POLKIT_HELPER").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    [
        "/usr/lib/polkit-1/polkit-agent-helper-1",
        "/usr/libexec/polkit-1/polkit-agent-helper-1",
        "/usr/libexec/polkit-agent-helper-1",
        "/usr/lib64/polkit-1/polkit-agent-helper-1",
        "/usr/lib/policykit-1/polkit-agent-helper-1",
        "/run/current-system/sw/libexec/polkit-agent-helper-1",
        "/run/wrappers/bin/polkit-agent-helper-1",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// One running helper (it authenticates once, then exits). Dropping it kills it.
pub struct Helper {
    child: Child,
    stdin: ChildStdin,
}

impl Helper {
    /// Start the helper for `user` and the polkit `cookie`. Everything it prints arrives on `tx` as `(id, line)`,
    /// ending with `Line::Eof`.
    pub fn spawn(path: &PathBuf, user: &str, cookie: &str, id: u64, tx: Sender<(u64, Line)>) -> std::io::Result<Helper> {
        let mut child = Command::new(path).arg(user).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
        writeln!(stdin, "{cookie}")?;
        stdin.flush()?;
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send((id, parse_line(&line))).is_err() {
                    return;
                }
            }
            let _ = tx.send((id, Line::Eof));
        });
        Ok(Helper { child, stdin })
    }

    /// Answer a prompt.
    pub fn answer(&mut self, text: &str) -> std::io::Result<()> {
        writeln!(self.stdin, "{text}")?;
        self.stdin.flush()
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prompts_and_results() {
        assert_eq!(parse_line("PAM_PROMPT_ECHO_OFF Password: \n"), Line::PromptHidden("Password: ".into()));
        assert_eq!(parse_line("PAM_PROMPT_ECHO_ON Login:"), Line::PromptVisible("Login:".into()));
        assert_eq!(parse_line("PAM_ERROR_MSG Authentication failure"), Line::Error("Authentication failure".into()));
        assert_eq!(parse_line("PAM_TEXT_INFO Place your finger on the reader"), Line::Info("Place your finger on the reader".into()));
        assert_eq!(parse_line("SUCCESS\n"), Line::Success);
        assert_eq!(parse_line("FAILURE\r\n"), Line::Failure);
    }

    #[test]
    fn keeps_unknown_lines_and_empty_messages() {
        assert_eq!(parse_line("SUCCESS but not really"), Line::Other("SUCCESS but not really".into()));
        assert_eq!(parse_line("hello"), Line::Other("hello".into()));
        assert_eq!(parse_line("PAM_ERROR_MSG"), Line::Error(String::new()));
        assert_eq!(parse_line(""), Line::Other(String::new()));
    }

    /// A fake helper (a shell script) drives the whole conversation: cookie in, prompt out, password in, result out.
    #[test]
    fn talks_to_a_helper_process() {
        use smithay_client_toolkit::reexports::calloop::channel::{channel, Event};
        let dir = std::env::temp_dir().join(format!("vesper-polkit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("helper.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nread cookie\nread_user=\"$1\"\necho \"PAM_PROMPT_ECHO_OFF Password: \"\nread pw\nif [ \"$cookie:$read_user:$pw\" = \"c00k1e:alice:hunter2\" ]; then echo SUCCESS; else echo PAM_ERROR_MSG nope; echo FAILURE; fi\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (tx, rx) = channel::<(u64, Line)>();
        let mut h = Helper::spawn(&script, "alice", "c00k1e", 7, tx).unwrap();
        let mut got = Vec::new();
        let mut rx = Some(rx);
        // Plain blocking read of the calloop channel through a tiny event loop.
        let mut lp = smithay_client_toolkit::reexports::calloop::EventLoop::<(Vec<(u64, Line)>, bool)>::try_new().unwrap();
        lp.handle()
            .insert_source(rx.take().unwrap(), |ev, _, st| {
                if let Event::Msg(m) = ev {
                    st.1 |= m.1 == Line::Eof;
                    st.0.push(m);
                }
            })
            .unwrap();
        let mut st = (Vec::new(), false);
        let mut answered = false;
        for _ in 0..200 {
            lp.dispatch(Some(std::time::Duration::from_millis(50)), &mut st).unwrap();
            if !answered && st.0.iter().any(|(_, l)| matches!(l, Line::PromptHidden(_))) {
                h.answer("hunter2").unwrap();
                answered = true;
            }
            if st.1 {
                break;
            }
        }
        got.extend(st.0);
        assert_eq!(got[0], (7, Line::PromptHidden("Password: ".into())));
        assert_eq!(got[1], (7, Line::Success));
        assert_eq!(got[2], (7, Line::Eof));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
