//! vesper-launcher: an overlay under the top bar with three modes: Apps (search installed apps), Files (search your
//! files) and Run (type a command).
//!
//! Type to search, Up/Down (or Tab) to move, Left/Right to change mode, Enter to open or run, Ctrl+P to pin an app,
//! Shift+Enter (Run) to run in a terminal, Esc to cancel. The mouse works too. It is styled after the top bar.
//! The compositor toggles it with Mod+Space.

mod calc;
mod search;

use std::{collections::HashMap, num::NonZeroU32, path::PathBuf};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{channel, EventLoop},
        calloop_wayland_source::WaylandSource,
        client::{
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
            Connection, QueueHandle,
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use vesper_apps::{Apps, DesktopEntry, Icons};
use vesper_config::theme::{alpha, with_alpha, Palette};
use vesper_gfx::{Canvas, Image, Rgba, Text};

const PAD: i32 = 14;
const INPUT_H: i32 = 46;
const CHIP_H: i32 = 32;
/// Size of one emoji cell and how many rows of them show.
const CELL: i32 = 52;
const EMOJI_ROWS: usize = 5;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

const TILE_TEXT: Rgba = [0xff, 0xff, 0xff, 0xf0];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Apps,
    Files,
    Run,
    /// What you copied lately (Mod+Shift+V).
    Clipboard,
    /// A grid of emoji to search and copy.
    Emoji,
}

/// (mode, label, icon glyph, search hint)
const MODES: [(Mode, &str, &str, &str); 5] = [
    (Mode::Apps, "Apps", "\u{f0570}", "Search apps…"),
    (Mode::Files, "Files", "\u{f024b}", "Search your files…"),
    (Mode::Run, "Run", "\u{f018d}", "Type a command and press Enter"),
    (Mode::Clipboard, "Clipboard", "\u{f0ea}", "Search what you copied\u{2026}"),
    (Mode::Emoji, "Emoji", "", "Search emoji by name\u{2026}"),
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    /// Index into the current result list.
    Row(usize),
    Pin(usize),
    Chip(usize),
}

type Rect = (i32, i32, i32, i32);

struct FileEntry {
    path: String,
    /// Lower-case file name, for matching.
    name_lc: String,
    dir: bool,
}

fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("vesper")
}

/// How often and when each app was started, kept between runs: id -> (count, last time in seconds).
fn load_usage() -> HashMap<String, (u32, u64)> {
    let mut m = HashMap::new();
    for line in std::fs::read_to_string(state_dir().join("launcher-usage.tsv")).unwrap_or_default().lines() {
        let mut it = line.split('\t');
        if let (Some(id), Some(c), Some(t)) = (it.next(), it.next().and_then(|c| c.parse().ok()), it.next().and_then(|t| t.parse().ok())) {
            m.insert(id.to_string(), (c, t));
        }
    }
    m
}

fn save_usage(m: &HashMap<String, (u32, u64)>) {
    let dir = state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let text: String = m.iter().map(|(id, (c, t))| format!("{id}\t{c}\t{t}\n")).collect();
    let _ = std::fs::write(dir.join("launcher-usage.tsv"), text);
}

/// Commands run from Run mode, newest first.
fn load_history() -> Vec<String> {
    std::fs::read_to_string(state_dir().join("launcher-run-history.txt")).unwrap_or_default().lines().map(str::to_owned).filter(|l| !l.is_empty()).collect()
}

fn save_history(h: &[String]) {
    let dir = state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("launcher-run-history.txt"), h.join("\n"));
}

/// Walk the home folder (not hidden folders, build folders or very deep ones) for the Files mode.
fn index_files() -> Vec<FileEntry> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    let mut out = Vec::new();
    let mut stack = vec![(home, 0u32)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                if !matches!(name.as_str(), "node_modules" | "target" | "__pycache__" | "venv" | "build") && depth < 7 {
                    stack.push((path.clone(), depth + 1));
                }
                out.push(FileEntry { path: path.to_string_lossy().into_owned(), name_lc: name.to_lowercase(), dir: true });
            } else {
                out.push(FileEntry { path: path.to_string_lossy().into_owned(), name_lc: name.to_lowercase(), dir: false });
            }
            if out.len() >= 200_000 {
                return out;
            }
        }
    }
    out
}

struct Launcher {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    /// Transparent full-screen surface behind the card: a click on it (anywhere outside the launcher) closes it.
    backdrop: LayerSurface,
    backdrop_pool: SlotPool,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,

    width: u32,
    height: u32,
    configured: bool,
    exit: bool,

    apps: Vec<DesktopEntry>,
    icons: Icons,
    icon_cache: Vec<Option<Image>>,
    text: Text,
    small: Text,
    terminal: Option<String>,
    pal: Palette,

    mode: Mode,
    query: String,
    /// Apps mode: indices into `apps`, best match first.
    results: Vec<usize>,
    /// Files mode: the index of the home folder and the matches (indices into `files`).
    files: Vec<FileEntry>,
    file_results: Vec<usize>,
    files_started: bool,
    indexing: bool,
    /// Run mode: previously run commands (newest first) and what is listed now.
    history: Vec<String>,
    cmds: Vec<String>,
    /// Clipboard mode: the saved texts (newest first) and the ones matching the search.
    clips: Vec<String>,
    clip_results: Vec<usize>,
    /// Emoji mode: the bundled colour pictures (Twemoji SVGs by code point), those drawn so far, the emoji with their
    /// names, and the ones matching the search.
    emoji_svgs: HashMap<u32, &'static str>,
    emoji_imgs: HashMap<(u32, u32), Option<Image>>,
    emojis: Vec<(String, String)>,
    emoji_results: Vec<usize>,
    selected: usize,
    /// First visible result.
    scroll: usize,
    pinned: Vec<String>,
    usage: HashMap<String, (u32, u64)>,
    ctrl: bool,
    shift: bool,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    started: Option<std::time::Instant>,
    files_tx: channel::Sender<Vec<FileEntry>>,
    /// Search providers that plugins add (folder, source), the answers that came back for `asked`, and how many entries of `apps` are real.
    sources: Vec<(PathBuf, vesper_config::plugins::Source, String)>,
    src_tx: channel::Sender<(String, Vec<DesktopEntry>)>,
    dynamic: Vec<DesktopEntry>,
    asked: String,
    base_len: usize,

    // layout from `[launcher]`
    row_h: i32,
    max_rows: usize,
    icon: i32,

    // the top bar's look
    radius: f32,
    border_w: f32,
    border: Option<Rgba>,
    border_blend: bool,
    bg_alpha: u8,
}

fn main() -> Result<()> {
    let t0 = std::time::Instant::now();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = vesper_config::Config::load();

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    // Top centre, a little below the bar. Exclusive keyboard focus while it is open.
    let bar = &config.bar;
    let floating = matches!(bar.style.as_str(), "detached" | "islands");
    let bar_bottom = bar.height.max(16) as i32 + if floating { bar.margin_top.clamp(0, 80) as i32 } else { 0 };
    let width = config.launcher.width.clamp(400, 1100);
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("vesper-launcher"), None);
    if config.launcher.position == "center" {
        layer.set_anchor(Anchor::empty());
    } else {
        layer.set_anchor(Anchor::TOP);
        layer.set_margin(bar_bottom + config.launcher.gap.min(200) as i32, 0, 0, 0);
    }
    let height = (INPUT_H + CHIP_H + 3 * PAD + PAD / 2) as u32;
    layer.set_size(width, height);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();
    let backdrop = layer_shell.create_layer_surface(&qh, compositor.create_surface(&qh), Layer::Top, Some("vesper-launcher-backdrop"), None);
    backdrop.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    backdrop.set_size(0, 0);
    backdrop.set_exclusive_zone(-1);
    backdrop.set_keyboard_interactivity(KeyboardInteractivity::None);
    backdrop.commit();

    tracing::debug!("connected after {:?}", t0.elapsed());
    let mut apps: Vec<DesktopEntry> = Apps::load().entries().iter().filter(|e| !e.no_display).cloned().collect();
    // actions that enabled plugins add to the list
    let plugins_on = vesper_config::plugins::active(&config.plugins.enabled);
    let sources: Vec<(PathBuf, vesper_config::plugins::Source, String)> =
        plugins_on.iter().filter(|p| p.can("launcher")).flat_map(|p| p.manifest.launcher_source.iter().filter(|s| !s.command.trim().is_empty()).map(|s| (p.dir.clone(), s.clone(), p.id.clone()))).collect();
    for p in plugins_on.iter().filter(|p| p.can("launcher")) {
        for (n, a) in p.manifest.action.iter().enumerate().filter(|(_, a)| !a.name.trim().is_empty() && !a.command.trim().is_empty()) {
            let script = vesper_config::plugins::shell_in(&p.dir, &a.command).replace('\'', "'\\''");
            apps.push(DesktopEntry { id: format!("plugin-{}-{n}", p.id), name: a.name.clone(), exec: format!("sh -c '{script}'"), categories: vec!["Plugin".into()], ..Default::default() });
        }
    }
    apps.sort_by_key(|e| e.name.to_lowercase());
    apps.dedup_by(|a, b| a.name == b.name && a.exec == b.exec);
    let base_len = apps.len();

    let (files_tx, files_rx) = channel::channel::<Vec<FileEntry>>();
    let (src_tx, src_rx) = channel::channel::<(String, Vec<DesktopEntry>)>();
    let text = Text::new(&bar.font, 17.0);
    let mut launcher = Launcher {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(width as usize * 700 * 4, &shm)?,
        backdrop_pool: SlotPool::new(4096, &shm)?,
        shm,
        layer,
        backdrop,
        keyboard: None,
        pointer: None,
        width,
        height,
        configured: false,
        exit: false,
        icon_cache: vec![None; apps.len()],
        apps,
        icons: Icons::default(),
        small: text.resized(13.0),
        text,
        terminal: config.terminal_command(),
        pal: Palette::load(),
        mode: match std::env::args().skip_while(|a| a != "--mode").nth(1).as_deref() {
            Some("clipboard") => Mode::Clipboard,
            Some("emoji") => Mode::Emoji,
            _ => Mode::Apps,
        },
        query: String::new(),
        results: Vec::new(),
        files: Vec::new(),
        file_results: Vec::new(),
        files_started: false,
        indexing: false,
        history: load_history(),
        cmds: Vec::new(),
        clips: Vec::new(),
        clip_results: Vec::new(),
        emoji_svgs: load_emoji_pack(),
        emoji_imgs: HashMap::new(),
        emojis: load_emojis(),
        emoji_results: Vec::new(),
        selected: 0,
        scroll: 0,
        pinned: config.launcher.pinned.clone(),
        usage: load_usage(),
        ctrl: false,
        shift: false,
        hits: Vec::new(),
        hover: None,
        started: None,
        files_tx,
        sources,
        src_tx,
        dynamic: Vec::new(),
        asked: String::new(),
        base_len,
        radius: bar.radius.clamp(6, 30) as f32,
        row_h: config.launcher.row_height.clamp(32, 72) as i32,
        max_rows: config.launcher.rows.clamp(3, 14),
        icon: config.launcher.icon_size.clamp(16, 64) as i32,
        border_w: bar.border_width.clamp(0, 6).max(1) as f32,
        border: vesper_config::theme::hex(&bar.border_color),
        border_blend: bar.border_blend,
        bg_alpha: (bar.opacity.clamp(20, 100) * 255 / 100) as u8,
    };
    launcher.refilter();
    launcher.started = Some(t0);

    let mut event_loop: EventLoop<Launcher> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;
    // Remember the loop handle for key-repeat; the keyboard is created once the seat reports it.
    KEY_LOOP.with(|k| *k.borrow_mut() = Some(handle.clone()));
    // Answers from plugin search providers arrive from their threads.
    handle
        .insert_source(src_rx, |event, _, l: &mut Launcher| {
            if let channel::Event::Msg((query, found)) = event {
                if l.mode == Mode::Apps && query == l.query.trim() {
                    l.dynamic.extend(found);
                    l.refilter();
                }
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    // The file index arrives from its thread.
    handle
        .insert_source(files_rx, |event, _, l: &mut Launcher| {
            if let channel::Event::Msg(files) = event {
                l.files = files;
                l.indexing = false;
                if l.mode == Mode::Files {
                    l.refilter();
                }
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    // Follow matugen colour changes while open.
    handle
        .insert_source(
            smithay_client_toolkit::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_millis(250)),
            |_, _, l: &mut Launcher| {
                let p = Palette::load();
                if p != l.pal {
                    l.pal = p;
                    l.redraw();
                }
                smithay_client_toolkit::reexports::calloop::timer::TimeoutAction::ToDuration(
                    std::time::Duration::from_millis(250),
                )
            },
        )
        .map_err(|e| anyhow!("{e}"))?;

    while !launcher.exit {
        if let Err(err) = event_loop.dispatch(None, &mut launcher) {
            tracing::info!("compositor connection closed: {err}");
            break;
        }
    }
    Ok(())
}

thread_local! {
    static KEY_LOOP: std::cell::RefCell<Option<smithay_client_toolkit::reexports::calloop::LoopHandle<'static, Launcher>>> =
        const { std::cell::RefCell::new(None) };
}

/// The bundled emoji with their names (lower case).
fn load_emojis() -> Vec<(String, String)> {
    include_str!("../../../assets/emoji/emoji.txt")
        .lines()
        .filter_map(|l| {
            let (glyph, name) = l.split_once('\t')?;
            Some((glyph.to_string(), name.to_lowercase()))
        })
        .collect()
}

/// The picture of every emoji: `emoji.pack` is a series of "codepoint-in-hex length" lines, each followed by that many bytes of SVG.
fn load_emoji_pack() -> HashMap<u32, &'static str> {
    static PACK: &str = include_str!("../../../assets/emoji/emoji.pack");
    let mut map = HashMap::new();
    let mut rest = PACK;
    while let Some((head, tail)) = rest.split_once('\n') {
        let Some((cp, len)) = head.split_once(' ') else { break };
        let (Ok(cp), Ok(len)) = (u32::from_str_radix(cp, 16), len.parse::<usize>()) else { break };
        if len > tail.len() {
            break;
        }
        map.insert(cp, &tail[..len]);
        rest = tail[len..].trim_start_matches('\n');
    }
    map
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Spawn `cmd` through the shell, detached.
fn run_shell(cmd: &str) -> bool {
    match std::process::Command::new("sh").arg("-c").arg(cmd).stdin(std::process::Stdio::null()).spawn() {
        Ok(_) => true,
        Err(err) => {
            tracing::error!("failed to run {cmd}: {err}");
            false
        }
    }
}

impl Launcher {
    fn pin_pos(&self, app: usize) -> Option<usize> {
        let id = &self.apps[app].id;
        self.pinned.iter().position(|p| p == id)
    }

    fn count(&self) -> usize {
        match self.mode {
            Mode::Apps => self.results.len(),
            Mode::Files => self.file_results.len(),
            Mode::Run => self.cmds.len(),
            Mode::Clipboard => self.clip_results.len(),
            Mode::Emoji => self.emoji_results.len(),
        }
    }

    /// Ask the plugin search providers about the typed text (once per text), each in its own thread.
    fn ask_sources(&mut self) {
        let q = self.query.trim().to_string();
        if q == self.asked {
            return;
        }
        self.asked = q.clone();
        self.dynamic.clear();
        if q.is_empty() {
            return;
        }
        for (dir, src, id) in self.sources.clone() {
            let text = if src.prefix.is_empty() {
                q.clone()
            } else {
                match q.strip_prefix(src.prefix.as_str()) {
                    Some(rest) if rest.is_empty() || rest.starts_with(' ') => rest.trim().to_string(),
                    _ => continue,
                }
            };
            let (tx, q) = (self.src_tx.clone(), q.clone());
            std::thread::spawn(move || {
                let out = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(vesper_config::plugins::shell_in(&dir, &src.command))
                    .env("VESPER_PLUGIN_ID", &id)
                    .env("VESPER_PLUGIN_DIR", &dir)
                    .env("VESPER_QUERY", &text)
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output();
                let Ok(out) = out else { return };
                let found: Vec<DesktopEntry> = String::from_utf8_lossy(&out.stdout).lines().take(20).enumerate().filter_map(|(n, line)| parse_source_line(&dir, &id, n, line)).collect();
                if !found.is_empty() {
                    let _ = tx.send((q, found));
                }
            });
        }
    }

    fn refilter(&mut self) {
        let q = self.query.trim().to_lowercase();
        match self.mode {
            Mode::Apps => {
                self.apps.truncate(self.base_len);
                let mut r = search::rank(&self.apps, &self.query);
                self.ask_sources();
                // what plugin providers answered goes first
                let mut extra: Vec<usize> = Vec::new();
                for e in &self.dynamic {
                    extra.push(self.apps.len());
                    self.apps.push(e.clone());
                }
                r.splice(0..0, extra);
                // browsing: pinned first, then what you used lately, the rest alphabetical
                if q.is_empty() {
                    r.sort_by_key(|&i| {
                        let pin = self.pin_pos(i).unwrap_or(usize::MAX);
                        let last = self.usage.get(&self.apps[i].id).map_or(0, |u| u.1);
                        (pin, std::cmp::Reverse(last))
                    });
                }
                self.results = r;
            }
            Mode::Files => {
                if !self.files_started {
                    self.files_started = true;
                    self.indexing = true;
                    let tx = self.files_tx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(index_files());
                    });
                }
                let words: Vec<&str> = q.split_whitespace().collect();
                let mut scored: Vec<(u32, usize)> = Vec::new();
                if !words.is_empty() {
                    for (i, f) in self.files.iter().enumerate() {
                        let mut total = 0;
                        let mut ok = true;
                        for w in &words {
                            match f.name_lc.find(w) {
                                Some(0) => total += 30,
                                Some(p) if f.name_lc[..p].ends_with(|c: char| !c.is_alphanumeric()) => total += 20,
                                Some(_) => total += 10,
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok {
                            // shorter names and folders first among equals
                            scored.push((total * 10 + if f.dir { 5 } else { 0 } + 4u32.saturating_sub((f.name_lc.len() / 16) as u32), i));
                        }
                    }
                    scored.sort_by(|a, b| b.0.cmp(&a.0).then(self.files[a.1].name_lc.len().cmp(&self.files[b.1].name_lc.len())));
                    scored.truncate(300);
                }
                self.file_results = scored.into_iter().map(|(_, i)| i).collect();
            }
            Mode::Emoji => {
                let words: Vec<&str> = q.split_whitespace().collect();
                self.emoji_results = (0..self.emojis.len()).filter(|&i| words.iter().all(|w| self.emojis[i].1.contains(w))).collect();
            }
            Mode::Clipboard => {
                let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
                self.clips = std::fs::read_to_string(dir.join("vesper-clipboard.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                self.clip_results = (0..self.clips.len()).filter(|&i| q.is_empty() || self.clips[i].to_lowercase().contains(&q)).collect();
            }
            Mode::Run => {
                let typed = self.query.trim().to_string();
                let mut c: Vec<String> = Vec::new();
                // a calculation shows its answer first (Enter copies it); marked with a leading \u{1}
                if let Some(r) = calc::eval_text(&typed) {
                    c.push(format!("\u{1}{r}"));
                }
                if !typed.is_empty() {
                    c.push(typed.clone());
                }
                c.extend(self.history.iter().filter(|h| **h != typed && (q.is_empty() || h.to_lowercase().contains(&q))).take(20).cloned());
                self.cmds = c;
            }
        }
        self.selected = 0;
        self.scroll = 0;
        self.resize();
    }

    fn visible_rows(&self) -> usize {
        if self.mode == Mode::Emoji {
            return 0; // drawn as a grid
        }
        self.count().saturating_sub(self.scroll).min(self.max_rows)
    }

    /// Resize the surface to fit the result list.
    fn resize(&mut self) {
        let rows = self.count().min(self.max_rows).max(1) as i32;
        let body = if self.mode == Mode::Emoji { EMOJI_ROWS as i32 * CELL + 26 } else { rows * self.row_h };
        let h = (PAD + INPUT_H + PAD / 2 + CHIP_H + PAD / 2 + body + PAD) as u32;
        if h != self.height || !self.configured {
            self.height = h;
            self.layer.set_size(self.width, h);
            self.layer.commit(); // the configure that follows redraws
        } else {
            self.redraw();
        }
    }

    fn set_mode(&mut self, m: Mode) {
        if self.mode != m {
            self.mode = m;
            self.refilter();
        }
    }

    fn cycle_mode(&mut self, d: isize) {
        let i = MODES.iter().position(|(m, ..)| *m == self.mode).unwrap_or(0) as isize;
        self.set_mode(MODES[(i + d).rem_euclid(MODES.len() as isize) as usize].0);
    }

    fn toggle_pin(&mut self, result: usize) {
        if self.mode != Mode::Apps {
            return;
        }
        let Some(&app) = self.results.get(result) else { return };
        let id = self.apps[app].id.clone();
        match self.pinned.iter().position(|p| *p == id) {
            Some(i) => {
                self.pinned.remove(i);
            }
            None => self.pinned.push(id),
        }
        let value = toml::Value::Array(self.pinned.iter().cloned().map(toml::Value::String).collect());
        if let Err(e) = vesper_config::Config::set("launcher.pinned", value) {
            tracing::warn!("could not save the pinned apps: {e}");
        }
        // re-sort, then stay on the same app
        let sel_app = self.results.get(self.selected).copied();
        let scroll = self.scroll;
        self.refilter();
        if let Some(p) = sel_app.and_then(|a| self.results.iter().position(|&i| i == a)) {
            self.selected = p;
            self.scroll = scroll.min(self.results.len().saturating_sub(self.max_rows));
            self.ensure_visible();
        }
        self.redraw();
    }

    fn ensure_visible(&mut self) {
        if self.mode == Mode::Emoji {
            let row = self.selected / self.grid_cols();
            if row < self.scroll {
                self.scroll = row;
            } else if row >= self.scroll + EMOJI_ROWS {
                self.scroll = row + 1 - EMOJI_ROWS;
            }
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.max_rows {
            self.scroll = self.selected + 1 - self.max_rows;
        }
    }

    fn key(&mut self, event: KeyEvent) {
        if self.ctrl && matches!(event.keysym, Keysym::p | Keysym::P) {
            self.toggle_pin(self.selected);
            return;
        }
        // Ctrl+Left/Right always switch mode; in the emoji grid the plain arrows move the selection
        if self.ctrl && matches!(event.keysym, Keysym::Left | Keysym::Right) {
            self.cycle_mode(if event.keysym == Keysym::Right { 1 } else { -1 });
            return;
        }
        if self.mode == Mode::Emoji {
            let cols = self.grid_cols() as isize;
            match event.keysym {
                Keysym::Down => return self.step_grid(cols),
                Keysym::Up => return self.step_grid(-cols),
                Keysym::Right | Keysym::Tab => return self.step_grid(1),
                Keysym::Left | Keysym::ISO_Left_Tab => return self.step_grid(-1),
                _ => {}
            }
        }
        match event.keysym {
            Keysym::Escape => self.exit = true,
            Keysym::Return | Keysym::KP_Enter => self.launch(self.selected),
            Keysym::Down | Keysym::Tab => self.step(1),
            Keysym::Up | Keysym::ISO_Left_Tab => self.step(-1),
            Keysym::Right => self.cycle_mode(1),
            Keysym::Left => self.cycle_mode(-1),
            Keysym::BackSpace => {
                if self.query.pop().is_some() {
                    self.refilter();
                }
            }
            _ if self.ctrl => {}
            _ => {
                if let Some(s) = event.utf8 {
                    let printable: String = s.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() {
                        self.query.push_str(&printable);
                        self.refilter();
                    }
                }
            }
        }
    }

    fn grid_cols(&self) -> usize {
        ((self.width as i32 - 2 * PAD) / CELL).max(1) as usize
    }

    /// Move through the emoji grid without wrapping round.
    fn step_grid(&mut self, d: isize) {
        let n = self.emoji_results.len() as isize;
        if n > 0 {
            self.selected = (self.selected as isize + d).clamp(0, n - 1) as usize;
            self.ensure_visible();
            self.redraw();
        }
    }

    fn step(&mut self, d: isize) {
        let n = self.count() as isize;
        if n > 0 {
            self.selected = (self.selected as isize + d).rem_euclid(n) as usize;
            self.ensure_visible();
            self.redraw();
        }
    }

    /// Open the result at `result` (start the app, open the file, run the command).
    fn launch(&mut self, result: usize) {
        match self.mode {
            Mode::Apps => {
                let Some(&i) = self.results.get(result) else { return };
                let app = &self.apps[i];
                let mut cmd = app.exec.clone();
                if app.terminal {
                    match &self.terminal {
                        Some(t) => cmd = format!("{t} -e {cmd}"),
                        None => tracing::warn!("{} needs a terminal but none was found", app.name),
                    }
                }
                tracing::info!("launching {cmd}");
                if !run_shell(&format!("exec {cmd}")) {
                    return;
                }
                let e = self.usage.entry(app.id.clone()).or_insert((0, 0));
                *e = (e.0 + 1, now_secs());
                save_usage(&self.usage);
            }
            Mode::Files => {
                let Some(&i) = self.file_results.get(result) else { return };
                let path = self.files[i].path.clone();
                if std::process::Command::new("xdg-open").arg(&path).stdin(std::process::Stdio::null()).spawn().is_err() {
                    tracing::error!("xdg-open is not installed, cannot open {path}");
                    return;
                }
            }
            Mode::Emoji => {
                let Some(&i) = self.emoji_results.get(result) else { return };
                let (glyph, name) = self.emojis[i].clone();
                send_to_compositor(vesper_ipc::Command::Clipboard { text: glyph, summary: "Emoji copied".into(), body: name });
            }
            Mode::Clipboard => {
                let Some(&i) = self.clip_results.get(result) else { return };
                let text = self.clips[i].clone();
                let preview: String = text.lines().next().unwrap_or("").chars().take(80).collect();
                send_to_compositor(vesper_ipc::Command::Clipboard { text, summary: "Copied".into(), body: preview });
            }
            Mode::Run => {
                let Some(cmd) = self.cmds.get(result).cloned() else { return };
                if let Some(answer) = cmd.strip_prefix('\u{1}') {
                    send_to_compositor(vesper_ipc::Command::Clipboard {
                        text: answer.to_string(),
                        summary: "Result copied".into(),
                        body: format!("{} = {answer}", self.query.trim()),
                    });
                    self.exit = true;
                    return;
                }
                let full = match (&self.terminal, self.shift) {
                    (Some(t), true) => format!("{t} -e sh -c '{}'", cmd.replace('\'', "'\\''")),
                    _ => cmd.clone(),
                };
                if !run_shell(&full) {
                    return;
                }
                self.history.retain(|h| *h != cmd);
                self.history.insert(0, cmd);
                self.history.truncate(100);
                save_history(&self.history);
            }
        }
        self.exit = true; // the child is reparented when we exit
    }

    /// The colour picture of the emoji with code point `cp`, `px` pixels square (drawn once, then kept).
    fn emoji_image(&mut self, cp: u32, px: u32) -> Option<Image> {
        let svg = self.emoji_svgs.get(&cp).copied();
        self.emoji_imgs.entry((cp, px)).or_insert_with(|| svg.and_then(|s| vesper_apps::icons::render_svg_data(s, px, px))).clone()
    }

    fn icon_for(&mut self, i: usize) -> Option<Image> {
        if self.icon_cache[i].is_none() {
            let name = self.apps[i].icon.clone();
            self.icon_cache[i] = self.icons.get(&name, self.icon);
        }
        self.icon_cache[i].clone()
    }

    fn redraw(&mut self) {
        if !self.configured || self.width == 0 {
            return;
        }
        let (w, h) = (self.width, self.height);
        // Resolve icons before borrowing the buffer pool.
        let rows = self.visible_rows();
        let first = self.scroll;
        let total = self.count();
        // the emoji on show (and the one on the Emoji tab) are drawn before the canvas is borrowed
        let chip_emoji = self.emoji_image(0x1f600, 18);
        let grid_imgs: Vec<Option<Image>> = if self.mode == Mode::Emoji {
            let cols = self.grid_cols();
            let range = (self.scroll * cols)..((self.scroll + EMOJI_ROWS) * cols).min(self.emoji_results.len());
            range.map(|idx| self.emojis[self.emoji_results[idx]].0.chars().next().and_then(|ch| self.emoji_image(ch as u32, 34))).collect()
        } else {
            Vec::new()
        };
        let icons: Vec<Option<Image>> = if self.mode == Mode::Apps { (0..rows).map(|r| self.icon_for(self.results[first + r])).collect() } else { Vec::new() };
        let Ok((buffer, canvas)) =
            self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            tracing::warn!("could not allocate a {w}x{h} buffer");
            return;
        };
        let mut c = Canvas::new(canvas, w, h);
        c.clear([0, 0, 0, 0]);
        let p = &self.pal;
        let accent = p.primary;
        let mut hits: Vec<(Rect, Hit)> = Vec::new();

        // the box: the bar's fill, radius, border and see-through level
        c.rounded_rect(0, 0, w as i32, h as i32, self.radius, alpha(p.surface, self.bg_alpha));
        let mut border = self.border.unwrap_or(accent);
        if self.border_blend {
            border[3] = (border[3] as u32 * self.bg_alpha as u32 / 255) as u8;
        }
        c.rounded_outline(0, 0, w as i32, h as i32, self.radius, self.border_w, border);

        // Search field
        let (fx, fy, fw) = (PAD, PAD, w as i32 - 2 * PAD);
        c.rounded_rect(fx, fy, fw, INPUT_H, (self.radius - 4.0).max(8.0), p.surface_container_high);
        let mid = fy + INPUT_H / 2;
        let tx = fx + 16;
        let hint = MODES.iter().find(|(m, ..)| *m == self.mode).map_or("", |m| m.3);
        if self.query.is_empty() {
            self.text.draw(&mut c, tx, mid, hint, p.on_surface_variant);
        } else {
            let shown = self.text.fit(&self.query, fw - 40);
            self.text.draw(&mut c, tx, mid, &shown, p.on_surface);
            let cx = tx + self.text.width(&shown) + 2;
            c.rounded_rect(cx, mid - 10, 2, 20, 1.0, accent);
        }
        if self.query.is_empty() {
            c.rounded_rect(tx - 2, mid - 10, 2, 20, 1.0, accent);
        }

        // Mode chips: an icon and a name
        let cy = fy + INPUT_H + PAD / 2;
        let mut x = PAD;
        for (i, (m, label, glyph, _)) in MODES.iter().enumerate() {
            let cw = self.small.width(label) + 54;
            let r = (x, cy, cw, CHIP_H);
            let on = *m == self.mode;
            let hot = self.hover == Some(Hit::Chip(i));
            let fill = if on { accent } else if hot { p.surface_container_highest } else { p.surface_container_high };
            c.rounded_rect(r.0, r.1, r.2, r.3, CHIP_H as f32 / 2.0, fill);
            let fg = if on { p.on_primary } else { p.on_surface_variant };
            match (*m, &chip_emoji) {
                (Mode::Emoji, Some(img)) => c.image(x + 21 - img.w / 2, cy + CHIP_H / 2 - img.h / 2, img),
                _ => self.text.draw_centered(&mut c, x + 21, cy + CHIP_H / 2, glyph, fg),
            }
            self.small.draw(&mut c, x + 36, cy + CHIP_H / 2, label, fg);
            hits.push((r, Hit::Chip(i)));
            x += cw + 8;
        }
        let hint_keys = match self.mode {
            Mode::Apps => "Ctrl+P pins",
            Mode::Files => "",
            Mode::Run => "Shift+Enter: in a terminal",
            Mode::Clipboard => "Enter copies it again",
            Mode::Emoji => "Enter copies",
        };
        let hw = self.small.width(hint_keys);
        if !hint_keys.is_empty() && x + hw + 12 < w as i32 - PAD - hw {
            self.small.draw(&mut c, w as i32 - PAD - hw, cy + CHIP_H / 2, hint_keys, with_alpha(p.on_surface_variant, 0.8));
        }

        // Results
        let top = cy + CHIP_H + PAD / 2;
        // the emoji grid
        if self.mode == Mode::Emoji {
            let cols = ((w as i32 - 2 * PAD) / CELL).max(1) as usize;
            let gx = PAD + ((w as i32 - 2 * PAD) - cols as i32 * CELL) / 2;
            {
                for r in 0..EMOJI_ROWS {
                    for col in 0..cols {
                        let idx = (self.scroll + r) * cols + col;
                        if idx >= total {
                            break;
                        }
                        let (x, y) = (gx + col as i32 * CELL, top + r as i32 * CELL);
                        let sel = idx == self.selected;
                        if sel {
                            c.rounded_rect(x + 2, y + 2, CELL - 4, CELL - 4, 10.0, with_alpha(accent, 0.30));
                        }
                        if let Some(Some(img)) = grid_imgs.get(idx - self.scroll * cols) {
                            c.image(x + (CELL - img.w) / 2, y + (CELL - img.h) / 2, img);
                        }
                        hits.push(((x, y, CELL, CELL), Hit::Row(idx)));
                    }
                }
            }
            if let Some(&i) = self.emoji_results.get(self.selected) {
                let name = self.emojis[i].1.clone();
                self.small.draw(&mut c, PAD + 4, top + EMOJI_ROWS as i32 * CELL + 14, &self.small.fit(&name, w as i32 - 2 * PAD - 8), p.on_surface_variant);
            }
        }
        if total == 0 {
            let msg = match self.mode {
                Mode::Apps => "No matching apps",
                Mode::Files if self.indexing => "Reading your files…",
                Mode::Files if self.query.trim().is_empty() => "Type to search your home folder",
                Mode::Files => "No matching files",
                Mode::Run => "Type a command and press Enter",
                Mode::Emoji => "No matching emoji",
                Mode::Clipboard if self.query.trim().is_empty() => "Nothing copied yet",
                Mode::Clipboard => "No matching entries",
            };
            self.small.draw_centered(&mut c, w as i32 / 2, top + self.row_h / 2, &self.small.fit(msg, w as i32 - 2 * PAD), p.on_surface_variant);
        }
        let home = std::env::var("HOME").unwrap_or_default();
        for row in 0..rows {
            let idx = first + row;
            let y = top + row as i32 * self.row_h;
            let selected = idx == self.selected;
            let rr = (PAD, y, w as i32 - 2 * PAD, self.row_h - 2);
            if selected {
                c.rounded_rect(rr.0, rr.1, rr.2, rr.3, (self.radius - 4.0).max(8.0), with_alpha(accent, 0.28));
            }
            hits.push((rr, Hit::Row(idx)));
            let iy = y + (self.row_h - 2 - self.icon) / 2;
            let ymid = y + (self.row_h - 2) / 2;
            let text_x = PAD + 12 + self.icon + 12;
            match self.mode {
                Mode::Emoji => {}
                Mode::Apps => {
                    let i = self.results[idx];
                    match &icons[row] {
                        Some(img) => c.image(PAD + 12, iy, img),
                        None => {
                            let name = self.apps[i].name.clone();
                            c.rounded_rect(PAD + 12, iy, self.icon, self.icon, self.icon as f32 * 0.24, search::tile_color(&name));
                            let initial: String = name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
                            self.small.draw_centered(&mut c, PAD + 12 + self.icon / 2, iy + self.icon / 2, &initial, TILE_TEXT);
                        }
                    }
                    // the star: filled when pinned, a faint outline on the selected row to show it can be pinned
                    let star = (w as i32 - PAD - 40, y + 4, 34, self.row_h - 10);
                    let pinned = self.pinned.contains(&self.apps[i].id);
                    let star_hot = self.hover == Some(Hit::Pin(idx));
                    if pinned || selected || star_hot {
                        let glyph = if pinned { "\u{f04ce}" } else { "\u{f04d2}" };
                        let col = if pinned || star_hot { accent } else { with_alpha(p.on_surface_variant, 0.7) };
                        self.text.draw_centered(&mut c, star.0 + star.2 / 2, ymid, glyph, col);
                    }
                    hits.push((star, Hit::Pin(idx)));
                    let name = self.text.fit(&self.apps[i].name, w as i32 - 2 * PAD - 12 - self.icon - 24 - 40);
                    self.text.draw(&mut c, text_x, ymid, &name, p.on_surface);
                }
                Mode::Files => {
                    let f = &self.files[self.file_results[idx]];
                    let glyph = if f.dir { "\u{f024b}" } else { "\u{f0214}" };
                    self.text.draw_centered(&mut c, PAD + 12 + self.icon / 2, ymid, glyph, accent);
                    let path = f.path.strip_prefix(&home).map(|r| format!("~{r}")).unwrap_or_else(|| f.path.clone());
                    let (dir, name) = path.rsplit_once('/').map_or(("", path.as_str()), |(d, n)| (d, n));
                    let avail = w as i32 - 2 * PAD - 12 - self.icon - 24 - 10;
                    self.text.draw(&mut c, text_x, ymid - 8, &self.text.fit(name, avail), p.on_surface);
                    self.small.draw(&mut c, text_x, ymid + 11, &self.small.fit(dir, avail), p.on_surface_variant);
                }
                Mode::Clipboard => {
                    let text = &self.clips[self.clip_results[idx]];
                    self.text.draw_centered(&mut c, PAD + 12 + self.icon / 2, ymid, "\u{f0ea}", accent);
                    let avail = w as i32 - 2 * PAD - 12 - self.icon - 24 - 10;
                    let first = text.lines().next().unwrap_or("");
                    let extra = text.lines().count().saturating_sub(1);
                    self.text.draw(&mut c, text_x, ymid - if extra > 0 { 8 } else { 0 }, &self.text.fit(first, avail), p.on_surface);
                    if extra > 0 {
                        self.small.draw(&mut c, text_x, ymid + 11, &format!("+ {extra} more lines"), p.on_surface_variant);
                    }
                }
                Mode::Run => {
                    let cmd = &self.cmds[idx];
                    if let Some(answer) = cmd.strip_prefix('\u{1}') {
                        self.text.draw_centered(&mut c, PAD + 12 + self.icon / 2, ymid, "\u{f00ec}", accent);
                        let avail = w as i32 - 2 * PAD - 12 - self.icon - 24 - 10;
                        self.text.draw(&mut c, text_x, ymid, &self.text.fit(&format!("= {answer}"), avail), p.on_surface);
                        let hint = "Enter copies";
                        self.small.draw(&mut c, w as i32 - PAD - 12 - self.small.width(hint), ymid, hint, p.on_surface_variant);
                        continue;
                    }
                    let typed = idx == 0 && cmd.as_str() == self.query.trim();
                    self.text.draw_centered(&mut c, PAD + 12 + self.icon / 2, ymid, if typed { "\u{f040a}" } else { "\u{f02da}" }, accent);
                    let avail = w as i32 - 2 * PAD - 12 - self.icon - 24 - 10;
                    let label = if typed { format!("Run  {cmd}") } else { cmd.clone() };
                    self.text.draw(&mut c, text_x, ymid, &self.text.fit(&label, avail), p.on_surface);
                }
            }
        }
        // hits are tested last-first so the star wins over its row
        self.hits = hits;

        self.layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(self.layer.wl_surface()).is_ok() {
            self.layer.commit();
        }
    }

    fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        let (x, y) = (x as i32, y as i32);
        self.hits.iter().rev().find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh).map(|(_, h)| *h)
    }
}

impl LayerShellHandler for Launcher {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if layer.wl_surface() == self.backdrop.wl_surface() {
            let (w, h) = (c.new_size.0.max(1), c.new_size.1.max(1));
            if let Ok((buffer, canvas)) = self.backdrop_pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) {
                canvas.fill(0);
                self.backdrop.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
                if buffer.attach_to(self.backdrop.wl_surface()).is_ok() {
                    self.backdrop.commit();
                }
            }
            return;
        }
        self.width = NonZeroU32::new(c.new_size.0).map_or(self.width, NonZeroU32::get);
        self.height = NonZeroU32::new(c.new_size.1).map_or(self.height, NonZeroU32::get);
        let first = !self.configured;
        self.configured = true;
        self.redraw();
        if let (true, Some(t0)) = (first, self.started) {
            tracing::debug!("first frame after {:?}", t0.elapsed());
        }
    }
}

impl PointerHandler for Launcher {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            if &e.surface == self.backdrop.wl_surface() {
                if matches!(e.kind, PointerEventKind::Press { .. }) {
                    self.exit = true;
                }
                continue;
            }
            if &e.surface != self.layer.wl_surface() {
                continue;
            }
            match e.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    let hover = self.hit_at(e.position.0, e.position.1);
                    if hover != self.hover {
                        self.hover = hover;
                        if let Some(Hit::Row(i) | Hit::Pin(i)) = hover {
                            self.selected = i;
                        }
                        self.redraw();
                    }
                }
                PointerEventKind::Leave { .. } => {
                    if self.hover.take().is_some() {
                        self.redraw();
                    }
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => match self.hit_at(e.position.0, e.position.1) {
                    Some(Hit::Row(i)) => self.launch(i),
                    Some(Hit::Pin(i)) => self.toggle_pin(i),
                    Some(Hit::Chip(i)) => self.set_mode(MODES[i].0),
                    None => {}
                },
                PointerEventKind::Press { button: BTN_RIGHT, .. } => {
                    if let Some(Hit::Row(i) | Hit::Pin(i)) = self.hit_at(e.position.0, e.position.1) {
                        self.toggle_pin(i);
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let d = if vertical.discrete != 0 { vertical.discrete.signum() as isize } else if vertical.absolute > 0.0 { 1 } else if vertical.absolute < 0.0 { -1 } else { 0 };
                    let max = if self.mode == Mode::Emoji { self.count().div_ceil(self.grid_cols()).saturating_sub(EMOJI_ROWS) } else { self.count().saturating_sub(self.max_rows) };
                    let s = (self.scroll as isize + d * 2).clamp(0, max as isize) as usize;
                    if s != self.scroll {
                        self.scroll = s;
                        self.hover = None;
                        self.redraw();
                    }
                }
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for Launcher {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        // Clicking elsewhere or switching away dismisses it, like most launchers.
        if self.configured {
            self.exit = true;
        }
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key(event);
    }

    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key(event);
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, modifiers: Modifiers, _: RawModifiers, _: u32) {
        self.ctrl = modifiers.ctrl;
        self.shift = modifiers.shift;
    }
}

impl SeatHandler for Launcher {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let handle = KEY_LOOP.with(|k| k.borrow().clone());
            self.keyboard = match handle {
                Some(h) => self
                    .seat_state
                    .get_keyboard_with_repeat(qh, &seat, None, h, Box::new(|l: &mut Launcher, _, ev| l.key(ev)))
                    .ok(),
                None => self.seat_state.get_keyboard(qh, &seat, None).ok(),
            };
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for Launcher {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Launcher {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Launcher {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Launcher);
delegate_output!(Launcher);
delegate_shm!(Launcher);
delegate_seat!(Launcher);
delegate_keyboard!(Launcher);
delegate_pointer!(Launcher);
delegate_layer!(Launcher);
delegate_registry!(Launcher);

impl ProvidesRegistryState for Launcher {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

/// Hand a command to the compositor over its socket (it copies text and shows notifications for us).
fn send_to_compositor(cmd: vesper_ipc::Command) {
    use std::io::Write;
    let path = vesper_ipc::socket_path();
    if let Ok(mut sock) = std::os::unix::net::UnixStream::connect(&path) {
        if let Ok(json) = serde_json::to_string(&cmd) {
            let _ = writeln!(sock, "{json}");
            let _ = sock.flush();
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// One line of a plugin search provider: `title`, `title|description` or `title|description|command`.
fn parse_source_line(dir: &std::path::Path, plugin: &str, n: usize, line: &str) -> Option<DesktopEntry> {
    let mut parts = line.splitn(3, '|');
    let title = parts.next()?.trim();
    if title.is_empty() {
        return None;
    }
    let desc = parts.next().unwrap_or("").trim();
    let command = parts.next().map(str::trim).filter(|c| !c.is_empty()).unwrap_or(title);
    let script = vesper_config::plugins::shell_in(dir, command).replace('\'', "'\\''");
    let name = if desc.is_empty() { title.to_string() } else { format!("{title} \u{2013} {desc}") };
    Some(DesktopEntry { id: format!("plugin-result-{plugin}-{n}"), name, exec: format!("sh -c '{script}'"), categories: vec!["Plugin".into()], ..Default::default() })
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn source_lines() {
        let d = std::path::Path::new("/tmp");
        let e = parse_source_line(d, "p", 0, "Title|about|echo hi").unwrap();
        assert_eq!(e.name, "Title \u{2013} about");
        assert!(e.exec.contains("echo hi"));
        assert!(parse_source_line(d, "p", 1, " |x").is_none());
        assert!(parse_source_line(d, "p", 2, "Only a title").unwrap().exec.contains("Only a title"));
    }
}
