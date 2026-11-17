//! Password check through PAM (libpam), with just the three calls we need.

use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::Path,
};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_BUF_ERR: c_int = 5;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const PamConv, handle: *mut *mut c_void) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
}

/// Answers every password prompt with the password in `appdata`.
unsafe extern "C" fn converse(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    let password = appdata as *const c_char;
    let out = libc::calloc(count as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if out.is_null() {
        return PAM_BUF_ERR;
    }
    for i in 0..count as usize {
        let msg = *messages.add(i);
        let style = (*msg).msg_style;
        if style == PAM_PROMPT_ECHO_OFF || style == PAM_PROMPT_ECHO_ON {
            let dup = libc::strdup(password);
            if dup.is_null() {
                for j in 0..i {
                    libc::free((*out.add(j)).resp as *mut c_void);
                }
                libc::free(out as *mut c_void);
                return PAM_BUF_ERR;
            }
            (*out.add(i)).resp = dup;
        }
    }
    *responses = out;
    PAM_SUCCESS
}

/// The login name of the user running this process.
pub fn current_user() -> Option<String> {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        Some(CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned())
    }
}

/// True if `password` is correct for `user`. Blocks (PAM adds a delay after failures), so call it off the UI thread.
pub fn authenticate(user: &str, password: &str) -> bool {
    let service = if Path::new("/etc/pam.d/vesper-lock").exists() { "vesper-lock" } else { "login" };
    let (Ok(service), Ok(user), Ok(password)) = (CString::new(service), CString::new(user), CString::new(password)) else {
        return false;
    };
    unsafe {
        let conv = PamConv { conv: converse, appdata_ptr: password.as_ptr() as *mut c_void };
        let mut handle: *mut c_void = std::ptr::null_mut();
        if pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle) != PAM_SUCCESS {
            return false;
        }
        let rc = pam_authenticate(handle, 0);
        pam_end(handle, rc);
        rc == PAM_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_password_is_rejected() {
        let user = current_user().expect("a current user");
        assert!(!authenticate(&user, "this-is-definitely-not-the-password-\u{1f512}"));
        assert!(!authenticate(&user, ""));
        assert!(!authenticate("no-such-user-vesper-test", "x"));
    }
}
