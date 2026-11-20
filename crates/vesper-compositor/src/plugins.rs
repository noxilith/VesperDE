//! Starting and stopping the background programs of enabled plugins (`run` in `plugin.toml`), and running their hooks and shortcuts.

use std::{collections::HashMap, process::Child};

use vesper_config::plugins;

use crate::state::Vesper;

/// What the running plugin programs are, by plugin id.
#[derive(Default)]
pub struct Running {
    children: HashMap<String, Child>,
    /// The plugins that are on (read again by `plugins_sync`): their hooks and shortcuts are looked up here.
    pub active: Vec<plugins::Plugin>,
}

fn kill_group(child: &mut Child) {
    // the program was started in its own process group, so its helpers go too
    let _ = std::process::Command::new("kill").args(["--", &format!("-{}", child.id())]).status();
    let _ = child.kill();
    let _ = child.wait();
}

impl Running {
    pub fn stop_all(&mut self) {
        for (_, mut c) in self.children.drain() {
            kill_group(&mut c);
        }
    }
}

impl Vesper {
    /// Start the `run` program of every enabled plugin that is not running, and stop those that are no longer wanted.
    pub fn plugins_sync(&mut self) {
        use std::os::unix::process::CommandExt;
        self.plugins.active = plugins::active(&self.config.plugins.enabled);
        crate::state::set_replacements(plugins::replacements(&self.config.plugins.enabled).into_iter().map(|(name, p, command)| (name, p.dir.clone(), command, p.id.clone())).collect());
        let want: Vec<plugins::Plugin> = self.plugins.active.iter().filter(|p| p.can("run") && !p.manifest.run.trim().is_empty()).cloned().collect();
        let ids: Vec<String> = want.iter().map(|p| p.id.clone()).collect();
        let stale: Vec<String> = self.plugins.children.keys().filter(|id| !ids.contains(id)).cloned().collect();
        for id in stale {
            if let Some(mut c) = self.plugins.children.remove(&id) {
                tracing::info!("plugin {id}: stopped");
                kill_group(&mut c);
            }
        }
        // one that has exited is started again on the next change of settings (not in a loop)
        self.plugins.children.retain(|_, c| matches!(c.try_wait(), Ok(None)));
        for p in want {
            if self.plugins.children.contains_key(&p.id) {
                continue;
            }
            let log_dir = std::env::var_os("XDG_STATE_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))
                .unwrap_or_else(|| "/tmp".into())
                .join("vesper");
            let _ = std::fs::create_dir_all(&log_dir);
            let log = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join(format!("plugin-{}.log", p.id)));
            let (out, err) = match log.and_then(|f| f.try_clone().map(|g| (f, g))) {
                Ok((a, b)) => (std::process::Stdio::from(a), std::process::Stdio::from(b)),
                Err(_) => (std::process::Stdio::null(), std::process::Stdio::null()),
            };
            let cmd = plugins::shell_in(&p.dir, &p.manifest.run);
            match std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .env("VESPER_PLUGIN_ID", &p.id)
                .env("VESPER_PLUGIN_DIR", &p.dir)
                .stdin(std::process::Stdio::null())
                .stdout(out)
                .stderr(err)
                .process_group(0)
                .spawn()
            {
                Ok(child) => {
                    tracing::info!("plugin {}: started {:?}", p.id, p.manifest.run);
                    self.plugins.children.insert(p.id.clone(), child);
                }
                Err(e) => tracing::warn!("plugin {}: could not start {:?}: {e}", p.id, p.manifest.run),
            }
        }
    }
}

impl Vesper {
    /// Run the `[[hook]]` commands of enabled plugins (with the `hooks` permission) that listen for `event`.
    pub fn plugin_event(&mut self, event: &str, env: &[(&str, String)]) {
        for p in self.plugins.active.iter().filter(|p| p.can("hooks")) {
            for h in p.manifest.hook.iter().filter(|h| h.event == event) {
                let mut cmd = std::process::Command::new("sh");
                cmd.arg("-c")
                    .arg(plugins::shell_in(&p.dir, &h.command))
                    .env("VESPER_PLUGIN_ID", &p.id)
                    .env("VESPER_PLUGIN_DIR", &p.dir)
                    .env("VESPER_EVENT", event)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                for (k, v) in env {
                    cmd.env(k, v);
                }
                match cmd.spawn() {
                    Ok(child) => crate::state::reap(child),
                    Err(e) => tracing::warn!("plugin {}: hook {event}: {e}", p.id),
                }
            }
        }
    }

    /// Run the command of shortcut `key` of plugin `plugin` (indexes into `plugins.active`).
    pub fn run_plugin_key(&mut self, plugin: usize, key: usize) {
        let Some(p) = self.plugins.active.get(plugin) else { return };
        let Some(k) = p.manifest.key.get(key) else { return };
        match std::process::Command::new("sh")
            .arg("-c")
            .arg(plugins::shell_in(&p.dir, &k.command))
            .env("VESPER_PLUGIN_ID", &p.id)
            .env("VESPER_PLUGIN_DIR", &p.dir)
            .stdin(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => crate::state::reap(child),
            Err(e) => tracing::warn!("plugin {}: shortcut {}: {e}", p.id, k.id),
        }
    }
}
