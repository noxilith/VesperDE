//! The greetd IPC protocol: a native-endian u32 length, then a JSON object, over the unix socket in `$GREETD_SOCK`.
//! See `greetd-ipc(7)`.

use std::io::{Read, Write};

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    CreateSession { username: String },
    PostAuthMessageResponse { response: Option<String> },
    StartSession { cmd: Vec<String>, env: Vec<String> },
    CancelSession,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessageKind {
    Visible,
    Secret,
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Success,
    Error { description: String, auth_error: bool },
    AuthMessage { kind: MessageKind, message: String },
}

impl Request {
    fn to_json(&self) -> Value {
        match self {
            Request::CreateSession { username } => json!({"type": "create_session", "username": username}),
            Request::PostAuthMessageResponse { response } => json!({"type": "post_auth_message_response", "response": response}),
            Request::StartSession { cmd, env } => json!({"type": "start_session", "cmd": cmd, "env": env}),
            Request::CancelSession => json!({"type": "cancel_session"}),
        }
    }
}

pub fn parse_response(v: &Value) -> Option<Response> {
    match v["type"].as_str()? {
        "success" => Some(Response::Success),
        "error" => Some(Response::Error {
            description: v["description"].as_str().unwrap_or("login failed").to_string(),
            auth_error: v["error_type"].as_str() == Some("auth_error"),
        }),
        "auth_message" => {
            let kind = match v["auth_message_type"].as_str()? {
                "visible" => MessageKind::Visible,
                "secret" => MessageKind::Secret,
                "error" => MessageKind::Error,
                _ => MessageKind::Info,
            };
            Some(Response::AuthMessage { kind, message: v["auth_message"].as_str().unwrap_or_default().to_string() })
        }
        _ => None,
    }
}

pub fn send<W: Write>(w: &mut W, req: &Request) -> std::io::Result<()> {
    let body = serde_json::to_vec(&req.to_json()).map_err(std::io::Error::other)?;
    w.write_all(&(body.len() as u32).to_ne_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

pub fn receive<R: Read>(r: &mut R) -> std::io::Result<Response> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_ne_bytes(len) as usize;
    if len > 1 << 20 {
        return Err(std::io::Error::other("greetd sent an absurdly large message"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let v: Value = serde_json::from_slice(&body).map_err(std::io::Error::other)?;
    parse_response(&v).ok_or_else(|| std::io::Error::other("greetd sent a message this greeter does not know"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_length_prefixed_json() {
        let mut out = Vec::new();
        send(&mut out, &Request::CreateSession { username: "ada".into() }).unwrap();
        let len = u32::from_ne_bytes(out[..4].try_into().unwrap()) as usize;
        assert_eq!(len, out.len() - 4);
        let v: Value = serde_json::from_slice(&out[4..]).unwrap();
        assert_eq!(v, json!({"type": "create_session", "username": "ada"}));
        let mut out = Vec::new();
        send(&mut out, &Request::PostAuthMessageResponse { response: None }).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&out[4..]).unwrap(), json!({"type": "post_auth_message_response", "response": null}));
    }

    #[test]
    fn responses_parse() {
        let frame = |s: &str| {
            let mut b = (s.len() as u32).to_ne_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let mut r = &frame(r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#)[..];
        assert_eq!(receive(&mut r).unwrap(), Response::AuthMessage { kind: MessageKind::Secret, message: "Password: ".into() });
        let mut r = &frame(r#"{"type":"error","error_type":"auth_error","description":"pam_authenticate: AUTH_ERR"}"#)[..];
        assert_eq!(receive(&mut r).unwrap(), Response::Error { description: "pam_authenticate: AUTH_ERR".into(), auth_error: true });
        let mut r = &frame(r#"{"type":"success"}"#)[..];
        assert_eq!(receive(&mut r).unwrap(), Response::Success);
    }
}
