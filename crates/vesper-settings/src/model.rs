//! What the settings window shows: pages of sections of rows, built from the current configuration, and how a change is saved.

use toml::Value;
use vesper_config::{lookup, theme::ThemeSettings, Config};
use vesper_ipc::State;

/// A value a control can take.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// Back to the default: removes the key from the config file.
    Unset,
}

impl Val {
    fn to_toml(&self) -> Option<Value> {
        Some(match self {
            Val::Bool(b) => Value::Boolean(*b),
            Val::Int(i) => Value::Integer(*i),
            Val::Float(f) => Value::Float(*f),
            Val::Str(s) => Value::String(s.clone()),
            Val::Unset => return None,
        })
    }

    pub fn from_toml(v: &Value) -> Val {
        match v {
            Value::Boolean(b) => Val::Bool(*b),
            Value::Integer(i) => Val::Int(*i),
            Value::Float(f) => Val::Float(*f),
            Value::String(s) => Val::Str(s.clone()),
            _ => Val::Unset,
        }
    }

    /// Equal even when one side is an integer and the other a float of the same value.
    pub fn same(&self, other: &Val) -> bool {
        match (self, other) {
            (Val::Int(a), Val::Float(b)) | (Val::Float(b), Val::Int(a)) => (*a as f64 - b).abs() < 1e-9,
            (Val::Float(a), Val::Float(b)) => (a - b).abs() < 1e-9,
            _ => self == other,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Act {
    OpenPicker,
    OpenThemeEditor,
    OpenNotifications,
    OpenConfigFile,
    ClearBarSplits,
    /// Config profiles: switch to one, share one (file and clipboard), delete one.
    PluginToggle(String),
    PluginRemove(String),
    OpenPluginsFolder,
    ProfileApply(String),
    ProfileShare(String),
    ProfileDelete(String),
    /// Show/hide (0) or move left (-1) / right (1) one of the bar's right-hand items.
    BarItem(&'static str, i8),
    MakePrimary(String),
    ClearOutput(String),
    RemoveListItem(&'static str, usize),
    Lock,
    Suspend,
}

#[derive(Clone, Debug)]
pub enum Ctl {
    Toggle { path: String, on: bool },
    Choice { path: String, options: Vec<(Val, String)>, cur: Val },
    Number { path: String, val: f64, min: f64, max: f64, step: f64, int: bool, unit: &'static str },
    Text { path: String, val: String, hint: String },
    /// A draggable slider.
    Slider { path: String, val: f64, min: f64, max: f64, int: bool, unit: &'static str },
    Button { label: String, act: Act },
    /// Several buttons side by side (apply, share, delete ...).
    Buttons(Vec<(String, Act)>),
    /// A few of a cursor theme's pointers, drawn as a preview.
    Cursors { theme: String, size: u32 },
    /// One item of the bar: a switch to show it and arrows to move it left or right.
    BarItem { id: &'static str, shown: bool, left: bool, right: bool },
    /// The monitors as boxes you drag to arrange them; `center` is the middle of their bounding box (logical pixels).
    MonitorMap { mons: Vec<MapMon>, center: (f64, f64) },
    Info(String),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub title: String,
    pub desc: String,
    pub ctl: Ctl,
}

#[derive(Clone, Debug)]
pub struct Section {
    pub title: String,
    pub rows: Vec<Row>,
    /// The tab this section belongs to (empty = the page has no tabs).
    pub tab: String,
}

impl Page {
    /// The tab names, in order (empty when the page has no tabs).
    pub fn tabs(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in &self.sections {
            if !s.tab.is_empty() && !out.contains(&s.tab) {
                out.push(s.tab.clone());
            }
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    pub title: &'static str,
    pub sections: Vec<Section>,
}

pub const PAGES: [(&str, &str, &str); 13] = [
    ("display", "Display", "\u{f0379}"),
    ("power", "Power & sleep", "\u{f0425}"),
    ("appearance", "Appearance", "\u{f03d8}"),
    ("background", "Background & colours", "\u{f0e09}"),
    ("bar", "Bar", "\u{f056e}"),
    ("input", "Keyboard & mouse", "\u{f030c}"),
    ("notifications", "Notifications", "\u{f009a}"),
    ("apps", "Apps", "\u{f003b}"),
    ("lock", "Lock & session", "\u{f033e}"),
    ("keys", "Keyboard shortcuts", "\u{f0313}"),
    ("profiles", "Profiles", "\u{f0c5}"),
    ("plugins", "Plugins", "\u{f12e}"),
    ("about", "About", "\u{f02fc}"),
];

/// Everything the pages are built from.
pub struct Snapshot {
    pub cfg: Value,
    pub theme: ThemeSettings,
    pub state: Option<State>,
}

impl Snapshot {
    pub fn load(state: Option<State>) -> Self {
        Self { cfg: Config::effective(), theme: ThemeSettings::load(), state }
    }

    fn get(&self, path: &str) -> Option<Val> {
        if let Some(rest) = path.strip_prefix("theme.") {
            let v = toml::Value::try_from(&self.theme).ok()?;
            return lookup(&v, rest).map(Val::from_toml);
        }
        lookup(&self.cfg, path).map(Val::from_toml)
    }

    fn b(&self, path: &str) -> bool {
        matches!(self.get(path), Some(Val::Bool(true)))
    }

    fn f(&self, path: &str) -> f64 {
        match self.get(path) {
            Some(Val::Int(i)) => i as f64,
            Some(Val::Float(f)) => f,
            _ => 0.0,
        }
    }

    fn list(&self, path: &str) -> Vec<String> {
        match lookup(&self.cfg, path) {
            Some(toml::Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
            _ => Vec::new(),
        }
    }

    fn s(&self, path: &str) -> String {
        match self.get(path) {
            Some(Val::Str(s)) => s,
            _ => String::new(),
        }
    }
}

fn toggle(snap: &Snapshot, title: &str, desc: &str, path: &str) -> Row {
    Row { title: title.into(), desc: desc.into(), ctl: Ctl::Toggle { path: path.into(), on: snap.b(path) } }
}

fn num(snap: &Snapshot, title: &str, desc: &str, path: &str, min: f64, max: f64, step: f64, int: bool, unit: &'static str) -> Row {
    Row { title: title.into(), desc: desc.into(), ctl: Ctl::Number { path: path.into(), val: snap.f(path), min, max, step, int, unit } }
}

fn slider(snap: &Snapshot, title: &str, desc: &str, path: &str, min: f64, max: f64, unit: &'static str) -> Row {
    Row { title: title.into(), desc: desc.into(), ctl: Ctl::Slider { path: path.into(), val: snap.f(path).clamp(min, max), min, max, int: true, unit } }
}

fn text(snap: &Snapshot, title: &str, desc: &str, path: &str, hint: &str) -> Row {
    Row { title: title.into(), desc: desc.into(), ctl: Ctl::Text { path: path.into(), val: snap.s(path), hint: hint.into() } }
}

fn choice(snap: &Snapshot, title: &str, desc: &str, path: &str, options: Vec<(Val, &str)>) -> Row {
    let cur = snap.get(path).unwrap_or(Val::Unset);
    Row {
        title: title.into(),
        desc: desc.into(),
        ctl: Ctl::Choice { path: path.into(), options: options.into_iter().map(|(v, l)| (v, l.to_owned())).collect(), cur },
    }
}

fn s(v: &str) -> Val {
    Val::Str(v.into())
}

fn secs(options: &[u64]) -> Vec<(Val, String)> {
    options
        .iter()
        .map(|&n| {
            let label = match n {
                0 => "Never".to_owned(),
                n if n < 60 => format!("{n} seconds"),
                n if n == 60 => "1 minute".to_owned(),
                n if n < 3600 => format!("{} minutes", n / 60),
                n if n == 3600 => "1 hour".to_owned(),
                n => format!("{} hours", n / 3600),
            };
            (Val::Int(n as i64), label)
        })
        .collect()
}

fn secs_choice(snap: &Snapshot, title: &str, desc: &str, path: &str, options: &[u64]) -> Row {
    let cur = snap.get(path).unwrap_or(Val::Unset);
    let mut opts = secs(options);
    // a value set by hand in the file still shows up
    if let Val::Int(n) = cur {
        if !opts.iter().any(|(v, _)| *v == Val::Int(n)) {
            opts.push((Val::Int(n), format!("{n} seconds")));
        }
    }
    Row { title: title.into(), desc: desc.into(), ctl: Ctl::Choice { path: path.into(), options: opts, cur } }
}

fn section(title: &str, rows: Vec<Row>) -> Section {
    Section { title: title.into(), rows, tab: String::new() }
}

fn tab_section(tab: &str, title: &str, rows: Vec<Row>) -> Section {
    Section { title: title.into(), rows, tab: tab.into() }
}

pub fn build(page: &str, snap: &Snapshot) -> Page {
    let (id, title, _icon) = PAGES.iter().find(|p| p.0 == page).copied().unwrap_or(PAGES[0]);
    let sections = match id {
        "display" => display(snap),
        "power" => power(snap),
        "appearance" => appearance(snap),
        "background" => background(snap),
        "bar" => bar(snap),
        "input" => input(snap),
        "notifications" => notifications(snap),
        "apps" => apps(snap),
        "lock" => lock(snap),
        "keys" => keys(snap),
        "profiles" => profiles_page(),
        "plugins" => plugins_page(snap),
        _ => about(),
    };
    // pages made of a few big sections get one tab per section
    let mut sections = sections;
    if matches!(id, "appearance" | "input" | "background" | "keys") {
        for sec in &mut sections {
            sec.tab = sec.title.clone();
        }
    }
    Page { title, sections }
}

fn display(snap: &Snapshot) -> Vec<Section> {
    let Some(state) = &snap.state else {
        return vec![section(
            "Monitors",
            vec![Row {
                title: "Not connected to the compositor".into(),
                desc: "Monitor information comes from the running Vesper session. Start Settings from inside Vesper (Mod+,).".into(),
                ctl: Ctl::Info(String::new()),
            }],
        )];
    };
    let mut out = Vec::new();
    let enabled = state.outputs.iter().filter(|o| o.enabled).count();
    let mons: Vec<MapMon> = state.outputs.iter().filter(|o| o.enabled).map(|o| MapMon { name: o.name.clone(), x: o.x, y: o.y, w: o.w, h: o.h, primary: o.primary }).collect();
    if mons.len() > 1 {
        let (x0, y0) = (mons.iter().map(|m| m.x).min().unwrap_or(0), mons.iter().map(|m| m.y).min().unwrap_or(0));
        let (x1, y1) = (mons.iter().map(|m| m.x + m.w).max().unwrap_or(0), mons.iter().map(|m| m.y + m.h).max().unwrap_or(0));
        let center = ((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
        out.push(section("Arrange monitors", vec![Row { title: String::new(), desc: String::new(), ctl: Ctl::MonitorMap { mons, center } }]));
    }
    for o in &state.outputs {
        let key = |f: &str| format!("display.outputs.\"{}\".{f}", o.name);
        let mut rows = Vec::new();
        if o.enabled && enabled <= 1 {
            rows.push(Row { title: "Enabled".into(), desc: "The only active monitor cannot be switched off".into(), ctl: Ctl::Info("On".into()) });
        } else {
            rows.push(Row {
                title: "Enabled".into(),
                desc: "Switch this monitor on or off".into(),
                ctl: Ctl::Toggle { path: key("enabled"), on: o.enabled },
            });
        }
        let mut modes: Vec<(Val, String)> = vec![(s(""), "Preferred".into())];
        modes.extend(o.modes.iter().map(|m| (s(m), m.replace('x', " × ").replace('@', " @ ") + " Hz")));
        let cur = snap.get(&key("mode")).unwrap_or(s(""));
        rows.push(Row { title: "Resolution".into(), desc: "Size and refresh rate".into(), ctl: Ctl::Choice { path: key("mode"), options: modes, cur } });
        let mut scales: Vec<(Val, String)> = vec![(Val::Unset, "Automatic".into())];
        for v in [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
            scales.push((Val::Float(v), format!("{}%", (v * 100.0) as i32)));
        }
        rows.push(Row {
            title: "Scale".into(),
            desc: "Make everything larger or smaller".into(),
            ctl: Ctl::Choice { path: key("scale"), options: scales, cur: snap.get(&key("scale")).filter(|v| !matches!(v, Val::Float(f) if *f == 0.0)).unwrap_or(Val::Unset) },
        });
        let turns: Vec<(Val, String)> = vec![(s("normal"), "Landscape".into()), (s("90"), "Portrait (turned left)".into()), (s("180"), "Landscape, upside down".into()), (s("270"), "Portrait (turned right)".into())];
        let cur = snap.get(&key("transform")).filter(|v| !matches!(v, Val::Str(t) if t.is_empty())).unwrap_or(s("normal"));
        rows.push(Row { title: "Orientation".into(), desc: "Rotate the picture, for a monitor stood on its side".into(), ctl: Ctl::Choice { path: key("transform"), options: turns, cur } });
        if o.enabled {
            let at = |f: &str, d: i32| match snap.get(&key(f)) {
                Some(Val::Int(i)) => i as f64,
                _ => d as f64,
            };
            let pos = |title: &str, f: &str, d: i32| Row {
                title: title.into(),
                desc: if f == "x" { "Where this monitor sits relative to the others (leave unchanged to line them up left to right)".into() } else { String::new() },
                ctl: Ctl::Number { path: key(f), val: at(f, d), min: -32000.0, max: 32000.0, step: 100.0, int: true, unit: " px" },
            };
            rows.push(pos("Position X", "x", o.x));
            rows.push(pos("Position Y", "y", o.y));
            rows.push(Row {
                title: "Reset position".into(),
                desc: "Go back to automatic placement".into(),
                ctl: Ctl::Button { label: "Reset".into(), act: Act::ClearOutput(o.name.clone()) },
            });
            rows.push(if o.primary {
                Row { title: "Main monitor".into(), desc: "Holds the tiled windows and workspaces".into(), ctl: Ctl::Info("Primary".into()) }
            } else {
                Row {
                    title: "Main monitor".into(),
                    desc: "Holds the tiled windows and workspaces".into(),
                    ctl: Ctl::Button { label: "Make this the main monitor".into(), act: Act::MakePrimary(o.name.clone()) },
                }
            });
        }
        let head = if o.enabled { format!("{}  ·  {} × {}  ·  {}", o.name, o.w, o.h, o.mode.replace('x', " × ").replace('@', " @ ")) } else { format!("{}  ·  off", o.name) };
        out.push(section(&head, rows));
    }
    if out.is_empty() {
        out.push(section("Monitors", vec![Row { title: "No monitors reported".into(), desc: String::new(), ctl: Ctl::Info(String::new()) }]));
    }
    out
}

fn power(snap: &Snapshot) -> Vec<Section> {
    vec![
        section(
            "Screen and sleep",
            vec![
                secs_choice(snap, "Turn the screen off after", "When you have not touched anything", "power.screen_off_after", &[0, 60, 120, 300, 600, 900, 1800, 3600]),
                secs_choice(snap, "Lock the screen after", "Shows the lock screen when you have been away", "idle.lock_after", &[0, 60, 120, 300, 600, 900, 1800, 3600]),
                secs_choice(snap, "Suspend after", "Put the computer to sleep when idle", "power.suspend_after", &[0, 300, 600, 900, 1800, 3600, 7200]),
                toggle(snap, "Lock before sleeping", "Ask for your password when the computer wakes up", "power.lock_before_sleep"),
                choice(snap, "Closing the lid", "Nothing happens while another monitor is connected", "power.lid_close", vec![(s("lock"), "Lock the screen"), (s("suspend"), "Suspend"), (s("ignore"), "Do nothing")]),
                toggle(snap, "Suspend when the battery is nearly empty", "At 3% and not charging (you are warned at 20%, 10% and 5%)", "power.critical_battery_suspend"),
            ],
        ),
        section(
            "Power button",
            vec![choice(
                snap,
                "Pressing the power button",
                "Also Mod+Shift+P opens the power menu",
                "power.power_key",
                vec![(s("menu"), "Show the power menu"), (s("suspend"), "Sleep"), (s("poweroff"), "Shut down"), (s("ignore"), "Do nothing")],
            )],
        ),
        section(
            "Commands",
            vec![
                text(snap, "Suspend command", "Leave empty for loginctl suspend", "power.suspend_command", "loginctl suspend"),
                text(snap, "Shut down command", "Leave empty for loginctl poweroff", "power.poweroff_command", "loginctl poweroff"),
                text(snap, "Restart command", "Leave empty for loginctl reboot", "power.reboot_command", "loginctl reboot"),
                Row { title: "Lock now".into(), desc: "Test the lock screen".into(), ctl: Ctl::Button { label: "Lock".into(), act: Act::Lock } },
            ],
        ),
    ]
}

fn appearance(snap: &Snapshot) -> Vec<Section> {
    let blur_apps: Vec<String> = match lookup(&snap.cfg, "blur.apps") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    };
    let mut blur_rows = vec![
        toggle(snap, "Blur behind windows", "Frosted-glass background for the apps listed below (they need a see-through background, such as kitty's background_opacity)", "blur.enabled"),
        slider(snap, "Blur strength", "How far the background is smeared: 1 is a hint, 100 is very heavy", "blur.strength", 1.0, 100.0, ""),
        slider(snap, "Blur brightness", "Below 100 darkens the blurred background, above lightens it", "blur.brightness", 20.0, 200.0, "%"),
        slider(snap, "Blur colour", "Saturation of the blurred background; 0 is grey, 100 is unchanged", "blur.saturation", 0.0, 200.0, "%"),
    ];
    for (i, app) in blur_apps.iter().enumerate() {
        blur_rows.push(Row { title: app.clone(), desc: "Blurred".into(), ctl: Ctl::Button { label: "Remove".into(), act: Act::RemoveListItem("blur.apps", i) } });
    }
    blur_rows.push(Row {
        title: "Add an app".into(),
        desc: "Its application id, such as kitty (or focus a window and press Mod+B)".into(),
        ctl: Ctl::Text { path: "blur.apps.+".into(), val: String::new(), hint: "kitty".into() },
    });
    vec![
        section("Blur", blur_rows),
        section("Cursor", cursor_rows(snap)),
        section(
            "Windows",
            vec![
                num(snap, "Rounded corners", "Corner radius of every window; 0 keeps them square", "appearance.corner_radius", 0.0, 40.0, 2.0, true, " px"),
                toggle(snap, "Night light", "A warm, dimmer tint over the whole screen for the evening (also in the bar's quick settings)", "appearance.night_light"),
                slider(snap, "Night light strength", "1 is faint, 100 is strong", "appearance.night_light_strength", 1.0, 100.0, ""),
                num(snap, "Border thickness", "Outline around every window; 0 switches it off", "appearance.border_width", 0.0, 12.0, 1.0, true, " px"),
                text(snap, "Focused border colour", "A hex colour such as #8b7bff; empty follows your wallpaper", "appearance.border_focus", "automatic"),
                text(snap, "Other windows' border colour", "Empty uses a muted colour from your wallpaper", "appearance.border_unfocus", "automatic"),
                toggle(snap, "Focus follows mouse", "Hovering a window focuses it, no click needed", "input.focus_follows_mouse"),
                toggle(snap, "Move the pointer to the focused window", "When Mod+arrows change the focused window the pointer jumps to its middle (not when you click or hover)", "input.warp_mouse_on_focus"),
                toggle(snap, "Border on tiled windows", "Off: only floating windows get a border", "appearance.border_tiled"),
                num(snap, "Gap between windows", "Space between columns and around the screen", "layout.gap", 0.0, 64.0, 2.0, true, " px"),
                num(snap, "New window width", "Fraction of the screen a new column takes", "layout.column_width", 0.2, 1.0, 0.05, false, ""),
                toggle(snap, "Workspaces take over unused ones", "Workspaces with windows fill the first places: unused ones move to the end a second after you leave them, so three busy workspaces are always 1 to 3", "layout.dynamic_workspaces"),
                num(snap, "Workspaces", "How many you can switch between", "layout.workspaces", 1.0, 10.0, 1.0, true, ""),
            ],
        ),
        section(
            "Animations",
            vec![
                toggle(snap, "Animate windows", "Windows glide into place when columns scroll or workspaces change", "animations.enabled"),
                num(snap, "Animation length", "", "animations.duration_ms", 50.0, 800.0, 10.0, true, " ms"),
                choice(
                    snap,
                    "Motion curve",
                    "How the movement speeds up and slows down",
                    "animations.curve",
                    vec![(s("ease-out"), "Ease out"), (s("ease-in-out"), "Ease in and out"), (s("linear"), "Linear"), (s("spring"), "Springy")],
                ),
            ],
        ),
    ]
}

fn background(snap: &Snapshot) -> Vec<Section> {
    let schemes: Vec<(Val, String)> = vesper_config::theme::SCHEMES.iter().map(|x| (s(x), x.trim_start_matches("scheme-").replace('-', " "))).collect();
    let contrast: Vec<(Val, String)> =
        [(-1.0, "Lowest"), (-0.5, "Lower"), (0.0, "Standard"), (0.5, "Higher"), (1.0, "Highest")].iter().map(|(v, l)| (Val::Float(*v), (*l).to_owned())).collect();
    vec![
        section("Colour overrides", colour_rows(snap)),
        section(
            "Wallpaper",
            vec![
                Row { title: "Change wallpaper".into(), desc: "Also Mod+W".into(), ctl: Ctl::Button { label: "Open wallpaper picker".into(), act: Act::OpenPicker } },
                text(snap, "Wallpaper folder", "Where the picker looks for images", "wallpaper.dir", "~/Pictures/Wallpapers"),
                choice(
                    snap,
                    "Fit",
                    "How an image fills the screen",
                    "wallpaper.mode",
                    vec![(s("fill"), "Fill (crop)"), (s("fit"), "Fit (bars)"), (s("stretch"), "Stretch"), (s("center"), "Centre")],
                ),
                text(snap, "Background colour", "Behind the image, or on its own with no wallpaper", "wallpaper.color", "#14141a"),
            ],
        ),
        section(
            "Login screen",
            vec![
                text(snap, "Login background", "An image file; empty uses your desktop wallpaper", "greeter.background", "desktop wallpaper"),
                slider(snap, "Background blur", "0 keeps the picture sharp", "greeter.blur", 0.0, 100.0, ""),
                slider(snap, "Background dimming", "Darkens the picture so the text stays readable", "greeter.dim", 0.0, 90.0, "%"),
            ],
        ),
        section(
            "Colours from the wallpaper",
            vec![
                toggle(snap, "Colour the desktop from the wallpaper", "Uses matugen to pick the accent and surface colours", "theme.enabled"),
                choice(snap, "Mode", "", "theme.mode", vec![(s("dark"), "Dark"), (s("light"), "Light"), (s("smart"), "Smart")]),
                Row { title: "Scheme".into(), desc: "The colour recipe".into(), ctl: Ctl::Choice { path: "theme.scheme".into(), options: schemes, cur: snap.get("theme.scheme").unwrap_or(Val::Unset) } },
                Row { title: "Contrast".into(), desc: String::new(), ctl: Ctl::Choice { path: "theme.contrast".into(), options: contrast, cur: snap.get("theme.contrast").unwrap_or(Val::Unset) } },
                toggle(snap, "Colour kitty", "Writes ~/.config/kitty/vesper-colors.conf and includes it from kitty.conf", "theme.kitty"),
                toggle(snap, "Colour foot", "Writes ~/.config/foot/vesper-colors.ini and includes it from foot.ini", "theme.foot"),
                toggle(snap, "Colour the shell prompt", "Writes ~/.local/state/vesper/shell-colors.sh for the prompt to read", "theme.shell"),
                toggle(snap, "Colour fetch", "Writes ~/.config/fetch/colors (the logo and labels follow the wallpaper)", "theme.fetch"),
                toggle(snap, "Colour File Manager", "Adds a theme called Wallpaper (Vesper) and selects it", "theme.file_manager"),
                Row { title: "Your own templates".into(), desc: "Add matugen templates for other apps".into(), ctl: Ctl::Button { label: "Open template editor".into(), act: Act::OpenThemeEditor } },
            ],
        ),
    ]
}

fn bar(snap: &Snapshot) -> Vec<Section> {
    let style = snap.s("bar.style");

    // ---- Style
    let mut look = vec![
        toggle(snap, "Show the bar", "Takes effect the next time you log in", "shell.bar"),
        choice(
            snap,
            "Bar style",
            "Fill: full width. Detached: a floating box. Notch: a tab that curves into the top. Islands: separate left, middle and right boxes",
            "bar.style",
            vec![(s("fill"), "Fill"), (s("detached"), "Detached"), (s("notch"), "Notch"), (s("islands"), "Islands")],
        ),
        num(snap, "Height", "", "bar.height", 20.0, 64.0, 2.0, true, " px"),
    ];
    if matches!(style.as_str(), "detached" | "notch" | "islands") {
        look.push(num(snap, "Corner radius", "Roundness of the box corners", "bar.radius", 0.0, 40.0, 2.0, true, " px"));
    }
    if matches!(style.as_str(), "detached" | "islands") {
        look.push(num(snap, "Space above the bar", "Gap between the top of the screen and the bar", "bar.margin_top", 0.0, 80.0, 2.0, true, " px"));
        look.push(num(snap, "Space at the sides", "Gap between the screen sides and the bar", "bar.margin_side", 0.0, 200.0, 4.0, true, " px"));
    }
    if style == "notch" {
        look.push(num(snap, "Notch width", "Width of the tab at the top of the screen", "bar.notch_width", 240.0, 2400.0, 20.0, true, " px"));
    }
    let text_rows = vec![
        text(snap, "Font", "Any installed font family (a Nerd Font shows the icons)", "bar.font", "JetBrainsMono Nerd Font"),
        num(snap, "Font size", "", "bar.font_size", 9.0, 24.0, 1.0, false, " px"),
    ];

    // ---- Border
    let border = vec![
        num(snap, "Border width", "0 removes the border (Fill: a line along the bottom; Notch: sides and bottom; others: all round)", "bar.border_width", 0.0, 12.0, 1.0, true, " px"),
        text(snap, "Border colour", "A hex colour such as #8b7bff; empty follows your wallpaper accent", "bar.border_color", "automatic"),
        toggle(snap, "Blend border with the bar", "The border becomes as see-through as the bar (follows Bar opacity)", "bar.border_blend"),
    ];

    // ---- Workspaces
    let workspaces = vec![
        choice(
            snap,
            "Workspace look",
            "Dots, halo (filled shapes with a soft ring) or numbers",
            "bar.workspace_style",
            vec![(s("dots"), "Dots"), (s("halo"), "Halo"), (s("numbers"), "Numbers")],
        ),
        choice(snap, "Workspaces shown", "Every workspace, or only the ones with windows and the current one", "bar.workspace_mode", vec![(s("all"), "All"), (s("occupied"), "Only active")]),
        text(snap, "Workspace colour", "A hex colour such as #e0c46c; empty follows your wallpaper accent", "bar.workspace_color", "automatic"),
        text(snap, "Inactive workspace colour", "A hex colour such as #6b6f80; empty keeps the muted default", "bar.workspace_inactive_color", "default"),
    ];

    // ---- Icons and splits
    let status_look = choice(snap, "Battery and wifi icons", "iPhone-style (battery with the percentage inside, fan-shaped wifi) or the font glyphs", "bar.status_icons", vec![(s("ios"), "iPhone style"), (s("glyphs"), "Font glyphs")]);
    let icon_size_row = num(snap, "Bar icon size", "One size for the bar's icons: battery, wifi, volume, bell, power, brightness, system logo and tray", "bar.item_icon_size", 12.0, 40.0, 1.0, true, " px");
    let icons = vec![icon_size_row, num(snap, "Minimized icon size", "0 fits the bar", "bar.icon_size", 0.0, 48.0, 2.0, true, " px"), status_look];
    let splits = vec![
        Row {
            title: "How to split".into(),
            desc: "Hover the gap between two icons in the bar and click it to split there; click again to join. Fill, detached and notch get a divider, islands split the island".into(),
            ctl: Ctl::Button { label: "Remove all splits".into(), act: Act::ClearBarSplits },
        },
        choice(snap, "Split look", "A solid line, dots or dashes (islands show separate boxes instead)", "bar.split_style", vec![(s("line"), "Line"), (s("dots"), "Dots"), (s("dashes"), "Dashes")]),
        num(snap, "Split thickness", "", "bar.split_width", 1.0, 8.0, 1.0, true, " px"),
        num(snap, "Split height", "As a share of the bar height", "bar.split_height", 20.0, 100.0, 5.0, true, "%"),
        text(snap, "Split colour", "A hex colour such as #6b6f80; empty keeps the muted default", "bar.split_color", "default"),
        num(snap, "Split spacing", "Room kept on each side of a split (also the gap between islands)", "bar.split_spacing", 0.0, 40.0, 2.0, true, " px"),
    ];

    // ---- System icon
    let mut icon_opts: Vec<(Val, String)> = vec![(s("auto"), "Automatic (this distribution)".into()), (s("none"), "None".into())];
    icon_opts.push((s("bundled:singularitysys"), "SingularitySYS logo (matches the bar colour)".into()));
    for (id, label, _) in vesper_apps::distro::GLYPHS {
        icon_opts.push((Val::Str(format!("glyph:{id}")), format!("Font logo: {label}")));
    }
    for (label, path) in vesper_apps::distro::distro_logos() {
        icon_opts.push((Val::Str(path), format!("Logo file: {label}")));
    }
    let cur = snap.get("bar.system_icon").unwrap_or(s("auto"));
    let system = vec![
        Row {
            title: "System icon".into(),
            desc: "The icon on the right side of the bar: your distribution's logo, a font logo, or a logo file found on this computer".into(),
            ctl: Ctl::Choice { path: "bar.system_icon".into(), options: icon_opts, cur },
        },
        text(snap, "Custom icon file", "Type the path of any .svg or .png file (replaces the choice above)", "bar.system_icon", "/path/to/logo.svg"),
        num(snap, "Icon size", "0 = the bar icon size plus 15%", "bar.system_icon_size", 0.0, 64.0, 2.0, true, " px"),
        toggle(snap, "Use the accent colour", "Paint the icon in your wallpaper accent colour", "bar.system_icon_accent"),
    ];
    let mut system = system;
    if !snap.b("bar.system_icon_accent") {
        system.push(text(snap, "Custom icon colour", "A hex colour such as #8b7bff; empty keeps the logo's own colours", "bar.system_icon_color", "original"));
    }

    // ---- Right-hand items, clock and title
    let layout = bar_layout(snap.list("bar.left_items"), snap.list("bar.middle_items"), snap.list("bar.right_items"));
    let base: [(&'static str, &str, &str); 14] = [
        ("system", "System icon", "Opens the start menu: tools, quick settings, system numbers and power"),
        ("workspaces", "Workspaces", "Click to switch workspace"),
        ("minimized", "Minimized apps", "From the current workspace; click one to bring it back"),
        ("mic", "Microphone", "Shows while the microphone is live; click to mute it"),
        ("drives", "Removable drives", "Shows while a USB stick or card is plugged in; click to open or eject it"),
        ("media", "Media player", "Previous, play/pause and next for any MPRIS player; a music note when nothing is playing"),
        ("keyboard", "Keyboard layout", "Click to switch; needs several layouts in Input settings"),
        ("tray", "Tray icons", "App icons keep their own colours"),
        ("brightness", "Brightness", "Click for a slider, scroll to change it; hidden without a backlight"),
        ("volume", "Volume", "Click for the slider and output devices"),
        ("network", "Network", "Click for nearby wifi networks"),
        ("battery", "Battery", "Click for details and the power mode"),
        ("clock", "Clock", "Click for the calendar"),
        ("bell", "Notifications", "Right-click for do not disturb"),
    ];
    let mut names: Vec<(&'static str, String, String)> = base.iter().map(|(a, b, c)| (*a, b.to_string(), c.to_string())).collect();
    // widgets that enabled plugins offer
    for p in vesper_config::plugins::active(&snap.list("plugins.enabled")).into_iter().filter(|p| p.can("bar")) {
        for b in &p.manifest.bar_item {
            let label = if b.label.is_empty() { format!("{}: {}", p.manifest.name, b.id) } else { b.label.clone() };
            names.push((vesper_config::intern(&vesper_config::plugins::item_id(&p.id, &b.id)), label, format!("From the plugin {}", p.manifest.name)));
        }
    }
    let mut items_rows: Vec<Row> = Vec::new();
    for (si, heading) in ["Left side", "Middle", "Right side"].iter().enumerate() {
        let list = &layout[si];
        let empty = list.iter().all(|id| !names.iter().any(|n| n.0 == id));
        items_rows.push(Row { title: (*heading).into(), desc: if empty { "Empty: move an item here with the arrows".into() } else { String::new() }, ctl: Ctl::Info(String::new()) });
        let known: Vec<&(&'static str, String, String)> = list.iter().filter_map(|id| names.iter().find(|n| n.0 == id)).collect();
        for (i, (id, label, desc)) in known.iter().enumerate() {
            let can_left = !(si == 0 && i == 0);
            let can_right = !(si == 2 && i + 1 == known.len());
            items_rows.push(Row { title: format!("   {label}"), desc: format!("   {desc}"), ctl: Ctl::BarItem { id, shown: true, left: can_left, right: can_right } });
        }
    }
    let hidden: Vec<&(&'static str, String, String)> = names.iter().filter(|n| !layout.iter().flatten().any(|o| o == n.0)).collect();
    if !hidden.is_empty() {
        items_rows.push(Row { title: "Hidden".into(), desc: "Switch one on to put it back on the bar".into(), ctl: Ctl::Info(String::new()) });
        for (id, label, desc) in hidden {
            items_rows.push(Row { title: format!("   {label}"), desc: format!("   {desc}"), ctl: Ctl::BarItem { id, shown: false, left: false, right: false } });
        }
    }
    let clock_rows = vec![
        toggle(snap, "24-hour clock", "Off shows 12-hour time with am/pm", "bar.clock_24h"),
        toggle(snap, "Show seconds", "", "bar.clock_seconds"),
        toggle(snap, "Show the date", "Weekday, day and month before the time", "bar.clock_date"),
        toggle(snap, "Bar on every monitor", "Each extra monitor gets its own copy of the bar", "bar.all_monitors"),
        toggle(snap, "Volume and brightness popup", "A small popup under the bar when you change the volume or brightness with the keys or by scrolling", "bar.osd"),
        toggle(snap, "Window title in the bar", "Shows the focused window's title in the middle when no apps are minimized", "bar.show_title"),
    ];

    // ---- Opacity and blur
    let mut glass = vec![slider(snap, "Bar opacity", "100 is solid; lower lets the wallpaper show through", "bar.opacity", 10.0, 100.0, "%")];
    if snap.f("bar.opacity") < 100.0 {
        glass.push(toggle(snap, "Blur behind the bar", "Frosted glass behind the see-through bar", "bar.blur"));
        if snap.b("bar.blur") {
            glass.push(slider(snap, "Bar blur strength", "1 is light, 100 is very heavy", "bar.blur_strength", 1.0, 100.0, ""));
        }
    }

    vec![
        tab_section("Style", "Look", look),
        tab_section("Style", "Text", text_rows),
        tab_section("Border", "Border", border),
        tab_section("Workspaces", "Workspace indicators", workspaces),
        tab_section("Items", "Items on the bar", items_rows),
        tab_section("Items", "Clock and title", clock_rows),
        tab_section("Icons & splits", "Minimized apps", icons),
        tab_section("Icons & splits", "Splits", splits),
        tab_section("System icon", "System icon", system),
        tab_section("Opacity & blur", "See-through bar", glass),
        tab_section(
            "Session",
            "Session services",
            vec![
                toggle(snap, "Notification daemon", "Takes effect the next time you log in", "shell.notifications"),
                toggle(snap, "Wallpaper service", "Takes effect the next time you log in", "shell.wallpaper"),
                toggle(snap, "X11 apps (Xwayland)", "Run Xwayland so X11 apps work; needs the Xwayland package; takes effect the next time you log in", "shell.xwayland"),
            ],
        ),
    ]
}

fn input(snap: &Snapshot) -> Vec<Section> {
    let layouts: Vec<(Val, &str)> = vec![
        (s(""), "System default"),
        (s("us"), "English (US)"),
        (s("gb"), "English (UK)"),
        (s("de"), "German"),
        (s("fr"), "French"),
        (s("es"), "Spanish"),
        (s("it"), "Italian"),
        (s("se"), "Swedish"),
        (s("no"), "Norwegian"),
        (s("dk"), "Danish"),
        (s("fi"), "Finnish"),
        (s("pt"), "Portuguese"),
        (s("br"), "Portuguese (Brazil)"),
        (s("pl"), "Polish"),
        (s("cz"), "Czech"),
        (s("ru"), "Russian"),
        (s("ua"), "Ukrainian"),
        (s("jp"), "Japanese"),
        (s("kr"), "Korean"),
    ];
    vec![
        section(
            "Keyboard",
            vec![
                choice(snap, "Layout", "Applies the next time you log in", "input.xkb_layout", layouts),
                text(snap, "Variant", "For example dvorak or intl", "input.xkb_variant", ""),
                text(snap, "Options", "For example caps:escape or ctrl:nocaps", "input.xkb_options", ""),
                num(snap, "Repeat delay", "Before a held key starts repeating", "input.repeat_delay", 100.0, 1000.0, 50.0, true, " ms"),
                num(snap, "Repeat rate", "Characters per second while a key is held", "input.repeat_rate", 5.0, 80.0, 5.0, true, "/s"),
                choice(snap, "Shortcut key", "The key held for desktop shortcuts", "input.modifier", vec![(Val::Unset, "Automatic (Super)"), (s("super"), "Super"), (s("alt"), "Alt")]),
            ],
        ),
        section(
            "Touchpad",
            vec![
                toggle(snap, "Tap to click", "", "input.touchpad.tap"),
                toggle(snap, "Swap scroll direction", "Two-finger scrolling moves content the other way (on by default)", "input.touchpad.swap_scroll"),
                toggle(snap, "Three-finger swipes", "Up and down change workspace, left and right move between columns", "input.touchpad.gestures"),
                toggle(snap, "Natural scrolling (libinput)", "The device driver's own version; leave off when the option above is on", "input.touchpad.natural_scroll"),
                toggle(snap, "Disable while typing", "", "input.touchpad.disable_while_typing"),
                toggle(snap, "Middle-click emulation", "Press both buttons together", "input.touchpad.middle_emulation"),
                choice(snap, "Click method", "", "input.touchpad.click_method", vec![(s("clickfinger"), "Number of fingers"), (s("buttonareas"), "Areas of the pad"), (s(""), "Device default")]),
                num(snap, "Pointer speed", "-1 slowest, 1 fastest", "input.touchpad.accel_speed", -1.0, 1.0, 0.1, false, ""),
            ],
        ),
        section(
            "Mouse",
            vec![
                toggle(snap, "Natural scrolling", "", "input.mouse.natural_scroll"),
                choice(snap, "Acceleration", "", "input.mouse.accel_profile", vec![(s("adaptive"), "Adaptive"), (s("flat"), "None (flat)")]),
                num(snap, "Pointer speed", "-1 slowest, 1 fastest", "input.mouse.accel_speed", -1.0, 1.0, 0.1, false, ""),
            ],
        ),
        section(
            "Media keys",
            vec![
                text(snap, "Volume up", "", "media.volume_up", ""),
                text(snap, "Volume down", "", "media.volume_down", ""),
                text(snap, "Mute", "", "media.volume_mute", ""),
                text(snap, "Microphone mute", "", "media.mic_mute", ""),
                toggle(snap, "Volume boost", "Let the volume go up to 150% (the volume keys, the slider and scrolling in the bar); above 100% is louder than the speakers were meant for", "media.volume_boost"),
                toggle(snap, "Push to talk", "The microphone stays muted, also at startup, and is only live while the push-to-talk key is held", "media.ptt"),
                text(snap, "Push-to-talk key", "An xkb key name, or code:N for a physical key. Easier: Mic panel in the start menu > Set the key...", "media.ptt_key", "XF86AudioMicMute"),
                text(snap, "Brightness up", "", "media.brightness_up", ""),
                text(snap, "Brightness down", "", "media.brightness_down", ""),
                text(snap, "Play / pause", "", "media.play_pause", ""),
                text(snap, "Next track", "", "media.next", ""),
                text(snap, "Previous track", "", "media.previous", ""),
            ],
        ),
    ]
}

fn notifications(snap: &Snapshot) -> Vec<Section> {
    vec![section(
        "Notifications",
        vec![
            toggle(snap, "Do not disturb", "Notifications go to the history without popping up (urgent ones still do)", "notifications.do_not_disturb"),
            toggle(snap, "Do not disturb on a schedule", "Turns itself on between the two hours below", "notifications.dnd_schedule"),
            num(snap, "Schedule starts at", "Hour of the day, 0-23", "notifications.dnd_from", 0.0, 23.0, 1.0, true, ":00"),
            num(snap, "Schedule ends at", "Hour of the day, 0-23 (may be after midnight)", "notifications.dnd_to", 0.0, 23.0, 1.0, true, ":00"),
            secs_choice(snap, "Stay on screen for", "When the app does not say; 0 keeps them until clicked", "notifications.default_timeout", &[0, 3, 5, 6, 10, 15, 30]),
            num(snap, "History size", "How many past notifications are kept", "notifications.history_max", 10.0, 500.0, 10.0, true, ""),
            choice(snap, "Where popups appear", "", "notifications.position", vec![(s("top-right"), "Top right"), (s("top-left"), "Top left"), (s("bottom-right"), "Bottom right"), (s("bottom-left"), "Bottom left")]),
            num(snap, "Popup width", "", "notifications.width", 260.0, 640.0, 20.0, true, " px"),
            num(snap, "Space from the screen edge", "", "notifications.margin", 0.0, 200.0, 4.0, true, " px"),
            num(snap, "Popups at once", "More wait until others go", "notifications.max_visible", 1.0, 10.0, 1.0, true, ""),
            Row { title: "Notification centre".into(), desc: "Also Mod+N".into(), ctl: Ctl::Button { label: "Open".into(), act: Act::OpenNotifications } },
        ],
    )]
}

fn colour_rows(snap: &Snapshot) -> Vec<Row> {
    let roles: [(&str, &str); 10] = [
        ("primary", "Accent"),
        ("on_primary", "Text on the accent"),
        ("surface", "Panel background"),
        ("surface_container", "Raised background"),
        ("surface_container_high", "Raised background, brighter"),
        ("on_surface", "Text"),
        ("on_surface_variant", "Muted text"),
        ("secondary", "Second accent"),
        ("tertiary", "Third accent"),
        ("error", "Warnings and errors"),
    ];
    let mut rows = vec![Row { title: "Your own colours".into(), desc: "A hex colour such as #8b7bff replaces the wallpaper's colour everywhere (bar, launcher, popups, lock). Empty keeps the wallpaper's. Other roles can be set in config.toml under [colors]".into(), ctl: Ctl::Info(String::new()) }];
    for (role, label) in roles {
        rows.push(text(snap, label, "", &format!("colors.{role}"), "from the wallpaper"));
    }
    rows
}

fn launcher_rows(snap: &Snapshot) -> Vec<Row> {
    vec![
        choice(snap, "Where it opens", "", "launcher.position", vec![(s("top"), "Under the bar"), (s("center"), "Middle of the screen")]),
        num(snap, "Width", "", "launcher.width", 400.0, 1100.0, 20.0, true, " px"),
        num(snap, "Gap under the bar", "Only when it opens under the bar", "launcher.gap", 0.0, 200.0, 4.0, true, " px"),
        num(snap, "Results shown", "How many rows before it scrolls", "launcher.rows", 3.0, 14.0, 1.0, true, ""),
        num(snap, "Row height", "", "launcher.row_height", 32.0, 72.0, 2.0, true, " px"),
        num(snap, "Icon size", "", "launcher.icon_size", 16.0, 64.0, 2.0, true, " px"),
    ]
}

fn apps(snap: &Snapshot) -> Vec<Section> {
    let terminals: Vec<(Val, &str)> = vec![(s(""), "Automatic"), (s("foot"), "foot"), (s("kitty"), "kitty"), (s("alacritty"), "Alacritty"), (s("wezterm"), "WezTerm"), (s("xterm"), "xterm")];
    let auto = snap.get("autostart");
    let _ = auto;
    let mut rows = vec![
        choice(snap, "Terminal", "Opened by Mod+Enter", "terminal", terminals),
        text(snap, "Terminal command", "Or type any command, such as alacritty -e fish", "terminal", "automatic"),
        text(snap, "File manager", "Opened by Mod+E; empty uses File Manager", "apps.file_manager", "file-manager"),
    ];
    let list: Vec<String> = match lookup(&snap.cfg, "autostart") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    };
    let mut auto_rows = Vec::new();
    for (i, cmd) in list.iter().enumerate() {
        auto_rows.push(Row { title: cmd.clone(), desc: String::new(), ctl: Ctl::Button { label: "Remove".into(), act: Act::RemoveListItem("autostart", i) } });
    }
    auto_rows.push(Row { title: "Add a program".into(), desc: "Type its command, then press Enter. It starts with every login".into(), ctl: Ctl::Text { path: "autostart.+".into(), val: String::new(), hint: "command".into() } });
    rows.truncate(3);
    vec![section("Default apps", rows), section("Launcher (Mod+Space)", launcher_rows(snap)), section("Start at login", auto_rows)]
}

fn lock(snap: &Snapshot) -> Vec<Section> {
    vec![
        section(
            "Lock screen",
            vec![
                secs_choice(snap, "Lock after", "Idle time before the screen locks", "idle.lock_after", &[0, 60, 120, 300, 600, 900, 1800, 3600]),
                text(snap, "Switch user button", "auto (when SDDM or LightDM runs), never, or your own command", "lock.switch_user", "auto"),
                Row { title: "Lock now".into(), desc: "Also Mod+L or Ctrl+Alt+L".into(), ctl: Ctl::Button { label: "Lock".into(), act: Act::Lock } },
            ],
        ),
        section(
            "Session",
            vec![
                Row { title: "Sleep".into(), desc: "Lock and suspend the computer".into(), ctl: Ctl::Button { label: "Sleep now".into(), act: Act::Suspend } },
                Row { title: "Screenshots folder".into(), desc: "".into(), ctl: Ctl::Text { path: "screenshots.dir".into(), val: snap.s("screenshots.dir"), hint: "~/Pictures/Screenshots".into() } },
                toggle(snap, "Share single windows", "Lets the screen picker (Discord, browsers, OBS) offer one window, not only whole monitors. Applies after a restart; turn it off if monitor sharing misbehaves", "screenshots.share_windows"),
            ],
        ),
    ]
}

/// Shortcuts you can change (type a new key, for example `Mod+Shift+Q`; separate several with commas; empty = the default),
/// then the ones that are fixed.
fn keys(snap: &Snapshot) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for b in vesper_config::keys::BINDS {
        if out.last().map(|s| s.title.as_str()) != Some(b.section) {
            out.push(section(b.section, Vec::new()));
        }
        let path = format!("keys.{}", b.id);
        let val = match snap.get(&path) {
            Some(Val::Str(v)) if !v.trim().is_empty() => v,
            _ => b.default.to_string(),
        };
        let changed = val != b.default;
        out.last_mut().unwrap().rows.push(Row {
            title: b.label.into(),
            desc: if changed { format!("Default: {}", b.default) } else { String::new() },
            ctl: Ctl::Text { path, val, hint: b.default.into() },
        });
    }
    // shortcuts that enabled plugins add
    let mut plug: Vec<Row> = Vec::new();
    for p in vesper_config::plugins::active(&snap.list("plugins.enabled")).into_iter().filter(|p| p.can("keys")) {
        for key in &p.manifest.key {
            let id = vesper_config::plugins::key_id(&p.id, &key.id);
            let path = format!("keys.{id}");
            let val = match snap.get(&path) {
                Some(Val::Str(v)) if !v.trim().is_empty() => v,
                _ => key.default.clone(),
            };
            plug.push(Row { title: if key.label.is_empty() { key.id.clone() } else { key.label.clone() }, desc: format!("From the plugin {}", p.manifest.name), ctl: Ctl::Text { path, val, hint: key.default.clone() } });
        }
    }
    if !plug.is_empty() {
        out.push(section("From plugins", plug));
    }
    let k = |a: &str, b: &str| Row { title: a.into(), desc: String::new(), ctl: Ctl::Info(b.into()) };
    out.push(section(
        "Fixed shortcuts",
        vec![
            Row { title: "How to write a shortcut".into(), desc: "Mod is Super (or Alt, see Keyboard & mouse). Examples: Mod+Shift+Q, Ctrl+Alt+T, Print, or two keys: Mod+Left, Mod+H".into(), ctl: Ctl::Info(String::new()) },
            k("Go to workspace 1 – 10", "Mod + 1 … 9, 0"),
            k("Send the window to workspace 1 – 10", "Mod + Shift + 1 … 9, 0"),
            k("Move or resize a floating window", "Mod + drag with left / right button"),
            k("Switch to a text console", "Ctrl + Alt + F1 – F12"),
            k("Volume, brightness and media keys", "The keys on your keyboard"),
        ],
    ));
    out
}

fn about() -> Vec<Section> {
    vec![section(
        "Vesper",
        vec![
            Row { title: "Version".into(), desc: String::new(), ctl: Ctl::Info(env!("CARGO_PKG_VERSION").into()) },
            Row { title: "Settings file".into(), desc: Config::user_file().display().to_string(), ctl: Ctl::Button { label: "Open".into(), act: Act::OpenConfigFile } },
            Row { title: "Colour settings".into(), desc: ThemeSettings::file().display().to_string(), ctl: Ctl::Info(String::new()) },
            Row { title: "File Manager".into(), desc: "Forked from Ski by ilyamiro".into(), ctl: Ctl::Info(String::new()) },
        ],
    )]
}

// ------------------------------------------------------------------ saving

/// Save one value. Paths starting with `theme.` go to the colour settings, `autostart.+` appends a program.
pub fn apply(path: &str, val: &Val) -> Result<(), String> {
    if let Some(field) = path.strip_prefix("theme.") {
        let mut t = ThemeSettings::load();
        match (field, val) {
            ("enabled", Val::Bool(b)) => t.enabled = *b,
            ("kitty", Val::Bool(b)) => t.kitty = *b,
            ("foot", Val::Bool(b)) => t.foot = *b,
            ("shell", Val::Bool(b)) => t.shell = *b,
            ("fetch", Val::Bool(b)) => t.fetch = *b,
            ("file_manager", Val::Bool(b)) => t.file_manager = *b,
            ("mode", Val::Str(v)) => t.mode = v.clone(),
            ("scheme", Val::Str(v)) => t.scheme = v.clone(),
            ("contrast", Val::Float(v)) => t.contrast = *v as f32,
            ("contrast", Val::Int(v)) => t.contrast = *v as f32,
            _ => return Err(format!("unknown colour setting {field}")),
        }
        t.save().map_err(|e| e.to_string())?;
        vesper_config::theme::apply_app_links(&t);
        // regenerate for the current wallpaper so the change is visible straight away
        std::thread::spawn(move || {
            if let Some(choice) = vesper_config::WallpaperChoice::load() {
                let _ = vesper_config::theme::regenerate(std::path::Path::new(&choice.path), &t);
            }
        });
        return Ok(());
    }
    if let Some(key) = path.strip_suffix(".+") {
        let Val::Str(item) = val else { return Ok(()) };
        let item = item.trim().to_owned();
        if item.is_empty() {
            return Ok(());
        }
        let parts: Vec<String> = key.split('.').map(String::from).collect();
        return Config::edit(|t| push_item(t, &parts, item)).map_err(|e| e.to_string());
    }
    let res = match val.to_toml() {
        Some(v) => Config::set(path, v),
        None => Config::unset(path),
    }
    .map_err(|e| e.to_string());
    if path.starts_with("colors.") {
        vesper_config::theme::touch_colors();
    }
    res
}

/// Append `item` to the array at `path` (creating tables on the way).
fn push_item(t: &mut toml::Table, path: &[String], item: String) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut cur = t;
    for p in parents {
        let e = cur.entry(p.clone()).or_insert_with(|| Value::Table(toml::Table::new()));
        if !e.is_table() {
            *e = Value::Table(toml::Table::new());
        }
        cur = e.as_table_mut().unwrap();
    }
    let list = cur.entry(last.clone()).or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(a) = list {
        a.push(Value::String(item));
    }
}

pub fn remove_list_item(key: &str, index: usize) -> Result<(), String> {
    let parts: Vec<String> = key.split('.').map(String::from).collect();
    Config::edit(|t| {
        let mut cur = t;
        for p in &parts[..parts.len() - 1] {
            match cur.get_mut(p).and_then(|v| v.as_table_mut()) {
                Some(n) => cur = n,
                None => return,
            }
        }
        if let Some(Value::Array(a)) = cur.get_mut(&parts[parts.len() - 1]) {
            if index < a.len() {
                a.remove(index);
            }
        }
    })
    .map_err(|e| e.to_string())
}

pub fn clear_output(name: &str) -> Result<(), String> {
    Config::unset(&format!("display.outputs.\"{name}\".x")).and_then(|_| Config::unset(&format!("display.outputs.\"{name}\".y"))).map_err(|e| e.to_string())
}

/// Parse text typed into a number box.
pub fn parse_number(text: &str, min: f64, max: f64, int: bool) -> Option<Val> {
    let v: f64 = text.trim().trim_end_matches(|c: char| c.is_alphabetic() || c == '/' || c == '%').trim().parse().ok()?;
    let v = v.clamp(min, max);
    Some(if int { Val::Int(v.round() as i64) } else { Val::Float((v * 1000.0).round() / 1000.0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> Snapshot {
        Snapshot { cfg: Config::effective(), theme: ThemeSettings::default(), state: None }
    }

    #[test]
    fn every_page_builds_with_defaults() {
        let snap = snap();
        for (id, ..) in PAGES {
            let p = build(id, &snap);
            assert!(!p.sections.is_empty(), "{id}");
            assert!(p.sections.iter().all(|s| !s.rows.is_empty()), "{id}");
        }
    }

    #[test]
    fn number_parsing() {
        assert_eq!(parse_number("12 px", 0.0, 10.0, true), Some(Val::Int(10)));
        assert_eq!(parse_number("0.256", 0.0, 1.0, false), Some(Val::Float(0.256)));
        assert_eq!(parse_number("abc", 0.0, 1.0, false), None);
    }

    #[test]
    fn values_compare_across_int_and_float() {
        assert!(Val::Int(2).same(&Val::Float(2.0)));
        assert!(!Val::Int(2).same(&Val::Float(2.5)));
    }

    #[test]
    fn display_page_lists_outputs() {
        let mut snap = snap();
        snap.state = Some(State {
            outputs: vec![vesper_ipc::OutputInfo {
                name: "eDP-1".into(),
                mode: "2560x1440@60".into(),
                modes: vec!["2560x1440@60".into(), "1920x1080@60".into()],
                scale: 1.0,
                w: 2560,
                h: 1440,
                enabled: true,
                primary: true,
                ..Default::default()
            }],
            ..Default::default()
        });
        let p = build("display", &snap);
        assert!(p.sections[0].title.contains("eDP-1"));
        assert!(p.sections[0].rows.iter().any(|r| matches!(&r.ctl, Ctl::Choice { path, .. } if path == "display.outputs.\"eDP-1\".scale")));
    }
}

/// The bar's three sections as lists in screen order. The right list is stored right to left in the config.
pub type BarLayout = [Vec<String>; 3];

pub fn bar_layout(left: Vec<String>, middle: Vec<String>, right_rtl: Vec<String>) -> BarLayout {
    let mut seen: Vec<String> = Vec::new();
    let mut take = |v: Vec<String>| -> Vec<String> {
        v.into_iter().filter(|id| if seen.contains(id) { false } else { seen.push(id.clone()); true }).collect()
    };
    // the right section wins a duplicate, like the bar itself
    let right: Vec<String> = take(right_rtl).into_iter().rev().collect();
    let (left, middle) = (take(left), take(middle));
    [left, middle, right]
}

/// Show (dir 0, when hidden), hide (dir 0, when shown) or move an item one step along the bar (-1 left, 1 right).
/// An item at the end of a section moves into the neighbouring one.
pub fn apply_bar_move(l: &mut BarLayout, id: &str, dir: i8) {
    let pos = l.iter().enumerate().find_map(|(s, v)| v.iter().position(|x| x == id).map(|i| (s, i)));
    match (pos, dir) {
        (None, _) => {
            let s = match id {
                "system" | "workspaces" => 0,
                "minimized" => 1,
                _ => 2,
            };
            l[s].push(id.to_string());
        }
        (Some((s, i)), 0) => {
            l[s].remove(i);
        }
        (Some((s, i)), d) if d < 0 => {
            if i > 0 {
                l[s].swap(i, i - 1);
            } else if s > 0 {
                let it = l[s].remove(i);
                l[s - 1].push(it);
            }
        }
        (Some((s, i)), _) => {
            if i + 1 < l[s].len() {
                l[s].swap(i, i + 1);
            } else if s < 2 {
                let it = l[s].remove(i);
                l[s + 1].insert(0, it);
            }
        }
    }
}

#[cfg(test)]
mod bar_move_tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn items_cross_sections() {
        let mut l = bar_layout(v(&["system", "workspaces"]), v(&["minimized"]), v(&["clock", "bell"])); // right is stored right to left
        assert_eq!(l[2], v(&["bell", "clock"]));
        apply_bar_move(&mut l, "workspaces", 1); // end of left -> start of middle
        assert_eq!(l[1], v(&["workspaces", "minimized"]));
        apply_bar_move(&mut l, "bell", -1); // start of right -> end of middle
        assert_eq!(l[1], v(&["workspaces", "minimized", "bell"]));
        apply_bar_move(&mut l, "clock", 0); // hide
        assert!(l.iter().all(|s| !s.contains(&"clock".to_string())));
        apply_bar_move(&mut l, "clock", 0); // show again, in its natural place
        assert_eq!(l[2], v(&["clock"]));
        apply_bar_move(&mut l, "system", -1); // already first: stays
        assert_eq!(l[0], v(&["system"]));
    }
}

/// Cursor theme choice, a preview of it, and the size.
fn cursor_rows(snap: &Snapshot) -> Vec<Row> {
    let themes = vesper_config::cursor_themes();
    let mut options: Vec<(Val, String)> = vec![(Val::Str(String::new()), "Built-in arrow".into())];
    options.extend(themes.iter().map(|t| (Val::Str(t.clone()), t.replace('-', " "))));
    let current = snap.s("appearance.cursor_theme");
    let theme = if themes.contains(&current) { current.clone() } else { String::new() };
    let size = snap.f("appearance.cursor_size").max(12.0) as u32;
    vec![
        Row {
            title: "Cursor theme".into(),
            desc: "Bibata and Phinger cursors come with Vesper; any theme in ~/.local/share/icons shows up here too. Running apps keep their old cursor until restarted".into(),
            ctl: Ctl::Choice { path: "appearance.cursor_theme".into(), cur: Val::Str(current), options },
        },
        Row { title: "Preview".into(), desc: String::new(), ctl: Ctl::Cursors { theme, size } },
        num(snap, "Cursor size", "In pixels", "appearance.cursor_size", 16.0, 64.0, 4.0, true, " px"),
    ]
}

/// "3 days ago", for when a profile was saved.
fn ago(t: Option<std::time::SystemTime>) -> String {
    let Some(secs) = t.and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()) else { return "saved".into() };
    match secs {
        0..=59 => "saved just now".into(),
        60..=3599 => format!("saved {} min ago", secs / 60),
        3600..=86399 => format!("saved {} h ago", secs / 3600),
        _ => format!("saved {} days ago", secs / 86400),
    }
}

fn profiles_page() -> Vec<Section> {
    use vesper_config::profiles;
    let list = profiles::list();
    let mut saved: Vec<Row> = Vec::new();
    for p in &list {
        saved.push(Row {
            title: if p.active { format!("{}  (in use)", p.name) } else { p.name.clone() },
            desc: ago(p.modified),
            ctl: Ctl::Buttons(vec![
                ("Apply".into(), Act::ProfileApply(p.name.clone())),
                ("Share".into(), Act::ProfileShare(p.name.clone())),
                ("Delete".into(), Act::ProfileDelete(p.name.clone())),
            ]),
        });
    }
    if saved.is_empty() {
        saved.push(Row { title: "No profiles yet".into(), desc: "Save your current settings above, or import someone else's".into(), ctl: Ctl::Info(String::new()) });
    }
    vec![
        section(
            "Your settings",
            vec![
                Row {
                    title: "Save the current settings as a profile".into(),
                    desc: "Type a name and press Enter. Saving under an existing name replaces it".into(),
                    ctl: Ctl::Text { path: "profile.save.+".into(), val: String::new(), hint: "for example Dark and minimal".into() },
                },
                Row {
                    title: "Import a config".into(),
                    desc: "The path of a Vesper config file someone shared. It is added to the list below: apply it to use it".into(),
                    ctl: Ctl::Text { path: "profile.import.+".into(), val: String::new(), hint: "~/Downloads/their-config.toml".into() },
                },
            ],
        ),
        section("Saved profiles", saved),
        section(
            "About profiles",
            vec![Row {
                title: "Sharing".into(),
                desc: "Share leaves out monitors, wallpaper folders, autostart and pinned apps".into(),
                ctl: Ctl::Info(String::new()),
            }],
        ),
    ]
}

fn plugins_page(snap: &Snapshot) -> Vec<Section> {
    use vesper_config::plugins;
    let found = plugins::discover(&snap.list("plugins.enabled"));
    let mine = plugins::user_dir();
    let mut installed: Vec<Row> = Vec::new();
    for p in &found {
        let m = &p.manifest;
        let mut desc = m.description.clone();
        if !m.author.is_empty() {
            desc = format!("{desc}{}by {}", if desc.is_empty() { "" } else { " \u{b7} " }, m.author);
        }
        if p.too_new {
            desc = format!("Needs a newer Vesper (plugin API {})", m.api);
        } else {
            let can: Vec<&str> = plugins::PERMISSIONS.iter().copied().filter(|f| p.can(f)).collect();
            desc = format!("{desc}{}Allowed to: {}", if desc.is_empty() { "" } else { " \u{b7} " }, if can.is_empty() { "nothing".to_string() } else { can.join(", ") });
        }
        let mut buttons = vec![(if p.enabled { "Turn off" } else { "Turn on" }.to_string(), Act::PluginToggle(p.id.clone()))];
        if p.dir.starts_with(&mine) {
            buttons.push(("Remove".into(), Act::PluginRemove(p.id.clone())));
        }
        installed.push(Row {
            title: format!("{}{}{}", m.name, if m.version.is_empty() { String::new() } else { format!("  v{}", m.version) }, if p.enabled { "  (on)" } else { "" }),
            desc,
            ctl: Ctl::Buttons(buttons),
        });
    }
    if installed.is_empty() {
        installed.push(Row { title: "No plugins installed".into(), desc: "Add one below, or copy a plugin folder into the plugins folder".into(), ctl: Ctl::Info(String::new()) });
    }
    vec![
        section("Installed plugins", installed),
        section(
            "Add a plugin",
            vec![
                Row {
                    title: "Install from a folder or .tar.gz".into(),
                    desc: "The path of a plugin. It is copied in, turned off: turn it on above. Plugins run as you, so only add ones you trust".into(),
                    ctl: Ctl::Text { path: "plugin.install.+".into(), val: String::new(), hint: "~/Downloads/my-plugin".into() },
                },
                Row { title: "Plugins folder".into(), desc: "~/.config/vesper/plugins: one folder per plugin, each with a plugin.toml".into(), ctl: Ctl::Button { label: "Open".into(), act: Act::OpenPluginsFolder } },
            ],
        ),
        section(
            "Write your own",
            vec![Row {
                title: "A plugin is a folder with a plugin.toml".into(),
                desc: "It can run programs, react to desktop events, add shortcuts, bar widgets and launcher results. See docs/plugins.md and the plugin template".into(),
                ctl: Ctl::Info(String::new()),
            }],
        ),
    ]
}

/// One monitor in the arrangement map (logical pixels).
#[derive(Clone, Debug, PartialEq)]
pub struct MapMon {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub primary: bool,
}

/// Where monitor `idx` lands when dropped near `(x, y)`: edges snap to the other monitors' edges within 40 px, and it
/// never overlaps another monitor (it is pushed out along the shortest way).
pub fn snap_position(mons: &[MapMon], idx: usize, x: i32, y: i32) -> (i32, i32) {
    const NEAR: i32 = 40;
    let me = &mons[idx];
    let (mut bx, mut by) = (x, y);
    let (mut dx, mut dy) = (NEAR + 1, NEAR + 1);
    for (i, o) in mons.iter().enumerate().filter(|(i, _)| *i != idx) {
        let _ = i;
        for cand in [o.x - me.w, o.x + o.w, o.x, o.x + o.w - me.w] {
            if (cand - x).abs() < dx {
                dx = (cand - x).abs();
                bx = cand;
            }
        }
        for cand in [o.y - me.h, o.y + o.h, o.y, o.y + o.h - me.h] {
            if (cand - y).abs() < dy {
                dy = (cand - y).abs();
                by = cand;
            }
        }
    }
    // push out of any monitor it still overlaps
    for _ in 0..mons.len() {
        let Some(o) = mons.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, o)| o).find(|o| bx < o.x + o.w && bx + me.w > o.x && by < o.y + o.h && by + me.h > o.y) else { break };
        let moves = [(o.x - me.w - bx, 0), (o.x + o.w - bx, 0), (0, o.y - me.h - by), (0, o.y + o.h - by)];
        let (mx, my) = moves.into_iter().min_by_key(|(a, b)| a.abs() + b.abs()).unwrap();
        bx += mx;
        by += my;
    }
    (bx, by)
}

/// Save a new arrangement: shifted so the top-left of all monitors is 0,0, and written for every monitor.
pub fn apply_layout(mons: &[MapMon]) -> Result<(), String> {
    let (minx, miny) = (mons.iter().map(|m| m.x).min().unwrap_or(0), mons.iter().map(|m| m.y).min().unwrap_or(0));
    for m in mons {
        for (f, v) in [("x", m.x - minx), ("y", m.y - miny)] {
            Config::set(&format!("display.outputs.\"{}\".{f}", m.name), toml::Value::Integer(v as i64)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod monitor_map_tests {
    use super::*;

    fn mon(name: &str, x: i32, y: i32, w: i32, h: i32) -> MapMon {
        MapMon { name: name.into(), x, y, w, h, primary: false }
    }

    #[test]
    fn snaps_next_to_a_neighbour_and_never_overlaps() {
        let mons = vec![mon("A", 0, 0, 2560, 1440), mon("B", 2600, 100, 1920, 1080)];
        // dropped 30 px off the right edge, 20 px below the top: sits flush against A, top aligned
        assert_eq!(snap_position(&mons, 1, 2590, 20), (2560, 0));
        // dropped on top of A: pushed out
        let (x, y) = snap_position(&mons, 1, 1000, 300);
        assert!(x >= 2560 || x + 1920 <= 0 || y >= 1440 || y + 1080 <= 0, "{x},{y}");
        // far away: stays where it is
        assert_eq!(snap_position(&mons, 1, 5000, 900), (5000, 900));
    }
}
