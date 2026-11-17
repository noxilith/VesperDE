//! Session locking (ext-session-lock) and idle handling.
//!
//! While locked, only the lock client's surfaces are drawn and receive input; every other window, bar and
//! binding is out of reach. If the lock client dies the session stays locked and a new one is started.

use std::time::{Duration, Instant};

use smithay::{
    delegate_idle_inhibit, delegate_session_lock,
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{IsAlive, SERIAL_COUNTER},
    wayland::{
        idle_inhibit::IdleInhibitHandler,
        session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
    },
};

use crate::state::Vesper;

impl SessionLockHandler for Vesper {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if let Some(surfaces) = &self.lock {
            // Already locked: another client may only take over if the previous one is gone.
            if surfaces.iter().any(|(s, _)| s.alive()) {
                return; // dropping the confirmation tells the new client the lock failed
            }
        }
        tracing::info!("session locked");
        self.lock = Some(Vec::new());
        self.drag = None;
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(self, None, serial);
        }
        confirmation.lock();
    }

    fn unlock(&mut self) {
        tracing::info!("session unlocked");
        self.lock = None;
        self.last_activity = Instant::now();
        self.layout_changed();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let Some(output) = Output::from_resource(&wl_output) else { return };
        let Some(geo) = self.space.output_geometry(&output) else { return };
        surface.with_pending_state(|s| s.size = Some((geo.size.w as u32, geo.size.h as u32).into()));
        surface.send_configure();
        let focus = surface.wl_surface().clone();
        if let Some(lock) = self.lock.as_mut() {
            lock.retain(|(s, _)| s.alive());
            lock.push((surface, output));
        }
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(self, Some(focus), serial);
        }
    }
}

delegate_session_lock!(Vesper);

impl IdleInhibitHandler for Vesper {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.push(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.retain(|s| *s != surface);
    }
}

delegate_idle_inhibit!(Vesper);

impl Vesper {
    pub fn locked(&self) -> bool {
        self.lock.is_some()
    }

    /// The lock surfaces to draw on `output`, or None when not locked.
    pub fn lock_surfaces_for(&self, output: &Output) -> Option<Vec<WlSurface>> {
        self.lock.as_ref().map(|l| {
            l.iter().filter(|(s, o)| o == output && s.alive()).map(|(s, _)| s.wl_surface().clone()).collect()
        })
    }

    fn locker_running(&mut self) -> bool {
        self.lock_child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// Start the lock client (does nothing if one is already starting or running).
    pub fn start_lock(&mut self) {
        if self.locked() || self.locker_running() || self.last_lock_spawn.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.ptt_release();
        self.last_lock_spawn = Instant::now();
        match Self::spawn_component("vesper-lock") {
            Ok(child) => self.lock_child = Some(child),
            Err(err) => tracing::error!("could not start the lock screen: {err}"),
        }
    }

    /// Called every second: lock after idling, and keep a lock client alive while locked.
    pub fn idle_tick(&mut self) {
        if self.locked() {
            let alive = self.lock.as_ref().is_some_and(|l| l.iter().any(|(s, _)| s.alive()));
            if !alive && !self.locker_running() && self.last_lock_spawn.elapsed() >= Duration::from_secs(2) {
                // The lock client is gone but the session is still locked: bring it back.
                self.last_lock_spawn = Instant::now();
                match Self::spawn_component("vesper-lock") {
                    Ok(child) => self.lock_child = Some(child),
                    Err(err) => tracing::error!("could not restart the lock screen: {err}"),
                }
            }
            return;
        }
        let after = self.config.idle.lock_after;
        if after == 0 {
            return;
        }
        if vesper_ipc::keep_awake_path().exists() {
            return;
        }
        self.idle_inhibitors.retain(|s| s.alive());
        if self.idle_inhibitors.is_empty() && self.last_activity.elapsed() >= Duration::from_secs(after) {
            tracing::info!("idle for {after}s, locking");
            self.start_lock();
        }
    }
}
