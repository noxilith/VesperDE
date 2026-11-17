//! The "Switch user" button: which command (if any) takes you to the display manager's login screen.

use std::ffi::OsString;

/// What the system tells us, passed in so this can be tested without a display manager.
pub struct Probe<'a> {
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub path_exists: &'a dyn Fn(&str) -> bool,
    pub on_path: &'a dyn Fn(&str) -> bool,
}

/// Resolve the `[lock] switch_user` setting to a shell command, or None if there should be no button.
///
/// - `"never"` or empty: no button.
/// - `"auto"`: a button when a display manager is running. LightDM has `dm-tool`; SDDM (and others that implement
///   `org.freedesktop.DisplayManager`) is asked over D-Bus to show a new greeter. Your session stays running, locked.
/// - anything else: your own command.
pub fn resolve(setting: &str, probe: &Probe) -> Option<String> {
    match setting.trim() {
        "" | "never" => None,
        "auto" => {
            if (probe.on_path)("dm-tool") && (probe.path_exists)("/run/lightdm") {
                return Some("dm-tool switch-to-greeter".into());
            }
            let seat = (probe.env)("XDG_SEAT_PATH").and_then(|s| s.into_string().ok());
            let running_dm = (probe.path_exists)("/run/sddm") || (probe.path_exists)("/run/lightdm") || seat.is_some();
            running_dm.then(|| {
                let seat = seat.unwrap_or_else(|| "/org/freedesktop/DisplayManager/Seat0".into());
                format!(
                    "dbus-send --system --print-reply --dest=org.freedesktop.DisplayManager {seat} \
                     org.freedesktop.DisplayManager.Seat.SwitchToGreeter"
                )
            })
        }
        custom => Some(custom.to_owned()),
    }
}

pub fn system_probe_resolve(setting: &str) -> Option<String> {
    let env = |k: &str| std::env::var_os(k);
    let exists = |p: &str| std::path::Path::new(p).exists();
    let on_path = |name: &str| {
        std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(name).is_file()))
    };
    resolve(setting, &Probe { env: &env, path_exists: &exists, on_path: &on_path })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe<'a>(
        env: &'a dyn Fn(&str) -> Option<OsString>,
        exists: &'a dyn Fn(&str) -> bool,
        on_path: &'a dyn Fn(&str) -> bool,
    ) -> Probe<'a> {
        Probe { env, path_exists: exists, on_path }
    }

    #[test]
    fn no_display_manager_no_button() {
        let p = probe(&|_| None, &|_| false, &|_| false);
        assert_eq!(resolve("auto", &p), None);
        assert_eq!(resolve("never", &p), None);
        assert_eq!(resolve("", &p), None);
    }

    #[test]
    fn sddm_uses_dbus_seat0() {
        let p = probe(&|_| None, &|path| path == "/run/sddm", &|_| false);
        let cmd = resolve("auto", &p).unwrap();
        assert!(cmd.contains("/org/freedesktop/DisplayManager/Seat0"));
        assert!(cmd.contains("SwitchToGreeter"));
    }

    #[test]
    fn seat_path_from_environment_wins() {
        let p = probe(&|k| (k == "XDG_SEAT_PATH").then(|| "/org/freedesktop/DisplayManager/Seat3".into()), &|_| false, &|_| false);
        assert!(resolve("auto", &p).unwrap().contains("Seat3"));
    }

    #[test]
    fn lightdm_uses_dm_tool() {
        let p = probe(&|_| None, &|path| path == "/run/lightdm", &|n| n == "dm-tool");
        assert_eq!(resolve("auto", &p).as_deref(), Some("dm-tool switch-to-greeter"));
    }

    #[test]
    fn custom_command_is_used_as_is() {
        let p = probe(&|_| None, &|_| false, &|_| false);
        assert_eq!(resolve("my-switcher --now", &p).as_deref(), Some("my-switcher --now"));
    }
}
