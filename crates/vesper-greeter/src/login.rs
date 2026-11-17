//! The conversation with greetd, on its own thread so the screen stays alive while PAM thinks.

use std::{os::unix::net::UnixStream, sync::mpsc};

use smithay_client_toolkit::reexports::calloop::channel;

use crate::greetd::{self, MessageKind, Request, Response};

/// What the screen asks the worker to do.
pub enum Cmd {
    Login { user: String, password: String, session: Vec<String>, env: Vec<String> },
    /// The answer to a further question greetd asked (one-time code, ...).
    Answer(String),
    Cancel,
}

/// What the worker tells the screen.
#[derive(Debug)]
pub enum Ev {
    /// greetd needs another answer: the text to show, and whether to hide what is typed.
    Prompt { text: String, secret: bool },
    Info(String),
    Failed(String),
    /// The session was started: the greeter's job is done.
    Started,
}

pub fn spawn(sock: String, rx: mpsc::Receiver<Cmd>, tx: channel::Sender<Ev>) {
    std::thread::spawn(move || {
        let mut stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(err) => {
                let _ = tx.send(Ev::Failed(format!("cannot reach greetd at {sock}: {err}")));
                return;
            }
        };
        while let Ok(cmd) = rx.recv() {
            if let Cmd::Login { user, password, session, env } = cmd {
                if let Err(err) = login(&mut stream, &rx, &tx, user, password, session, env) {
                    let _ = tx.send(Ev::Failed(format!("lost the connection to greetd: {err}")));
                    return;
                }
            }
        }
    });
}

fn cancel(stream: &mut UnixStream) -> std::io::Result<()> {
    greetd::send(stream, &Request::CancelSession)?;
    greetd::receive(stream).map(|_| ())
}

fn login(stream: &mut UnixStream, rx: &mpsc::Receiver<Cmd>, tx: &channel::Sender<Ev>, user: String, password: String, session: Vec<String>, env: Vec<String>) -> std::io::Result<()> {
    greetd::send(stream, &Request::CreateSession { username: user })?;
    let mut password = Some(password);
    loop {
        match greetd::receive(stream)? {
            Response::AuthMessage { kind: MessageKind::Secret | MessageKind::Visible, message } if password.is_some() => {
                // the first question is answered with the password that was already typed
                let _ = message;
                greetd::send(stream, &Request::PostAuthMessageResponse { response: password.take() })?;
            }
            Response::AuthMessage { kind: kind @ (MessageKind::Secret | MessageKind::Visible), message } => {
                let _ = tx.send(Ev::Prompt { text: message, secret: kind == MessageKind::Secret });
                match rx.recv() {
                    Ok(Cmd::Answer(a)) => greetd::send(stream, &Request::PostAuthMessageResponse { response: Some(a) })?,
                    _ => {
                        cancel(stream)?;
                        let _ = tx.send(Ev::Failed("Cancelled".into()));
                        return Ok(());
                    }
                }
            }
            Response::AuthMessage { message, .. } => {
                let _ = tx.send(Ev::Info(message));
                greetd::send(stream, &Request::PostAuthMessageResponse { response: None })?;
            }
            Response::Error { description, auth_error } => {
                cancel(stream)?;
                let _ = tx.send(Ev::Failed(if auth_error { "Wrong username or password".into() } else { description }));
                return Ok(());
            }
            Response::Success => break,
        }
    }
    greetd::send(stream, &Request::StartSession { cmd: session, env })?;
    match greetd::receive(stream)? {
        Response::Success => {
            let _ = tx.send(Ev::Started);
        }
        Response::Error { description, .. } => {
            cancel(stream)?;
            let _ = tx.send(Ev::Failed(description));
        }
        Response::AuthMessage { .. } => {
            cancel(stream)?;
            let _ = tx.send(Ev::Failed("greetd asked a question after login".into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A fake greetd that accepts "pw" for "ada": the whole happy path and a wrong password.
    #[test]
    fn logs_in_against_a_fake_greetd() {
        let dir = std::env::temp_dir().join(format!("vesper-greeter-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("greetd.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            for round in 0..2 {
                let read = |s: &mut UnixStream| -> serde_json::Value {
                    use std::io::Read;
                    let mut len = [0u8; 4];
                    s.read_exact(&mut len).unwrap();
                    let mut b = vec![0u8; u32::from_ne_bytes(len) as usize];
                    s.read_exact(&mut b).unwrap();
                    serde_json::from_slice(&b).unwrap()
                };
                let write = |s: &mut UnixStream, v: serde_json::Value| {
                    use std::io::Write;
                    let b = serde_json::to_vec(&v).unwrap();
                    s.write_all(&(b.len() as u32).to_ne_bytes()).unwrap();
                    s.write_all(&b).unwrap();
                };
                assert_eq!(read(&mut s)["type"], "create_session");
                write(&mut s, serde_json::json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password: "}));
                let answer = read(&mut s);
                if answer["response"] == "pw" {
                    write(&mut s, serde_json::json!({"type": "success"}));
                    assert_eq!(read(&mut s)["type"], "start_session");
                    write(&mut s, serde_json::json!({"type": "success"}));
                } else {
                    write(&mut s, serde_json::json!({"type": "error", "error_type": "auth_error", "description": "nope"}));
                    assert_eq!(read(&mut s)["type"], "cancel_session");
                    write(&mut s, serde_json::json!({"type": "success"}));
                }
                let _ = round;
            }
        });
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel::<Ev>();
        // the calloop channel needs an event loop; drive the worker body directly instead
        let mut stream = UnixStream::connect(&path).unwrap();
        let (ctx, crx) = channel::channel::<Ev>();
        drop(cmd_tx);
        login(&mut stream, &cmd_rx, &ctx, "ada".into(), "wrong".into(), vec!["true".into()], vec![]).unwrap();
        login(&mut stream, &cmd_rx, &ctx, "ada".into(), "pw".into(), vec!["true".into()], vec![]).unwrap();
        drop(ev_tx);
        drop(ev_rx);
        server.join().unwrap();
        // calloop's channel is read through an event loop
        let mut seen = Vec::new();
        let mut ev_loop: smithay_client_toolkit::reexports::calloop::EventLoop<Vec<String>> = smithay_client_toolkit::reexports::calloop::EventLoop::try_new().unwrap();
        ev_loop.handle().insert_source(crx, |e, _, seen: &mut Vec<String>| {
            if let channel::Event::Msg(m) = e {
                seen.push(format!("{m:?}"));
            }
        }).unwrap();
        ev_loop.dispatch(std::time::Duration::from_millis(50), &mut seen).unwrap();
        assert_eq!(seen, vec!["Failed(\"Wrong username or password\")".to_string(), "Started".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
