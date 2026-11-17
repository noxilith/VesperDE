//! Bar widgets that plugins define: a command is run every few seconds and its first line is shown.

use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use vesper_config::plugins;

use crate::dbus::Dbus;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// `plugin:<folder>/<id>`, as listed in `bar.*_items`.
    pub id: &'static str,
    pub plugin: String,
    pub label: String,
    pub dir: PathBuf,
    pub command: String,
    pub interval: u64,
    pub on_click: String,
}

/// Every bar widget of the enabled plugins.
pub fn load(enabled: &[String]) -> Vec<Item> {
    let mut out = Vec::new();
    for p in plugins::active(enabled) {
        for b in &p.manifest.bar_item {
            out.push(Item {
                id: vesper_config::intern(&plugins::item_id(&p.id, &b.id)),
                plugin: p.id.clone(),
                label: if b.label.is_empty() { format!("{}: {}", p.manifest.name, b.id) } else { b.label.clone() },
                dir: p.dir.clone(),
                command: b.command.clone(),
                interval: b.interval.clamp(1, 3600) as u64,
                on_click: b.on_click.clone(),
            });
        }
    }
    out
}

/// Run `command` in the plugin's folder and return the first line it prints (empty if it fails).
pub fn run_once(item: &Item) -> String {
    let out = Command::new("sh")
        .arg("-c")
        .arg(plugins::shell_in(&item.dir, &item.command))
        .env("VESPER_PLUGIN_ID", &item.plugin)
        .env("VESPER_PLUGIN_DIR", &item.dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().chars().take(60).collect(),
        _ => String::new(),
    }
}

/// Click action of a widget: run it detached.
pub fn click(item: &Item) {
    if item.on_click.trim().is_empty() {
        return;
    }
    if let Ok(mut child) = Command::new("sh")
        .arg("-c")
        .arg(plugins::shell_in(&item.dir, &item.on_click))
        .env("VESPER_PLUGIN_ID", &item.plugin)
        .env("VESPER_PLUGIN_DIR", &item.dir)
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// One thread per widget that is on the bar, each looping until told to stop.
#[derive(Default)]
pub struct Runner {
    stops: HashMap<&'static str, Arc<AtomicBool>>,
}

impl Runner {
    /// Make the threads match `items` (the widgets that are placed on the bar).
    pub fn sync(&mut self, items: &[Item], tx: &Sender<Dbus>) {
        self.stops.retain(|id, stop| {
            let keep = items.iter().any(|i| i.id == *id);
            if !keep {
                stop.store(true, Ordering::Relaxed);
            }
            keep
        });
        for item in items {
            if self.stops.contains_key(item.id) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            self.stops.insert(item.id, stop.clone());
            let (item, tx) = (item.clone(), tx.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if tx.send(Dbus::Plugin(item.id, run_once(&item))).is_err() {
                        return;
                    }
                    // sleep in small steps so a removed widget stops promptly
                    for _ in 0..item.interval * 4 {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            });
        }
    }
}
