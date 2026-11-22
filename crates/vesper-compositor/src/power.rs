//! Screen blanking, suspend, the power menu, and starting the settings window.

use std::time::{Duration, Instant};

use smithay::utils::IsAlive;
use crate::state::Vesper;

impl Vesper {
    /// Lock (when configured) and then suspend.
    pub fn suspend_now(&mut self) {
        self.power_saver(true);
        if self.config.power.lock_before_sleep {
            self.start_lock();
        }
        // give the lock screen a moment to cover the outputs before the machine sleeps
        let cmd = self.config.power.suspend();
        let delay = if self.config.power.lock_before_sleep { 600 } else { 0 };
        let _ = self.loop_handle.insert_source(
            smithay::reexports::calloop::timer::Timer::from_duration(Duration::from_millis(delay)),
            move |_, _, s| {
                s.spawn(&cmd);
                s.last_activity = Instant::now();
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            },
        );
    }

    /// Switch the power plan to saver while the screens are off or the laptop sleeps, and back to the plan it was on when you
    /// return. Uses `busctl` (no Python needed); the plan to go back to is kept in a file in the runtime directory.
    pub fn power_saver(&mut self, on: bool) {
        if self.saver_active == on || (on && !self.config.power.saver_when_idle) {
            return;
        }
        self.saver_active = on;
        const SCRIPT: &str = r#"
f="${XDG_RUNTIME_DIR:-/tmp}/vesper-profile-before"
call() { # call get|set [plan]
    for s in "org.freedesktop.UPower.PowerProfiles /org/freedesktop/UPower/PowerProfiles" "net.hadess.PowerProfiles /net/hadess/PowerProfiles"; do
        set -- "$1" "$2" $s
        if [ "$1" = get ]; then
            v=$(busctl --system get-property "$3" "$4" "$3" ActiveProfile 2>/dev/null | cut -d'"' -f2)
            [ -n "$v" ] && { echo "$v"; return 0; }
        else
            busctl --system set-property "$3" "$4" "$3" ActiveProfile s "$2" 2>/dev/null && return 0
        fi
    done
    return 1
}
if [ "$1" = on ]; then
    cur=$(call get)
    [ -z "$cur" ] && cur=$(powerprofilesctl get 2>/dev/null)
    if [ -n "$cur" ] && [ "$cur" != power-saver ]; then
        echo "$cur" > "$f"
        call set power-saver || powerprofilesctl set power-saver
    fi
else
    if [ -f "$f" ]; then
        prev=$(cat "$f")
        call set "$prev" || powerprofilesctl set "$prev"
        rm -f "$f"
    fi
fi
"#;
        let _ = std::process::Command::new("sh").args(["-c", SCRIPT, "sh", if on { "on" } else { "off" }]).spawn().map(|mut c| {
            // reap it without blocking the compositor
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        });
    }

    /// Idle handling for screen-off and suspend (the idle lock lives in `idle_tick`).
    pub fn power_tick(&mut self) {
        if vesper_ipc::keep_awake_path().exists() {
            // "keep awake" in the quick settings: no screen-off, no suspend
            if self.screens_off {
                self.set_screens_off(false);
            }
            return;
        }
        self.idle_inhibitors.retain(|s| s.alive());
        if !self.idle_inhibitors.is_empty() {
            if self.screens_off {
                self.set_screens_off(false);
            }
            return;
        }
        let idle = self.last_activity.elapsed().as_secs();
        let p = &self.config.power;
        if p.suspend_after > 0 && idle >= p.suspend_after {
            tracing::info!("idle for {idle}s, suspending");
            self.suspend_now();
            return;
        }
        let off = p.screen_off_after > 0 && idle >= p.screen_off_after;
        self.set_screens_off(off);
    }

    pub fn toggle_power_menu(&mut self) {
        Self::toggle_child(&mut self.power_menu, "vesper-power");
    }

    /// Open the settings window (once), optionally on a given page.
    pub fn open_settings(&mut self, page: Option<&str>) {
        if let Some(child) = self.settings.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return; // already open
            }
        }
        let sibling = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("vesper-settings")));
        let bin = sibling.filter(|p| p.is_file()).map(|p| p.into_os_string()).unwrap_or_else(|| "vesper-settings".into());
        let mut cmd = std::process::Command::new(bin);
        if let Some(p) = page {
            cmd.arg("--page").arg(p);
        }
        match cmd.spawn() {
            Ok(c) => self.settings = Some(c),
            Err(err) => tracing::warn!("could not start settings: {err}"),
        }
    }
}

impl Vesper {
    /// The lid was closed: lock or suspend, unless another monitor is connected (then the laptop is docked).
    pub fn lid_closed(&mut self) {
        let docked = self.space.outputs().count() > 1;
        tracing::info!("lid closed (docked: {docked}), action {:?}", self.config.power.lid_close);
        if docked {
            return;
        }
        match self.config.power.lid_close.as_str() {
            "suspend" => self.suspend_now(),
            "ignore" => {}
            _ => self.start_lock(),
        }
    }
}
