use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{keysyms, FilterResult, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::SERIAL_COUNTER,
};

use crate::state::{Drag, Vesper};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cap {
    Screen,
    Window,
    Region,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Media {
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MicMute,
    BrightnessUp,
    BrightnessDown,
    PlayPause,
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Spawn,
    CloseWindow,
    FocusColumn(isize),
    MoveColumn(isize),
    Workspace(isize),
    WorkspaceTo(usize),
    MoveToWorkspaceTo(usize),
    MoveToWorkspace(isize),
    Quit,
    VtSwitch(i32),
    CycleWidth,
    AdjustWidth(f64),
    ToggleFullscreen,
    Minimize,
    RestoreLast,
    Launcher,
    Switcher,
    Clipboard,
    Emoji,
    Lock,
    Wallpapers,
    ToggleFloating,
    /// Float or tile just the focused window.
    ToggleFloatingWindow,
    /// Focus the monitor to the left (-1) or right (1).
    MonitorFocus(isize),
    /// Send the focused window to the monitor to the left (-1) or right (1).
    MonitorSend(isize),
    Files,
    Settings,
    ConsumeLeft,
    Expel,
    FocusInColumn(isize),
    ToggleBlur,
    Notifications,
    StartMenu,
    PttDown,
    PttUp,
    /// The key pressed while learning the push-to-talk key: (xkb keycode, keysym).
    PttLearn(u32, u32),
    PowerMenu,
    PowerKey,
    Media(Media),
    Capture(Cap),
    /// A shortcut a plugin added: (index into `plugins.active`, index of its `[[key]]`).
    PluginKey(usize, usize),
    /// Key was consumed (e.g. a release for a consumed press).
    None,
}

/// ASCII-lowercase a keysym so Shift+Q and q hit the same binding.
fn lower(sym: u32) -> u32 {
    if (0x41..=0x5a).contains(&sym) { sym + 0x20 } else { sym }
}

impl Vesper {
    fn uses_super(&self) -> bool {
        match self.config.input.modifier.as_deref() {
            Some("super") | Some("logo") | Some("mod4") => true,
            Some("alt") | Some("mod1") => false,
            _ => !self.nested,
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        self.last_activity = std::time::Instant::now();
        if self.screens_off {
            self.set_screens_off(false);
        }
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard::<I>(event),
            InputEvent::PointerMotion { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                // holding Mod+Alt: the pointer stays where it is and the floating windows follow the motion (an endless desktop)
                if self.drag.is_none() && self.canvas_pan_held() {
                    self.pan_floating(event.delta());
                    return;
                }
                let pos = self.clamp_to_outputs(pointer.current_location() + event.delta());
                if self.drag.is_some() {
                    self.drag_to(pos, event.time_msec());
                    return;
                }
                self.move_pointer(pos, event.time_msec());
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(geo) = self.output_geometry() else { return };
                let pos = event.position_transformed(geo.size) + geo.loc.to_f64();
                if self.drag.is_some() {
                    self.drag_to(pos, event.time_msec());
                    return;
                }
                self.move_pointer(pos, event.time_msec());
            }
            InputEvent::PointerButton { event } => self.on_button::<I>(event),
            InputEvent::PointerAxis { event } => self.on_axis::<I>(event),
            InputEvent::GestureSwipeBegin { event } => {
                use smithay::backend::input::GestureBeginEvent;
                self.swipe = Some((0.0, 0.0, event.fingers()));
            }
            InputEvent::GestureSwipeUpdate { event } => {
                use smithay::backend::input::GestureSwipeUpdateEvent;
                if let Some(s) = self.swipe.as_mut() {
                    let d = event.delta();
                    s.0 += d.x;
                    s.1 += d.y;
                }
            }
            InputEvent::GestureSwipeEnd { event } => {
                use smithay::backend::input::GestureEndEvent;
                if let Some((dx, dy, fingers)) = self.swipe.take() {
                    if !event.cancelled() && fingers == 3 && self.config.input.touchpad.gestures && !self.locked() {
                        self.on_swipe(dx, dy);
                    }
                }
            }
            InputEvent::SwitchToggle { event } => {
                use smithay::backend::input::{Switch, SwitchState, SwitchToggleEvent};
                if event.switch() == Some(Switch::Lid) && event.state() == SwitchState::On {
                    self.lid_closed();
                }
            }
            _ => {}
        }
    }

    /// The monitor under `pos` becomes the focused one: new windows open there and the workspace keys act on it.
    fn focus_monitor_at(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>) {
        let name = self.space.outputs().find(|o| self.space.output_geometry(o).is_some_and(|g| g.to_f64().contains(pos))).map(|o| o.name());
        let Some(name) = name else { return };
        if name != self.layout.focused_name() && self.layout.focus_output(&name) {
            if !self.keyboard_on_layer() {
                let focused = self.layout.focused_window().cloned();
                self.focus_window(focused);
            }
            self.ipc_broadcast();
        }
    }

    fn move_pointer(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>, time: u32) {
        let pointer = self.seat.get_pointer().unwrap();
        if self.drag.is_none() && !self.locked() {
            self.focus_monitor_at(pos);
        }
        let under = self.surface_under(pos);
        if self.config.input.focus_follows_mouse && self.drag.is_none() && !self.locked() && !self.keyboard_on_layer() {
            self.focus_under(pos, under.as_ref().is_some_and(|(s, _)| self.is_layer_surface(s)));
        }
        pointer.motion(self, under, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    /// Focus-follows-mouse: give focus to the window under `pos` (without scrolling the layout).
    fn focus_under(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>, on_layer: bool) {
        if on_layer {
            return;
        }
        let Some((w, _)) = self.window_under(pos) else { return };
        if crate::xwayland::is_override_redirect(&w) {
            return;
        }
        if self.layout.focused_window() == Some(&w) {
            return;
        }
        // A window still sliding away after a workspace switch is under the pointer for a moment: hovering it must
        // not pull the layout back to the workspace just left.
        if !self.layout.is_on_shown_workspace(&w) {
            return;
        }
        self.layout.focus_window(&w);
        let focused = self.layout.focused_window().cloned();
        self.focus_window(focused);
        self.ipc_broadcast();
    }

    fn on_keyboard<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let key_state = event.state();
        let code = event.key_code();
        let keyboard = self.seat.get_keyboard().unwrap();

        let action = keyboard.input::<Action, _>(self, code, key_state, serial, time, |state, mods, handle| {
            let raw = code.raw();
            // push to talk: learning a key, or the configured key (swallowed: down opens the microphone, up closes it)
            if state.ptt_learning || state.ptt_sym.is_some() || state.ptt_code.is_some() {
                let sym = handle.modified_sym().raw();
                if key_state == KeyState::Pressed {
                    tracing::info!("key pressed: code {} (xkb {raw}), keysym {sym:#x}", raw.saturating_sub(8));
                }
                if state.ptt_learning {
                    return FilterResult::Intercept(if key_state == KeyState::Pressed { Action::PttLearn(raw, sym) } else { Action::None });
                }
                if state.ptt_code == Some(raw) || state.ptt_sym == Some(lower(sym)) {
                    return FilterResult::Intercept(if key_state == KeyState::Released { Action::PttUp } else { Action::PttDown });
                }
            }
            if key_state == KeyState::Released {
                return match state.suppressed_keys.iter().position(|k| *k == raw) {
                    Some(i) => {
                        state.suppressed_keys.remove(i);
                        FilterResult::Intercept(Action::None)
                    }
                    None => FilterResult::Forward,
                };
            }
            // while the switcher is open its own Tab key moves the selection: let the key through
            if state.switcher_running() && matches!(lower(handle.modified_sym().raw()), keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab) {
                return FilterResult::Forward;
            }
            let syms: Vec<u32> = std::iter::once(handle.modified_sym()).chain(handle.raw_syms().iter().copied()).map(|k| lower(k.raw())).collect();
            match state.binding(mods, &syms) {
                Some(action) => {
                    state.suppressed_keys.push(raw);
                    FilterResult::Intercept(action)
                }
                None => FilterResult::Forward,
            }
        });

        if let Some(action) = action {
            self.run_action(action);
        }
    }

    /// `syms`: the key as typed (Shift applied) first, then the key without modifiers.
    fn binding(&self, mods: &ModifiersState, syms: &[u32]) -> Option<Action> {
        let sym = syms[0];
        // Volume, brightness and media keys work with or without a modifier.
        let media = match sym {
            keysyms::KEY_XF86AudioRaiseVolume => Some(Media::VolumeUp),
            keysyms::KEY_XF86AudioLowerVolume => Some(Media::VolumeDown),
            keysyms::KEY_XF86AudioMute => Some(Media::VolumeMute),
            keysyms::KEY_XF86AudioMicMute => Some(Media::MicMute),
            keysyms::KEY_XF86MonBrightnessUp => Some(Media::BrightnessUp),
            keysyms::KEY_XF86MonBrightnessDown => Some(Media::BrightnessDown),
            keysyms::KEY_XF86AudioPlay | keysyms::KEY_XF86AudioPause => Some(Media::PlayPause),
            keysyms::KEY_XF86AudioNext => Some(Media::Next),
            keysyms::KEY_XF86AudioPrev => Some(Media::Previous),
            _ => None,
        };
        if let Some(m) = media {
            return Some(Action::Media(m));
        }
        // Virtual terminal switching works everywhere, so you can always get back to tty1.
        if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&sym) {
            return Some(Action::VtSwitch((sym - keysyms::KEY_XF86Switch_VT_1 + 1) as i32));
        }
        if self.locked() || self.greeter {
            return None; // nothing else works while the screen is locked, or at the login screen
        }
        if sym == keysyms::KEY_XF86PowerOff {
            return Some(Action::PowerKey);
        }
        if let Some(a) = self.user_binding(mods, syms) {
            return Some(a);
        }
        let modifier = if self.uses_super() { mods.logo } else { mods.alt };
        if !modifier || mods.ctrl {
            return None;
        }
        let shift = mods.shift;
        let a = match sym {
            k @ (keysyms::KEY_1..=keysyms::KEY_9) if !shift => Action::WorkspaceTo((k - keysyms::KEY_1) as usize),
            keysyms::KEY_0 if !shift => Action::WorkspaceTo(9),
            k @ (keysyms::KEY_exclam | keysyms::KEY_at | keysyms::KEY_numbersign | keysyms::KEY_dollar | keysyms::KEY_percent | keysyms::KEY_asciicircum | keysyms::KEY_ampersand | keysyms::KEY_asterisk | keysyms::KEY_parenleft | keysyms::KEY_parenright) => {
                let order = [keysyms::KEY_exclam, keysyms::KEY_at, keysyms::KEY_numbersign, keysyms::KEY_dollar, keysyms::KEY_percent, keysyms::KEY_asciicircum, keysyms::KEY_ampersand, keysyms::KEY_asterisk, keysyms::KEY_parenleft, keysyms::KEY_parenright];
                Action::MoveToWorkspaceTo(order.iter().position(|s| *s == k).unwrap_or(0))
            }
            _ => return None,
        };
        Some(a)
    }

    pub(crate) fn run_action(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::PluginKey(p, k) => self.run_plugin_key(p, k),
            Action::Spawn => {
                match self.config.terminal_command() {
                    Some(cmd) => self.spawn(&cmd),
                    None => tracing::warn!("no terminal found; set `terminal` in the config"),
                }
            }
            Action::CloseWindow => {
                if let Some(w) = self.layout.focused_window() {
                    Self::close_window(w);
                }
            }
            Action::FocusColumn(d) => {
                self.layout.focus_column(d);
                self.layout_changed();
                self.warp_pointer_to_focused();
            }
            Action::MoveColumn(d) => {
                self.layout.move_column(d);
                self.layout_changed();
            }
            Action::Workspace(d) => {
                self.with_workspace_slide(|s| s.layout.switch_workspace(d));
                self.warp_pointer_to_focused();
            }
            Action::WorkspaceTo(n) => {
                let d = n.min(self.layout.workspaces.len() - 1) as isize - self.layout.active as isize;
                if d != 0 {
                    self.run_action(Action::Workspace(d));
                }
            }
            Action::MoveToWorkspaceTo(n) => {
                let d = n.min(self.layout.workspaces.len() - 1) as isize - self.layout.active as isize;
                if d != 0 {
                    self.run_action(Action::MoveToWorkspace(d));
                }
            }
            Action::MonitorFocus(d) => {
                if let Some(name) = self.monitor_neighbor(d) {
                    self.focus_monitor(&name);
                }
            }
            Action::MonitorSend(d) => {
                if let (Some(name), Some(w)) = (self.monitor_neighbor(d), self.layout.focused_window().cloned()) {
                    if self.layout.move_window_to(&w, &name) {
                        self.layout_changed();
                        self.warp_pointer_to_output(&name);
                    }
                }
            }
            Action::MoveToWorkspace(d) => {
                self.with_workspace_slide(|s| s.layout.move_to_workspace(d));
            }
            Action::CycleWidth => {
                let d = self.config.layout.column_width;
                self.layout.cycle_width(d);
                self.layout_changed();
            }
            Action::AdjustWidth(delta) => {
                let d = self.config.layout.column_width;
                self.layout.adjust_width(d, delta);
                self.layout_changed();
            }
            Action::Launcher => self.toggle_launcher(),
            Action::Clipboard => self.toggle_clipboard_picker(),
            Action::Emoji => self.toggle_launcher_mode("emoji"),
            Action::Switcher => Self::toggle_child(&mut self.switcher, "vesper-switcher"),
            Action::Lock => self.start_lock(),
            Action::Wallpapers => self.toggle_wallpaper_picker(),
            Action::Capture(Cap::Screen) => self.request_capture(crate::screenshot::CaptureKind::Screen),
            Action::Capture(Cap::Window) => self.request_capture(crate::screenshot::CaptureKind::Window),
            Action::Capture(Cap::Region) => self.request_capture(crate::screenshot::CaptureKind::Freeze(crate::screenshot::Pick::Shot)),
            Action::Media(key) => {
                let m = &self.config.media;
                let cmd = match key {
                    Media::VolumeUp => &m.volume_up,
                    Media::VolumeDown => &m.volume_down,
                    Media::VolumeMute => &m.volume_mute,
                    Media::MicMute => &m.mic_mute,
                    Media::BrightnessUp => &m.brightness_up,
                    Media::BrightnessDown => &m.brightness_down,
                    Media::PlayPause => &m.play_pause,
                    Media::Next => &m.next,
                    Media::Previous => &m.previous,
                }
                .clone();
                // volume boost: the up key may go to 150% instead of stopping at 100%
                let cmd = if self.config.media.volume_boost && key == Media::VolumeUp { cmd.replace("-l 1.0", "-l 1.5") } else { cmd };
                if !cmd.trim().is_empty() {
                    self.spawn(&cmd);
                }
                // tell the bar, so it can show the new volume or brightness on screen
                let kind = match key {
                    Media::VolumeUp | Media::VolumeDown | Media::VolumeMute => Some("volume"),
                    Media::BrightnessUp | Media::BrightnessDown => Some("brightness"),
                    _ => None,
                };
                if let Some(kind) = kind {
                    self.osd_seq = self.osd_seq.wrapping_add(1);
                    self.osd_kind = kind.to_string();
                    self.ipc_broadcast();
                }
            }
            Action::Settings => self.open_settings(None),
            Action::ToggleBlur => self.toggle_blur_focused(),
            Action::ConsumeLeft => {
                self.layout.consume_left();
                self.layout_changed();
            }
            Action::Expel => {
                self.layout.expel();
                self.layout_changed();
            }
            Action::FocusInColumn(d) => {
                self.layout.focus_in_column(d);
                self.layout_changed();
                self.warp_pointer_to_focused();
            }
            Action::Notifications => Self::run_component("vesper-notify", &["center"]),
            Action::PttDown => {
                tracing::info!("push to talk: key down");
                if !self.ptt_held {
                    self.ptt_held = true;
                    self.ptt_down_at = Some(std::time::Instant::now());
                    self.ipc_broadcast(); // the bar shows the live microphone before the slower sound-server call returns
                    Self::ptt_set_mic(false);
                }
            }
            Action::PttLearn(raw, sym) => {
                self.ptt_learning = false;
                if sym == keysyms::KEY_Escape {
                    Self::run_component("vesper-notify", &["send", "Push to talk", "Cancelled, the key is unchanged"]);
                } else {
                    // a physical key code is the most reliable way to name a key
                    let code = raw.saturating_sub(8);
                    let name = smithay::input::keyboard::xkb::keysym_get_name(smithay::input::keyboard::xkb::Keysym::new(sym));
                    let _ = vesper_config::Config::set("media.ptt_key", format!("code:{code}").into());
                    let _ = vesper_config::Config::set("media.ptt", true.into());
                    Self::run_component("vesper-notify", &["send", "Push-to-talk key set", &format!("Key code {code} ({name}). Hold it to talk.")]);
                }
            }
            Action::PttUp => {
                let held = self.ptt_down_at.take().map(|t| t.elapsed().as_millis());
                tracing::info!("push to talk: key up after {held:?} ms");
                self.ptt_held = false;
                self.ipc_broadcast(); // the bar hides the microphone before the sound-server call returns
                Self::ptt_set_mic(true);
            }
            Action::StartMenu => {
                self.start_menu_seq = self.start_menu_seq.wrapping_add(1);
                self.ipc_broadcast();
            }
            Action::PowerMenu => self.toggle_power_menu(),
            Action::PowerKey => match self.config.power.power_key.as_str() {
                "suspend" => self.suspend_now(),
                "poweroff" => {
                    let c = self.config.power.poweroff();
                    self.spawn(&c);
                }
                "ignore" => {}
                _ => self.toggle_power_menu(),
            },
            Action::Files => self.open_files(),
            Action::ToggleFloating => self.toggle_float_workspace(),
            Action::ToggleFloatingWindow => {
                let rect = self.default_float_rect();
                self.layout.toggle_floating(rect);
                self.layout_changed();
            }
            Action::Minimize => {
                if self.layout.minimize_focused() {
                    self.layout_changed();
                }
            }
            Action::RestoreLast => {
                if self.layout.restore_last().is_some() {
                    self.layout_changed();
                }
            }
            Action::ToggleFullscreen => {
                self.layout.toggle_fullscreen();
                self.layout_changed();
            }
            Action::Quit => {
                tracing::info!("quit requested");
                self.loop_signal.stop();
            }
            Action::VtSwitch(vt) => {
                if let Some(f) = &self.vt_switch {
                    tracing::info!("switching to VT {vt}");
                    f(vt);
                }
            }
        }
    }

    fn on_button<I: InputBackend>(&mut self, event: I::PointerButtonEvent) {
        let pointer = self.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        let state = event.state();
        let code = event.button_code();
        if self.drag.is_some() && state == ButtonState::Released {
            if self.drag.as_ref().is_some_and(|d| d.resize) {
                self.drag_force = true;
                self.drag_to(pointer.current_location(), event.time_msec());
                self.drag_force = false;
            }
            let drag = self.drag.take(); // the press was ours, so the release is too
            crate::layout::set_dragged(None);
            if let Some(d) = drag.as_ref().filter(|d| d.tiled && d.resize && d.left) {
                // keep the right edge where it is: fold the shift into the scroll
                let _ = d;
                self.layout.ws_mut().scroll += self.resize_grown;
            }
            match drag {
                Some(d) if d.tiled && !d.resize => self.drop_tiled(&d.window, pointer.current_location()),
                Some(d) if !d.tiled && !d.resize => {
                    // a floating window dropped on another monitor changes monitor (and stays floating there)
                    if let Some(name) = self.other_monitor_at(pointer.current_location()) {
                        self.move_floating_to(&d.window, &name);
                    }
                    self.layout_changed();
                }
                _ => self.layout_changed(),
            }
            return;
        }
        if self.locked() {
            pointer.button(self, &ButtonEvent { button: code, state, serial, time: event.time_msec() });
            pointer.frame(self);
            return;
        }
        if state == ButtonState::Pressed && (code == BTN_LEFT || code == BTN_RIGHT) && self.mod_held() {
            let under = self.window_under(pointer.current_location()).map(|(w, _)| w);
            if let Some(window) = under.filter(|w| self.layout.is_floating(w)) {
                self.layout.focus_window(&window);
                self.layout_changed();
                if let Some(rect) = self.layout.float_rect_mut(&window).map(|r| *r) {
                    self.drag = Some(Drag { window, resize: code == BTN_RIGHT, start: pointer.current_location(), rect, tiled: false, left: pointer.current_location().x < (rect.loc.x + rect.size.w / 2) as f64, top: pointer.current_location().y < (rect.loc.y + rect.size.h / 2) as f64, base_shift: self.last_shift });
                    if code == BTN_LEFT {
                        crate::layout::set_dragged(self.drag.as_ref().map(|d| d.window.clone()));
                    }
                }
                return;
            }
            // A tiled column: Mod+left moves it (drop it between other columns), Mod+right changes its width.
            if let Some(window) = self.window_under(pointer.current_location()).map(|(w, _)| w) {
                if !self.layout.is_floating(&window) {
                    self.layout.focus_window(&window);
                    self.layout_changed();
                    if let Some(rect) = self.space.element_geometry(&window) {
                        self.drag = Some(Drag { window: window.clone(), resize: code == BTN_RIGHT, start: pointer.current_location(), rect, tiled: true, left: pointer.current_location().x < (rect.loc.x + rect.size.w / 2) as f64, top: pointer.current_location().y < (rect.loc.y + rect.size.h / 2) as f64, base_shift: self.last_shift });
                        if code == BTN_LEFT {
                            crate::layout::set_dragged(self.drag.as_ref().map(|d| d.window.clone()));
                        }
                        self.space.raise_element(&window, false);
                    }
                    return;
                }
            }
        }

        if state == ButtonState::Pressed && !pointer.is_grabbed() {
            // A click on a bar, popup or other layer surface must not take the keyboard away from whoever has it:
            // the wallpaper changer closes itself when it loses focus, and clicking the bar should not unfocus your window.
            let on_layer = self.surface_under(pointer.current_location()).is_some_and(|(s, _)| self.is_layer_surface(&s));
            if !on_layer {
                let under = self.window_under(pointer.current_location()).map(|(w, _)| w);
                match under {
                    // clicking a menu or tooltip leaves the focus where it is
                    Some(window) if crate::xwayland::is_override_redirect(&window) => {}
                    // a window still sliding away after a workspace switch is not clickable
                    Some(window) if !self.layout.is_on_shown_workspace(&window) => {}
                    Some(window) => {
                        self.layout.focus_window(&window);
                        self.layout_changed();
                    }
                    None => self.focus_window(None),
                }
            }
        }

        pointer.button(self, &ButtonEvent { button: event.button_code(), state, serial, time: event.time_msec() });
        pointer.frame(self);
    }

    fn on_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        let source = event.source();
        let amount = |axis| {
            event.amount(axis).unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.)
        };
        let flip = source == AxisSource::Finger && self.config.input.touchpad.swap_scroll;
        let mut frame = AxisFrame::new(event.time_msec()).source(source);
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let sign: f64 = if flip && axis == Axis::Vertical { -1.0 } else { 1.0 };
            let value = amount(axis) * sign;
            if value != 0.0 {
                frame = frame.value(axis, value);
                if let Some(d) = event.amount_v120(axis) {
                    frame = frame.v120(axis, (d * sign) as i32);
                }
            } else if source == AxisSource::Finger {
                frame = frame.stop(axis);
            }
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}

impl Vesper {
    /// Is the compositor modifier (Super, or Alt when nested) currently held?
    fn mod_held(&self) -> bool {
        let Some(kb) = self.seat.get_keyboard() else { return false };
        let mods = kb.modifier_state();
        if self.uses_super() { mods.logo } else { mods.alt }
    }

    /// Where a window floats when you first float it: centred, about 60% of the screen.
    /// Is the pan chord down: Mod+Alt (Mod+Ctrl when Alt is the shortcut key)?
    fn canvas_pan_held(&self) -> bool {
        if self.locked() || self.greeter {
            return false;
        }
        let Some(kb) = self.seat.get_keyboard() else { return false };
        let m = kb.modifier_state();
        if self.uses_super() { m.logo && m.alt && !m.ctrl && !m.shift } else { m.alt && m.ctrl && !m.logo && !m.shift }
    }

    /// Move every floating window of the active workspace by `delta` (they may leave the screen).
    fn pan_floating(&mut self, delta: smithay::utils::Point<f64, smithay::utils::Logical>) {
        self.pan_frac.0 += delta.x;
        self.pan_frac.1 += delta.y;
        let (dx, dy) = (self.pan_frac.0.trunc() as i32, self.pan_frac.1.trunc() as i32);
        if dx == 0 && dy == 0 {
            return;
        }
        self.pan_frac.0 -= dx as f64;
        self.pan_frac.1 -= dy as f64;
        let ws = self.layout.ws_mut();
        if ws.floating.is_empty() {
            return;
        }
        for (_, rect) in ws.floating.iter_mut() {
            rect.loc = (rect.loc.x + dx, rect.loc.y + dy).into();
        }
        self.relayout();
    }

    /// Mod+V: turn the active workspace into a floating one, or back. On: every window floats at the default floating size,
    /// cascaded so they visibly overlap (the focused one on top); windows that open here float too. Off: they tile again.
    pub fn toggle_float_workspace(&mut self) {
        let n = self.layout.ws().columns.len();
        let rects: Vec<_> = (0..n).map(|i| self.cascaded_float_rect(i)).collect();
        let on = self.layout.toggle_float_workspace(|_, i| rects[i]);
        tracing::info!("floating workspace {}", if on { "on" } else { "off" });
        self.layout_changed();
        self.dump_scene("floating workspace toggled");
    }

    /// The default floating rectangle, moved down and right a little for each window already floating (`n`).
    pub fn cascaded_float_rect(&self, n: usize) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        let mut r = self.default_float_rect();
        let step = (n % 6) as i32 * 32;
        r.loc = (r.loc.x + step - 80, r.loc.y + step - 80).into();
        r
    }

    pub fn default_float_rect(&self) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        let geo = self.output_geometry().unwrap_or_else(|| smithay::utils::Rectangle::from_size((1280, 720).into()));
        let (w, h) = (geo.size.w * 6 / 10, geo.size.h * 65 / 100);
        smithay::utils::Rectangle::new((geo.loc.x + (geo.size.w - w) / 2, geo.loc.y + (geo.size.h - h) / 2).into(), (w, h).into())
    }

    /// Move the pointer to `pos` and apply the active Mod+drag to the floating window.
    fn drag_to(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>, time: u32) {
        let Some(drag) = &self.drag else { return };
        let d = pos - drag.start;
        let (window, resize, start, left, top) = (drag.window.clone(), drag.resize, drag.rect, drag.left, drag.top);
        if resize && !self.drag_force && self.last_drag_layout.elapsed() < std::time::Duration::from_millis(12) {
            // too soon since the last re-tile: only move the pointer
            let pointer = self.seat.get_pointer().unwrap();
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        self.last_drag_layout = std::time::Instant::now();
        if drag.tiled {
            if resize {
                // width from the grabbed side; height (only in a stack) from the grabbed top or bottom
                let out_w = self.output_geometry().map_or(1920, |g| g.size.w);
                let gap = self.config.layout.gap;
                let dw = if left { -d.x } else { d.x } as i32;
                let ratio = ((start.size.w + dw + gap) as f64 / (out_w - gap).max(1) as f64).clamp(0.15, 1.0);
                if let Some((_, i)) = self.layout.find_window(&window) {
                    let ws = self.layout.ws();
                    let (s, e) = (ws.col_start(i), ws.col_end(i));
                    self.layout.ws_mut().ratios[s] = ratio;
                    // vertical: move the divider next to the grabbed edge (nothing to move at the top or bottom of a column)
                    let nb = if top { i.checked_sub(1).filter(|n| *n >= s) } else { Some(i + 1).filter(|n| *n <= e) };
                    if let Some(j) = nb {
                        let h = |k: usize| self.space.element_geometry(&self.layout.ws().columns[k]).map_or(0, |g| g.size.h);
                        let pair_px = (h(i) + h(j)).max(1);
                        let dh = if top { -d.y } else { d.y } as i32;
                        let share = ((start.size.h + dh) as f64 / pair_px as f64).clamp(0.12, 0.88);
                        let ws = self.layout.ws_mut();
                        let pair = ws.weights[i] + ws.weights[j];
                        ws.weights[i] = share * pair;
                        ws.weights[j] = pair - ws.weights[i];
                    }
                }
                self.relayout();
            } else {
                self.place_now(window.clone(), (start.loc.x + d.x as i32, start.loc.y + d.y as i32).into());
                self.space.raise_element(&window, false);
                self.drop_hint = self.plan_drop(&window, pos).map(|p| p.rect());
            }
            let pointer = self.seat.get_pointer().unwrap();
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if let Some(rect) = self.layout.float_rect_mut(&window) {
            if resize {
                // drag the edge nearest where you grabbed, like Hyprland
                let (dx, dy) = (d.x as i32, d.y as i32);
                let w = (if left { start.size.w - dx } else { start.size.w + dx }).max(160);
                let h = (if top { start.size.h - dy } else { start.size.h + dy }).max(100);
                rect.size = (w, h).into();
                rect.loc = (if left { start.loc.x + start.size.w - w } else { start.loc.x }, if top { start.loc.y + start.size.h - h } else { start.loc.y }).into();
            } else {
                rect.loc = (start.loc.x + d.x as i32, start.loc.y + d.y as i32).into();
            }
        }
        self.relayout();
        let pointer = self.seat.get_pointer().unwrap();
        pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }
}

impl Vesper {
    /// Where a tiled window dropped at `pos` would end up, and the box that shows it.
    /// The monitor under `pos`, if it is not the one the dragged window is on (the focused one): dropping there moves it.
    fn other_monitor_at(&self, pos: smithay::utils::Point<f64, smithay::utils::Logical>) -> Option<String> {
        let name = self.space.outputs().find(|o| self.space.output_geometry(o).is_some_and(|g| g.to_f64().contains(pos)))?.name();
        (name != self.layout.focused_name() && self.layout.get(&name).is_some()).then_some(name)
    }

    fn plan_drop(&self, window: &smithay::desktop::Window, pos: smithay::utils::Point<f64, smithay::utils::Logical>) -> Option<DropPlan> {
        use smithay::utils::{Logical, Rectangle};
        if self.other_monitor_at(pos).is_some() {
            return None; // no tiling hint over another monitor: the window moves there
        }
        // a portrait monitor lays its windows out top to bottom: the drop zones below assume columns left to right
        if self.output_geometry().is_some_and(|g| g.size.h > g.size.w) {
            return None;
        }
        let gap = self.config.layout.gap;
        let own = self.drag.as_ref().map(|d| d.rect).or_else(|| self.space.element_geometry(window))?;
        let over = self.layout.ws().columns.iter().filter(|w| *w != window).find_map(|w| {
            let g = self.space.element_geometry(w)?;
            g.to_f64().contains(pos).then(|| (w.clone(), g))
        });
        if let Some((target, g)) = over {
            let rel = (pos.x - g.loc.x as f64) / g.size.w.max(1) as f64;
            if (0.2..=0.8).contains(&rel) {
                let below = pos.y > (g.loc.y + g.size.h / 2) as f64;
                let half = g.size.h / 2;
                let rect = Rectangle::<i32, Logical>::new((g.loc.x, if below { g.loc.y + g.size.h - half } else { g.loc.y }).into(), (g.size.w, half).into());
                return Some(DropPlan::Stack { target, below, rect });
            }
            let after = rel > 0.5;
            // the window becomes a column of its own next to the target: show that column, where it will really be
            let (x, y, w, h) = self.new_column_rect(own, if after { g.loc.x + g.size.w + gap } else { g.loc.x });
            return Some(DropPlan::Beside { target, after, rect: Rectangle::new((x, y).into(), (w, h).into()) });
        }
        // free space: between the columns on either side of the pointer (the strip always starts at the left: no anchoring to
        // the right or the middle, dropping only reorders)
        let out = self.output_geometry()?;
        let leaders: Vec<smithay::desktop::Window> = {
            let ws = self.layout.ws();
            (0..ws.columns.len()).filter(|i| !ws.stacked[*i] && ws.columns[*i] != *window).map(|i| ws.columns[i].clone()).collect()
        };
        let geos: Vec<Rectangle<i32, Logical>> = leaders.iter().filter_map(|w| self.space.element_geometry(w)).collect();
        let to = geos.iter().filter(|g| (g.loc.x + g.size.w / 2) as f64 <= pos.x).count();
        // the dragged column takes the place of the column now at slot `to` (which moves right), or goes after the last one
        let at = geos.get(to).map(|g| g.loc.x).or_else(|| geos.last().map(|g| g.loc.x + g.size.w + gap)).unwrap_or(out.loc.x + gap);
        let (x, y, w, h) = self.new_column_rect(own, at);
        Some(DropPlan::Column { to, rect: Rectangle::new((x, y).into(), (w, h).into()) })
    }

    /// The box of a window that gets a column of its own at `x`: its own width and the full height of the tiling area,
    /// kept on the screen. Returns (x, y, w, h).
    fn new_column_rect(&self, own: smithay::utils::Rectangle<i32, smithay::utils::Logical>, x: i32) -> (i32, i32, i32, i32) {
        let gap = self.config.layout.gap;
        let Some(out) = self.focused_output() else { return (x, own.loc.y, own.size.w, own.size.h) };
        let og = self.space.output_geometry(out).unwrap_or_default();
        let zone = smithay::desktop::layer_map_for_output(out).non_exclusive_zone();
        let w = own.size.w.min(og.size.w - 2 * gap).max(60);
        let x = x.clamp(og.loc.x + gap, (og.loc.x + og.size.w - w - gap).max(og.loc.x + gap));
        (x, og.loc.y + zone.loc.y + gap, w, crate::layout::column_height(zone.size.h, gap))
    }

    /// A tiled window was dropped at `pos` (see `plan_drop`).
    fn drop_tiled(&mut self, window: &smithay::desktop::Window, pos: smithay::utils::Point<f64, smithay::utils::Logical>) {
        self.drop_hint = None;
        if let Some(name) = self.other_monitor_at(pos) {
            if self.layout.move_window_to(window, &name) {
                return;
            }
        }
        match self.plan_drop(window, pos) {
            Some(DropPlan::Stack { target, below, .. }) => self.layout.stack_onto(window, &target, below),
            Some(DropPlan::Beside { target, after, .. }) => self.layout.move_beside(window, &target, after),
            Some(DropPlan::Column { to, .. }) => {
                // a stray anchor from before (the strip pinned right or centred) goes away
                self.layout.ws_mut().align = crate::layout::Align::Left;
                self.layout.move_column_to(window, to);
            }
            None => {}
        }
        self.layout_changed();
    }
}

/// What dropping a dragged tiled window would do; `rect` is the box shown while dragging.
pub enum DropPlan {
    Stack { target: smithay::desktop::Window, below: bool, rect: smithay::utils::Rectangle<i32, smithay::utils::Logical> },
    Beside { target: smithay::desktop::Window, after: bool, rect: smithay::utils::Rectangle<i32, smithay::utils::Logical> },
    Column { to: usize, rect: smithay::utils::Rectangle<i32, smithay::utils::Logical> },
}

impl DropPlan {
    pub fn rect(&self) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        match self {
            DropPlan::Stack { rect, .. } | DropPlan::Beside { rect, .. } | DropPlan::Column { rect, .. } => *rect,
        }
    }
}

impl Vesper {
    /// A finished three-finger swipe: up/down switch workspace, left/right move focus between columns.
    fn on_swipe(&mut self, dx: f64, dy: f64) {
        const MIN: f64 = 60.0;
        if dy.abs() >= dx.abs() && dy.abs() >= MIN {
            // swiping up brings the next workspace up from below, like scrolling content
            self.run_action(Action::Workspace(if dy < 0.0 { 1 } else { -1 }));
        } else if dx.abs() >= MIN {
            self.run_action(Action::FocusColumn(if dx < 0.0 { 1 } else { -1 }));
        }
    }
}

/// What a rebindable shortcut (`vesper_config::keys::BINDS`) does.
pub fn action_for(id: &str) -> Option<Action> {
    Some(match id {
        "terminal" => Action::Spawn,
        "launcher" => Action::Launcher,
        "files" => Action::Files,
        "settings" => Action::Settings,
        "blur" => Action::ToggleBlur,
        "wallpapers" => Action::Wallpapers,
        "notifications" => Action::Notifications,
        "power_menu" => Action::PowerMenu,
        "close" => Action::CloseWindow,
        "focus_left" => Action::FocusColumn(-1),
        "focus_right" => Action::FocusColumn(1),
        "workspace_up" => Action::Workspace(-1),
        "workspace_down" => Action::Workspace(1),
        "move_left" => Action::MoveColumn(-1),
        "move_right" => Action::MoveColumn(1),
        "send_up" => Action::MoveToWorkspace(-1),
        "send_down" => Action::MoveToWorkspace(1),
        "stack" => Action::ConsumeLeft,
        "unstack" => Action::Expel,
        "column_up" => Action::FocusInColumn(-1),
        "column_down" => Action::FocusInColumn(1),
        "cycle_width" => Action::CycleWidth,
        "narrower" => Action::AdjustWidth(-0.05),
        "wider" => Action::AdjustWidth(0.05),
        "fullscreen" => Action::ToggleFullscreen,
        "float" => Action::ToggleFloating,
        "float_window" => Action::ToggleFloatingWindow,
        "monitor_left" => Action::MonitorFocus(-1),
        "monitor_right" => Action::MonitorFocus(1),
        "send_monitor_left" => Action::MonitorSend(-1),
        "send_monitor_right" => Action::MonitorSend(1),
        "minimize" => Action::Minimize,
        "restore" => Action::RestoreLast,
        "lock" => Action::Lock,
        "start_menu" => Action::StartMenu,
        "switcher" => Action::Switcher,
        "clipboard" => Action::Clipboard,
        "emoji" => Action::Emoji,
        "shot_screen" => Action::Capture(Cap::Screen),
        "shot_window" => Action::Capture(Cap::Window),
        "shot_region" => Action::Capture(Cap::Region),
        "quit" => Action::Quit,
        _ => return None,
    })
}

/// The keysym a key name stands for, lowercased: a single character is itself, anything else is looked up by xkb name.
fn key_sym(name: &str) -> Option<u32> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii() && !c.is_ascii_control() {
            return Some(lower(c as u32));
        }
    }
    let sym = smithay::input::keyboard::xkb::keysym_from_name(name, smithay::input::keyboard::xkb::KEYSYM_CASE_INSENSITIVE).raw();
    (sym != 0).then(|| lower(sym))
}

impl Vesper {
    /// The first rebindable shortcut whose keys are down (exact modifiers).
    fn user_binding(&self, mods: &ModifiersState, syms: &[u32]) -> Option<Action> {
        let sup = self.uses_super();
        for bind in vesper_config::keys::BINDS {
            for c in vesper_config::keys::effective(bind.id, &self.config.keys) {
                let (logo, alt) = (c.logo || (c.modk && sup), c.alt || (c.modk && !sup));
                if mods.logo != logo || mods.alt != alt || mods.ctrl != c.ctrl || mods.shift != c.shift {
                    continue;
                }
                if key_sym(&c.key).is_some_and(|k| syms.contains(&k)) {
                    return action_for(bind.id);
                }
            }
        }
        for (pi, p) in self.plugins.active.iter().enumerate().filter(|(_, p)| p.can("keys")) {
            for (ki, k) in p.manifest.key.iter().enumerate() {
                let id = vesper_config::plugins::key_id(&p.id, &k.id);
                let mine = self.config.keys.get(&id).map(|s| vesper_config::keys::parse_list(s)).unwrap_or_default();
                let combos = if mine.is_empty() { vesper_config::keys::parse_list(&k.default) } else { mine };
                for c in combos {
                    let (logo, alt) = (c.logo || (c.modk && sup), c.alt || (c.modk && !sup));
                    if mods.logo == logo && mods.alt == alt && mods.ctrl == c.ctrl && mods.shift == c.shift && key_sym(&c.key).is_some_and(|s| syms.contains(&s)) {
                        return Some(Action::PluginKey(pi, ki));
                    }
                }
            }
        }
        None
    }
}

impl Vesper {
    /// A new window opens on the active workspace: in a column, or floating if the workspace is in floating mode.
    pub fn add_window_here(&mut self, window: smithay::desktop::Window) {
        self.layout.add_window(window.clone());
        if self.layout.ws().float_mode {
            let rect = self.cascaded_float_rect(self.layout.ws().floating.len());
            self.layout.float_by_mode(&window, rect);
        }
    }
}

impl Vesper {
    /// Move a floating `window` to monitor `name`, keeping it floating where it was dropped (pulled inside that monitor).
    fn move_floating_to(&mut self, window: &smithay::desktop::Window, name: &str) {
        let Some(mut rect) = self.space.element_geometry(window) else { return };
        if let Some(og) = self.space.outputs().find(|o| o.name() == name).and_then(|o| self.space.output_geometry(o)) {
            rect.loc.x = rect.loc.x.clamp(og.loc.x, (og.loc.x + og.size.w - rect.size.w).max(og.loc.x));
            rect.loc.y = rect.loc.y.clamp(og.loc.y, (og.loc.y + og.size.h - rect.size.h).max(og.loc.y));
        }
        if self.layout.move_window_to(window, name) {
            self.layout.float_window(window, rect);
        }
    }
}
