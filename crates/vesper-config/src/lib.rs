//! Vesper configuration.
//!
//! Lookup order: `$VESPER_CONFIG`, `$XDG_CONFIG_HOME/vesper/config.toml`,
//! `/etc/vesper/config.toml`, then built-in defaults. Every field is optional.

pub mod plugins;
pub mod profiles;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod theme;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Command started by the "spawn terminal" binding. Empty = first of foot, kitty, alacritty, wezterm, xterm found in PATH.
    pub terminal: String,
    /// Commands started once when the session comes up.
    pub autostart: Vec<String>,
    pub layout: Layout,
    pub input: Input,
    pub media: Media,
    pub wallpaper: Wallpaper,
    pub greeter: Greeter,
    pub idle: Idle,
    pub lock: Lock,
    pub screenshots: Screenshots,
    pub shell: Shell,
    pub bar: Bar,
    pub appearance: Appearance,
    pub animations: Animations,
    pub blur: Blur,
    pub display: Display,
    pub power: Power,
    pub notifications: Notifications,
    pub apps: Apps,
    pub launcher: Launcher,
    /// Per-app window rules (first match wins), as `[[rules]]` tables.
    pub rules: Vec<WindowRule>,
    /// Plugins you have switched on (see `plugins` and Settings > Plugins).
    pub plugins: Plugins,
}

/// Which plugins are switched on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Plugins {
    /// Folder names of the enabled plugins.
    pub enabled: Vec<String>,
}

/// A window rule: windows whose app id (and title, if given) contain these texts get the settings below when they open.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowRule {
    /// Part of the app id, case-insensitive; empty matches any.
    pub app_id: String,
    /// Part of the title, case-insensitive; empty matches any. (Only checked when the window opens.)
    pub title: String,
    /// Open it floating (centred).
    pub float: bool,
    /// Floating size in pixels (0 = the usual size).
    pub width: u32,
    pub height: u32,
    /// Open it on this workspace (1 to 10), without switching to it; 0 = the current one.
    pub workspace: usize,
}

impl WindowRule {
    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        let has = |hay: &str, needle: &str| needle.is_empty() || hay.to_lowercase().contains(&needle.to_lowercase());
        (!self.app_id.is_empty() || !self.title.is_empty()) && has(app_id, &self.app_id) && has(title, &self.title)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Layout {
    /// Gap between columns and around the screen edge, in logical pixels.
    pub gap: i32,
    /// New column width as a fraction of the output width.
    pub column_width: f64,
    /// Number of workspaces.
    pub workspaces: usize,
    /// Workspaces in use take the first places: unused ones move to the end a moment after you leave them, so three
    /// busy workspaces are always 1 to 3.
    pub dynamic_workspaces: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shell {
    /// Keep a short history of copied text for the clipboard picker (Mod+Shift+V), in the runtime directory.
    pub clipboard_history: bool,
    /// Start `vesper-bar` with the session.
    pub bar: bool,
    /// Start `vesper-wallpaper` (only does anything if `[wallpaper] path` is set).
    pub wallpaper: bool,
    /// Start `vesper-notify` (the notification daemon) with the session.
    pub notifications: bool,
    /// Run Xwayland so X11 apps (Steam, older Electron/Java apps, games) work; `DISPLAY` is set for spawned apps.
    pub xwayland: bool,
    /// Start `vesper-polkit`, the polkit authentication agent (password dialog for pkexec and admin prompts).
    pub polkit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bar {
    pub height: u32,
    pub font: String,
    pub font_size: f32,
    /// Size of minimized-app icons in the bar centre; 0 = derive from the bar height.
    pub icon_size: u32,
    /// Look of the bar: "fill" (full width, border on top and bottom), "detached" (floating box with a border all round),
    /// "notch" (a centred tab that curves into the top of the screen) or "islands" (separate left, middle and right boxes).
    pub style: String,
    /// Border thickness in pixels (0 = none).
    pub border_width: i32,
    /// Border colour as #rrggbb[aa]; empty = the wallpaper accent.
    pub border_color: String,
    /// Make the border as see-through as the bar (it follows `opacity`).
    pub border_blend: bool,
    /// How workspaces are drawn: "dots" (pills and dots), "halo" (filled shapes with a soft ring round each) or "numbers".
    pub workspace_style: String,
    /// Colour of the workspace indicators as #rrggbb; empty = the wallpaper accent.
    pub workspace_color: String,
    /// "all" shows every workspace, "occupied" only those with windows (and the current one).
    pub workspace_mode: String,
    /// Places where the bar is split (see hovering between icons): the id of the icon to the left of each split.
    pub splits: Vec<String>,
    /// Look of a split: "line", "dots" or "dashes".
    pub split_style: String,
    /// Thickness of the split mark in pixels.
    pub split_width: i32,
    /// Height of the split mark as a percentage of the bar height.
    pub split_height: i32,
    /// Colour of the split mark as #rrggbb[aa]; empty = muted outline colour.
    pub split_color: String,
    /// Extra room kept on each side of a split, in pixels.
    pub split_spacing: i32,
    /// Battery and wifi: "ios" (a phone-style battery with the percentage inside and a fan-shaped wifi symbol) or "glyphs".
    pub status_icons: String,
    /// The system icon on the right: "auto" (your distribution's logo), "none", "glyph:arch" (a font logo), the name of a logo
    /// found on this system, or the path of any .svg/.png file.
    pub system_icon: String,
    /// Size in pixels; 0 = the same as `item_icon_size`.
    pub system_icon_size: u32,
    /// Colour the icon with the wallpaper accent (then `system_icon_color` is ignored).
    pub system_icon_accent: bool,
    /// Recolour the icon as #rrggbb; empty = its own colours.
    pub system_icon_color: String,
    /// Colour of inactive workspaces as #rrggbb[aa]; empty = the usual muted look.
    pub workspace_inactive_color: String,
    /// No longer used (replaced by `workspace_inactive_color`); still accepted so old config files load.
    pub workspace_inactive_border: bool,
    /// Corner radius of detached, notch and island shapes.
    pub radius: i32,
    /// Gap between the screen top and a detached or islands bar.
    pub margin_top: i32,
    /// Gap between the screen sides and a detached or islands bar.
    pub margin_side: i32,
    /// Width of the notch.
    pub notch_width: i32,
    /// Bar background opacity in percent (100 = solid).
    pub opacity: u32,
    /// Blur the wallpaper behind the bar (needs opacity below 100).
    pub blur: bool,
    /// 1 (light) to 100 (heavy).
    pub blur_strength: u32,
    /// Items on the right, drawn right to left in this order: "media", "tray", "keyboard", "brightness", "volume", "network", "battery", "clock", "bell", "system", "workspaces", "minimized". An item listed in several sections is drawn in the first (right, then left, then middle). Missing ones are hidden.
    pub right_items: Vec<String>,
    /// Items at the left edge, left to right: "system", "workspaces", or any item from `right_items`.
    pub left_items: Vec<String>,
    /// Items in the middle, left to right: "minimized" (apps minimized from this workspace) or any other item.
    pub middle_items: Vec<String>,
    /// 24-hour clock (false = 12-hour with am/pm).
    pub clock_24h: bool,
    /// Show seconds in the clock.
    pub clock_seconds: bool,
    /// Show the date before the time.
    pub clock_date: bool,
    /// Show the focused window's title in the middle of the bar.
    pub show_title: bool,
    /// Show a small popup when the volume or brightness changes (with the keys or by scrolling in the bar).
    pub osd: bool,
    /// Size of the bar's icons (battery, wifi, volume, bell, power, brightness, system, tray) in pixels.
    pub item_icon_size: u32,
}

impl Default for Shell {
    fn default() -> Self {
        Self { clipboard_history: true, bar: true, notifications: true, wallpaper: true, xwayland: true, polkit: true }
    }
}

impl Default for Bar {
    fn default() -> Self {
        Self { height: 32, font: "JetBrainsMono Nerd Font".into(), font_size: 14.0, icon_size: 0, style: "fill".into(), border_width: 1, border_color: String::new(), border_blend: false, workspace_style: "dots".into(), workspace_color: String::new(), workspace_mode: "all".into(), splits: Vec::new(), split_style: "line".into(), split_width: 2, split_height: 55, split_color: String::new(), split_spacing: 10, status_icons: "ios".into(), system_icon: "auto".into(), system_icon_size: 0, system_icon_accent: false, system_icon_color: String::new(), workspace_inactive_color: String::new(), workspace_inactive_border: false, radius: 14, margin_top: 8, margin_side: 12, notch_width: 560, opacity: 100, blur: false, blur_strength: 30, right_items: ["mic", "drives", "volume", "network", "battery", "clock", "bell"].iter().map(|s| s.to_string()).collect(), left_items: ["system", "workspaces"].iter().map(|s| s.to_string()).collect(), middle_items: vec!["minimized".to_string()], clock_24h: true, clock_seconds: false, clock_date: true, show_title: false, osd: true, item_icon_size: 20 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Input {
    /// Modifier for compositor bindings: "super" or "alt".
    /// `None` = "super" on a real session, "alt" when nested in another compositor.
    pub modifier: Option<String>,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
    /// Focus the window under the pointer without clicking.
    pub focus_follows_mouse: bool,
    /// Move the pointer to the middle of the window when keys (Mod+arrows) change the focus.
    pub warp_mouse_on_focus: bool,
    pub xkb_layout: String,
    pub xkb_variant: String,
    pub xkb_options: String,
    pub touchpad: Touchpad,
    pub mouse: Mouse,
}

/// Touchpad settings, applied to every touchpad as it appears.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Touchpad {
    /// Three-finger swipes: up/down change workspace, left/right move focus between columns.
    pub gestures: bool,
    /// Tap to click.
    pub tap: bool,
    pub natural_scroll: bool,
    /// -1.0 (slowest) to 1.0 (fastest).
    pub accel_speed: f64,
    pub disable_while_typing: bool,
    /// "clickfinger" (1, 2, 3 fingers = left, right, middle), "buttonareas", or "" for the device default.
    pub click_method: String,
    pub middle_emulation: bool,
    /// Swap up and down when scrolling with two fingers (done by the compositor, so it works on every touchpad).
    pub swap_scroll: bool,
}

/// Mouse settings (other pointing devices).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mouse {
    pub natural_scroll: bool,
    pub accel_speed: f64,
    /// "adaptive" (default) or "flat" (no acceleration curve).
    pub accel_profile: String,
}

/// Where screenshots go.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Screenshots {
    pub dir: String,
    /// Language(s) for "copy text from the screen", as tesseract names them ("eng", "eng+deu", ...). Needs `tesseract` installed.
    pub ocr_lang: String,
    /// Let apps share a single window (not just a whole monitor) through the screen-sharing portal. Applies after a restart.
    pub share_windows: bool,
}

impl Default for Screenshots {
    fn default() -> Self {
        Self { dir: "~/Pictures/Screenshots".into(), ocr_lang: "eng".into(), share_windows: true }
    }
}

/// Lock screen options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Lock {
    /// The "Switch user" button: "auto" (shown when a display manager such as SDDM or LightDM is running),
    /// "never", or your own command.
    pub switch_user: String,
}

impl Default for Lock {
    fn default() -> Self {
        Self { switch_user: "auto".into() }
    }
}

/// Idle behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Idle {
    /// Lock the screen after this many seconds without input; 0 = never.
    pub lock_after: u64,
}

impl Default for Idle {
    fn default() -> Self {
        Self { lock_after: 300 }
    }
}

/// Desktop background.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Wallpaper {
    /// Image file (png, jpeg, webp). Empty = no wallpaper client.
    pub path: String,
    /// "fill" (cover and crop), "fit" (letterbox), "stretch", or "center".
    pub mode: String,
    /// Colour behind the image, as `#rrggbb`.
    pub color: String,
    /// Folder the wallpaper changer (Mod+W) offers.
    pub dir: String,
}

impl Default for Wallpaper {
    fn default() -> Self {
        Self { path: String::new(), mode: "fill".into(), color: "#14141a".into(), dir: "~/Pictures/Wallpapers".into() }
    }
}

/// The login screen (`vesper-greeter`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Greeter {
    /// Background image; empty = the desktop wallpaper (`[wallpaper] path`).
    pub background: String,
    /// How strongly the background is blurred, 0 (sharp) to 100.
    pub blur: u32,
    /// How much the background is darkened, as a percentage (0 to 90), so the text stays readable.
    pub dim: u32,
}

impl Default for Greeter {
    fn default() -> Self {
        Self { background: String::new(), blur: 40, dim: 35 }
    }
}

/// Commands run by the media, volume and brightness keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Media {
    pub volume_up: String,
    pub volume_down: String,
    pub volume_mute: String,
    pub mic_mute: String,
    /// Push to talk: the microphone stays muted (also at startup) and is only live while `ptt_key` is held.
    pub ptt: bool,
    /// The push-to-talk key as an xkb key name; the default is the microphone button (F4 on many laptops).
    pub ptt_key: String,
    /// Let the volume go up to 150% (the keys, the slider and scrolling in the bar).
    pub volume_boost: bool,
    pub brightness_up: String,
    pub brightness_down: String,
    pub play_pause: String,
    pub next: String,
    pub previous: String,
}

impl Default for Touchpad {
    fn default() -> Self {
        Self { gestures: true, tap: true, natural_scroll: false, accel_speed: 0.0, disable_while_typing: true, click_method: "clickfinger".into(), middle_emulation: false, swap_scroll: true }
    }
}

impl Default for Mouse {
    fn default() -> Self {
        Self { natural_scroll: false, accel_speed: 0.0, accel_profile: "adaptive".into() }
    }
}

impl Default for Media {
    fn default() -> Self {
        let s = |x: &str| x.to_owned();
        Self {
            volume_up: s("wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+"),
            volume_down: s("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"),
            volume_mute: s("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"),
            mic_mute: s("wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle"),
            ptt: false,
            ptt_key: s("XF86AudioMicMute"),
            volume_boost: false,
            brightness_up: s("brightnessctl set 5%+"),
            brightness_down: s("brightnessctl set 5%-"),
            play_pause: s("playerctl play-pause"),
            next: s("playerctl next"),
            previous: s("playerctl previous"),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            terminal: String::new(),
            autostart: Vec::new(),
            layout: Layout::default(),
            input: Input::default(),
            media: Media::default(),
            wallpaper: Wallpaper::default(),
            greeter: Greeter::default(),
            idle: Idle::default(),
            lock: Lock::default(),
            screenshots: Screenshots::default(),
            shell: Shell::default(),
            bar: Bar::default(),
            appearance: Appearance::default(),
            animations: Animations::default(),
            blur: Blur::default(),
            display: Display::default(),
            power: Power::default(),
            notifications: Notifications::default(),
            apps: Apps::default(),
            launcher: Launcher::default(),
            rules: Vec::new(),
            plugins: Plugins::default(),
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self { gap: 8, column_width: 0.5, workspaces: 10, dynamic_workspaces: false }
    }
}

impl Default for Input {
    fn default() -> Self {
        Self {
            modifier: None,
            repeat_delay: 300,
            repeat_rate: 30,
            focus_follows_mouse: false,
            warp_mouse_on_focus: false,
            xkb_layout: String::new(),
            xkb_variant: String::new(),
            xkb_options: String::new(),
            touchpad: Touchpad::default(),
            mouse: Mouse::default(),
        }
    }
}

impl Config {
    pub fn candidate_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(p) = std::env::var_os("VESPER_CONFIG") {
            paths.push(PathBuf::from(p));
        }
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        {
            paths.push(dir.join("vesper/config.toml"));
        }
        paths.push(PathBuf::from("/etc/vesper/config.toml"));
        paths
    }

    /// Load the first config file that exists. A file that exists but is invalid is an error
    /// we report and then ignore, so a typo never leaves you without a desktop.
    pub fn load() -> Self {
        for path in Self::candidate_paths() {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            return match toml::from_str::<Config>(&text) {
                Ok(mut cfg) => {
                    tracing::info!("loaded config from {}", path.display());
                    cfg.sanitize();
                    cfg
                }
                Err(err) => {
                    tracing::error!("invalid config {}: {err}; using defaults", path.display());
                    Config::default()
                }
            };
        }
        Config::default()
    }

    /// The terminal command to run, resolving the empty default against $PATH.
    pub fn terminal_command(&self) -> Option<String> {
        if !self.terminal.trim().is_empty() {
            return Some(self.terminal.clone());
        }
        let path = std::env::var_os("PATH")?;
        ["foot", "kitty", "alacritty", "wezterm", "xterm"]
            .into_iter()
            .find(|t| std::env::split_paths(&path).any(|d| d.join(t).is_file()))
            .map(String::from)
    }

    fn sanitize(&mut self) {
        self.layout.column_width = self.layout.column_width.clamp(0.1, 1.0);
        self.layout.workspaces = self.layout.workspaces.clamp(1, 32);
        self.layout.gap = self.layout.gap.clamp(0, 200);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dnd_schedule_wraps_midnight() {
        let n = Notifications { dnd_schedule: true, dnd_from: 22, dnd_to: 7, ..Default::default() };
        assert!(n.scheduled_at(23) && n.scheduled_at(0) && n.scheduled_at(6));
        assert!(!n.scheduled_at(7) && !n.scheduled_at(12) && !n.scheduled_at(21));
        let day = Notifications { dnd_schedule: true, dnd_from: 9, dnd_to: 17, ..Default::default() };
        assert!(day.scheduled_at(9) && !day.scheduled_at(17));
        assert!(!Notifications { dnd_from: 22, dnd_to: 7, ..Default::default() }.scheduled_at(23), "off unless enabled");
    }

    use super::*;

    #[test]
    fn empty_is_default() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.layout.workspaces, 10);
    }

    #[test]
    fn input_sections_parse() {
        let cfg: Config = toml::from_str("[input.touchpad]\ntap = false\nclick_method = \"buttonareas\"\n[input.mouse]\naccel_profile = \"flat\"\n[media]\nvolume_up = \"x\"").unwrap();
        assert!(!cfg.input.touchpad.tap);
        assert!(cfg.input.touchpad.swap_scroll);
        assert_eq!(cfg.media.volume_up, "x");
    }

    #[test]
    fn unknown_keys_rejected() {
        assert!(toml::from_str::<Config>("bogus = 1").is_err());
    }
}

/// The wallpaper picked in the changer (Mod+W). It overrides `[wallpaper] path` and `mode`, so a choice made while
/// the desktop is running survives a restart without editing the config file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WallpaperChoice {
    pub path: String,
    pub mode: String,
}

impl WallpaperChoice {
    /// `$XDG_STATE_HOME/vesper/wallpaper.toml` (default `~/.local/state/vesper/wallpaper.toml`).
    pub fn file() -> PathBuf {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("vesper/wallpaper.toml")
    }

    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(Self::file()).ok()?;
        toml::from_str(&text).ok()
    }

    /// Write the choice atomically, so a reader never sees half a file.
    pub fn save(&self) -> std::io::Result<()> {
        let file = Self::file();
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, file)
    }
}

#[cfg(test)]
mod wallpaper_tests {
    use super::*;

    #[test]
    fn choice_roundtrip() {
        let c = WallpaperChoice { path: "/a b/c.jpg".into(), mode: "fit".into() };
        let text = toml::to_string(&c).unwrap();
        assert_eq!(toml::from_str::<WallpaperChoice>(&text).unwrap(), c);
    }
}

// ------------------------------------------------------------------ appearance, animations, displays, power

/// How windows look.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    /// Window border thickness in logical pixels; 0 = no borders.
    pub border_width: i32,
    /// Border of the focused window as `#rrggbb[aa]`; empty = the wallpaper-derived accent colour.
    pub border_focus: String,
    /// Border of the other windows; empty = a muted colour from the palette.
    pub border_unfocus: String,
    /// Draw the border around tiled windows too (otherwise only floating windows get one).
    pub border_tiled: bool,
    /// Rounded window corners, in logical pixels; 0 = square.
    pub corner_radius: i32,
    /// Warm, dimmed colours for the evening (a tint over the whole screen).
    pub night_light: bool,
    /// How strong the night light is, 1 to 100.
    pub night_light_strength: u32,
    /// Cursor theme name ("" = the built-in arrow); see Settings > Appearance. Themes are looked up in the usual icon folders and in Vesper's own `assets/cursors`.
    pub cursor_theme: String,
    /// Cursor size in pixels.
    pub cursor_size: u32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self { border_width: 2, border_focus: String::new(), border_unfocus: String::new(), border_tiled: true, corner_radius: 12, night_light: false, night_light_strength: 35, cursor_theme: "Bibata-Modern-Classic".into(), cursor_size: 24 }
    }
}

/// Window and workspace motion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Animations {
    pub enabled: bool,
    /// Length of a movement in milliseconds.
    pub duration_ms: u32,
    /// "ease-out", "ease-in-out", "linear" or "spring".
    pub curve: String,
}

impl Default for Animations {
    fn default() -> Self {
        Self { enabled: true, duration_ms: 220, curve: "ease-out".into() }
    }
}

/// One monitor. Anything left out keeps the monitor's own preference.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// `false` switches the monitor off (it is still listed so you can switch it back on).
    pub enabled: Option<bool>,
    /// `"2560x1440"` or `"2560x1440@144"`; empty = the preferred mode.
    pub mode: String,
    /// Scale factor, 0.5 to 4.0 (fractional values such as 1.25 are fine); 0 = automatic.
    pub scale: f64,
    /// Position of the top-left corner in logical pixels; both left out = placed to the right of the previous monitor.
    pub x: Option<i32>,
    pub y: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Display {
    /// Monitor that holds the tiled workspaces; empty = the first one found.
    pub primary: String,
    /// Settings per connector name (`eDP-1`, `HDMI-A-1`, ...), e.g. `[display.outputs."eDP-1"]`.
    pub outputs: std::collections::BTreeMap<String, OutputConfig>,
}

/// Sleep and power button.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Power {
    /// Turn the screens off after this many idle seconds; 0 = never.
    pub screen_off_after: u64,
    /// Suspend after this many idle seconds; 0 = never.
    pub suspend_after: u64,
    /// Lock the session before suspending.
    pub lock_before_sleep: bool,
    /// What the power key does: "menu", "suspend", "poweroff", or "ignore".
    pub power_key: String,
    /// Command that suspends the machine; empty = `loginctl suspend`.
    pub suspend_command: String,
    pub poweroff_command: String,
    pub reboot_command: String,
    /// What closing the lid does: "lock" (lock the screen; systemd-logind may suspend as well), "suspend", or "ignore".
    /// Nothing happens while another monitor is connected (docked).
    pub lid_close: String,
    /// Suspend when the battery is nearly empty (3%) and not charging.
    pub critical_battery_suspend: bool,
}

impl Default for Power {
    fn default() -> Self {
        Self {
            screen_off_after: 600,
            suspend_after: 0,
            lock_before_sleep: true,
            power_key: "menu".into(),
            suspend_command: String::new(),
            poweroff_command: String::new(),
            reboot_command: String::new(),
            lid_close: "lock".into(),
            critical_battery_suspend: true,
        }
    }
}

impl Power {
    pub fn suspend(&self) -> String {
        or_default(&self.suspend_command, "loginctl suspend")
    }
    pub fn poweroff(&self) -> String {
        or_default(&self.poweroff_command, "loginctl poweroff")
    }
    pub fn reboot(&self) -> String {
        or_default(&self.reboot_command, "loginctl reboot")
    }
}

fn or_default(v: &str, d: &str) -> String {
    if v.trim().is_empty() { d.into() } else { v.into() }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notifications {
    /// How many past notifications the history keeps.
    pub history_max: usize,
    /// Seconds a notification stays when the sender gives no timeout; 0 = until dismissed.
    pub default_timeout: u32,
    /// Do not pop notifications up (they still go into the history).
    pub do_not_disturb: bool,
    /// Turn do-not-disturb on by itself between two hours of the day.
    pub dnd_schedule: bool,
    /// The hour (0-23) the schedule starts, and the hour it ends (it may run past midnight).
    pub dnd_from: u32,
    pub dnd_to: u32,
}

impl Notifications {
    /// Whether the schedule says "do not disturb" at `hour` (0-23).
    pub fn scheduled_at(&self, hour: u32) -> bool {
        if !self.dnd_schedule || self.dnd_from % 24 == self.dnd_to % 24 {
            return false;
        }
        let (from, to, h) = (self.dnd_from % 24, self.dnd_to % 24, hour % 24);
        if from < to { h >= from && h < to } else { h >= from || h < to }
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Self { history_max: 100, default_timeout: 6, do_not_disturb: false, dnd_schedule: false, dnd_from: 22, dnd_to: 7 }
    }
}

/// The app launcher (Mod+Space).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Launcher {
    /// Desktop-file ids of pinned apps (they come first). Pin from the launcher with Ctrl+P or the star.
    pub pinned: Vec<String>,
    /// Gap between the bar and the launcher in pixels.
    pub gap: u32,
    /// Width in pixels.
    pub width: u32,
}

impl Default for Launcher {
    fn default() -> Self {
        Self { pinned: Vec::new(), gap: 12, width: 620 }
    }
}

/// Default applications.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Apps {
    /// Started by Mod+E; empty = `file-manager`.
    pub file_manager: String,
}

impl Config {
    /// The file Settings writes to: the first existing config file, else `~/.config/vesper/config.toml`.
    pub fn user_file() -> PathBuf {
        let paths = Self::candidate_paths();
        paths
            .iter()
            .find(|p| p.is_file())
            .cloned()
            .or_else(|| paths.into_iter().find(|p| p.starts_with(std::env::var_os("HOME").unwrap_or_default())))
            .unwrap_or_else(|| PathBuf::from("/tmp/vesper-config.toml"))
    }

    /// Change the user's config file without touching keys you do not mention, and write it atomically.
    /// The compositor notices the new timestamp and applies it straight away.
    pub fn edit(f: impl FnOnce(&mut toml::Table)) -> std::io::Result<()> {
        let file = Self::user_file();
        let mut table = std::fs::read_to_string(&file).ok().and_then(|t| t.parse::<toml::Table>().ok()).unwrap_or_default();
        f(&mut table);
        // never write something the compositor would then reject
        let text = toml::to_string_pretty(&table).map_err(std::io::Error::other)?;
        toml::from_str::<Config>(&text).map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, file)
    }

    /// Set `a.b.c` to `value` in the user's config.
    pub fn set(path: &str, value: toml::Value) -> std::io::Result<()> {
        Self::edit(|t| set_path(t, path, value))
    }

    /// Remove `a.b.c` so it goes back to its default.
    pub fn unset(path: &str) -> std::io::Result<()> {
        Self::edit(|t| {
            let parts: Vec<&str> = path.split('.').collect();
            let mut cur = t;
            for p in &parts[..parts.len() - 1] {
                match cur.get_mut(*p).and_then(|v| v.as_table_mut()) {
                    Some(n) => cur = n,
                    None => return,
                }
            }
            cur.remove(parts[parts.len() - 1]);
        })
    }

    /// Modification time of the config file in use, to notice edits.
    pub fn stamp() -> Option<std::time::SystemTime> {
        Self::candidate_paths().iter().find_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
    }
}

/// Set a dotted path (keys that themselves contain dots can be quoted: `display.outputs."eDP-1".scale`).
fn set_path(t: &mut toml::Table, path: &str, value: toml::Value) {
    let parts = split_path(path);
    let mut cur = t;
    for p in &parts[..parts.len() - 1] {
        let entry = cur.entry(p.clone()).or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        cur = entry.as_table_mut().unwrap();
    }
    cur.insert(parts[parts.len() - 1].clone(), value);
}

fn split_path(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in path.chars() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn set_path_nests_and_keeps_others() {
        let mut t: toml::Table = toml::from_str("[layout]\ngap = 3").unwrap();
        set_path(&mut t, "display.outputs.\"eDP-1\".scale", toml::Value::Float(1.5));
        set_path(&mut t, "layout.workspaces", toml::Value::Integer(6));
        let cfg: Config = toml::from_str(&toml::to_string(&t).unwrap()).unwrap();
        assert_eq!(cfg.layout.gap, 3);
        assert_eq!(cfg.layout.workspaces, 6);
        assert_eq!(cfg.display.outputs["eDP-1"].scale, 1.5);
    }

    #[test]
    fn new_sections_default() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.appearance.border_width, 2);
        assert!(cfg.animations.enabled);
        assert_eq!(cfg.power.suspend(), "loginctl suspend");
    }
}

// ------------------------------------------------------------------ reading values by path (for the settings window)

impl Config {
    /// The whole effective configuration (your file on top of the defaults) as a TOML value tree.
    pub fn effective() -> toml::Value {
        toml::Value::try_from(Config::load()).unwrap_or_else(|_| toml::Value::Table(toml::Table::new()))
    }
}

/// Look up `a.b.c` (keys containing dots may be quoted) in a value tree.
pub fn lookup<'a>(root: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    let mut cur = root;
    for part in split_path(path) {
        cur = cur.get(&part)?;
    }
    Some(cur)
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    #[test]
    fn effective_has_defaults_and_lookup_finds_them() {
        let v = Config::effective();
        assert_eq!(lookup(&v, "layout.gap").and_then(|x| x.as_integer()), Some(8));
        assert_eq!(lookup(&v, "animations.curve").and_then(|x| x.as_str()), Some("ease-out"));
        assert!(lookup(&v, "nope.nothing").is_none());
        let cfg: Config = toml::from_str("[display.outputs.\"HDMI-A-1\"]\nscale = 2.0").unwrap();
        let v = toml::Value::try_from(&cfg).unwrap();
        assert_eq!(lookup(&v, "display.outputs.\"HDMI-A-1\".scale").and_then(|x| x.as_float()), Some(2.0));
    }
}

#[cfg(test)]
mod default_file_tests {
    #[test]
    fn shipped_default_config_parses() {
        let text = include_str!("../../../config/default.toml");
        let cfg: super::Config = toml::from_str(text).expect("config/default.toml must be valid");
        assert_eq!(cfg.appearance.border_width, 2);
        assert_eq!(cfg.power.screen_off_after, 600);
    }
}

/// Background blur behind translucent windows. Only windows listed in `apps` get it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Blur {
    pub enabled: bool,
    /// 1 (light) to 100 (heavy).
    pub strength: u32,
    /// Brightness of the blurred background in percent (100 = unchanged, lower darkens).
    pub brightness: u32,
    /// Colour saturation of the blurred background in percent (100 = unchanged, 0 = grey).
    pub saturation: u32,
    /// Application ids (see Mod+B) whose windows get blurred backgrounds, e.g. "kitty". Matching ignores case.
    pub apps: Vec<String>,
}

impl Default for Blur {
    fn default() -> Self {
        Self { enabled: true, strength: 30, brightness: 100, saturation: 100, apps: Vec::new() }
    }
}

impl Blur {
    pub fn matches(&self, app_id: &str) -> bool {
        self.enabled && !app_id.is_empty() && self.apps.iter().any(|a| a.eq_ignore_ascii_case(app_id))
    }
}

#[cfg(test)]
mod blur_tests {
    use super::*;

    #[test]
    fn blur_matches_listed_apps_only() {
        let mut b = Blur { apps: vec!["kitty".into()], ..Default::default() };
        assert!(b.matches("Kitty"));
        assert!(!b.matches("firefox"));
        b.enabled = false;
        assert!(!b.matches("kitty"));
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    #[test]
    fn rules_match_by_app_id_and_title() {
        let r = WindowRule { app_id: "pavucontrol".into(), float: true, ..Default::default() };
        assert!(r.matches("org.pulseaudio.pavucontrol", "Volume Control"));
        assert!(!r.matches("firefox", "x"));
        let t = WindowRule { title: "Picture-in-Picture".into(), ..Default::default() };
        assert!(t.matches("firefox", "picture-in-picture"));
        // an empty rule matches nothing (it would catch every window)
        assert!(!WindowRule::default().matches("a", "b"));
    }

    #[test]
    fn rules_parse_from_toml() {
        let c: Config = toml::from_str("[[rules]]\napp_id = \"mpv\"\nfloat = true\nwidth = 800\nheight = 450\nworkspace = 3\n").unwrap();
        assert_eq!(c.rules.len(), 1);
        assert_eq!(c.rules[0].workspace, 3);
    }
}

/// Folders that can hold cursor themes (a theme is a folder with a `cursors` folder inside): `XCURSOR_PATH`, the usual
/// icon folders and the themes that come with Vesper.
pub fn cursor_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("XCURSOR_PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    if let Some(p) = std::env::var_os("VESPER_CURSORS") {
        dirs.extend(std::env::split_paths(&p));
    }
    if let Some(h) = &home {
        dirs.push(h.join(".local/share/icons"));
        dirs.push(h.join(".icons"));
    }
    dirs.push("/usr/local/share/icons".into());
    dirs.push("/usr/share/icons".into());
    // Vesper's own: next to a dev build (target/debug/../../assets) and where the package installs them
    if let Some(exe) = std::env::current_exe().ok().and_then(|e| e.parent().map(std::path::Path::to_path_buf)) {
        dirs.push(exe.join("../../assets/cursors"));
        dirs.push(exe.join("../share/vesper/cursors"));
    }
    dirs.push("/usr/share/vesper/cursors".into());
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let key = d.canonicalize().unwrap_or_else(|_| d.clone());
        let new = !seen.contains(&key);
        seen.push(key);
        new && d.is_dir()
    });
    dirs
}

/// Names of the cursor themes that are installed.
pub fn cursor_themes() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in cursor_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if e.path().join("cursors").is_dir() {
                names.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// Tests that point `VESPER_CONFIG` at a temporary file take this lock, because the environment is shared.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A string kept for the life of the program, one copy per distinct text (for ids that live in `&'static str` fields).
pub fn intern(s: &str) -> &'static str {
    use std::{collections::HashSet, sync::Mutex};
    static POOL: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
    let set = pool.get_or_insert_with(HashSet::new);
    if let Some(&existing) = set.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    set.insert(leaked);
    leaked
}
