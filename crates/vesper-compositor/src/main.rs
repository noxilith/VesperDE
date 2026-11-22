mod logfile;
mod backend;
mod blur;
mod cursor;
mod dmabuf;
mod effects;
mod gtkcss;
mod handlers;
mod input;
mod ipc;
mod layer;
mod lock;
mod outputs;
mod plugins;
mod power;
mod layout;
mod render;
mod round;
mod extcapture;
mod screencopy;
mod screenshot;
mod state;
mod xwayland;

use anyhow::{bail, Context, Result};
use std::time::Duration;

use smithay::reexports::{
    calloop::{
        timer::{TimeoutAction, Timer},
        EventLoop,
    },
    wayland_server::Display,
};
use state::Vesper;

const USAGE: &str = "\
vesper: the Vesper desktop compositor

USAGE: vesper [--winit | --tty] [--exit-after SECS] [--version]

  --tty     run on a real seat (DRM/KMS + libinput). Default when no other compositor is running.
  --winit   run as a window inside another compositor (for development).
  --greeter  run as the login screen under greetd: starts vesper-greeter instead of the desktop, no shortcuts.
  --exit-after SECS   quit by itself after SECS seconds (safety net when testing on a bare tty).
";

fn main() -> Result<()> {
    let mut force = None;
    let mut exit_after: Option<u64> = None;
    let mut greeter = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--exit-after" => {
                exit_after = Some(args.next().and_then(|s| s.parse().ok()).context("--exit-after needs a number of seconds")?);
            }
            "--tty" => force = Some(false),
            "--greeter" => greeter = true,
            "--winit" => force = Some(true),
            "--version" | "-V" => {
                println!("vesper {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }

    logfile::start();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let nested = force.unwrap_or_else(|| {
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
    });

    let mut config = vesper_config::Config::load();
    if greeter {
        // the login screen takes the display scales chosen in Settings (copied to /etc by a helper, see vesper-config/greeter_display.rs)
        vesper_config::greeter_display::apply_to_greeter(&mut config);
    } else {
        vesper_config::greeter_display::publish(&config);
    }
    let mut event_loop: EventLoop<'static, Vesper> = EventLoop::try_new().context("creating event loop")?;
    let display: Display<Vesper> = Display::new().context("creating wayland display")?;
    let seat_name = if nested { "winit" } else { "seat0" };
    // children (and GTK/Qt apps) pick the cursor theme from these
    {
        let appearance = &config.appearance;
        std::env::set_var("XCURSOR_PATH", std::env::join_paths(vesper_config::cursor_dirs()).unwrap_or_default());
        if !appearance.cursor_theme.is_empty() {
            std::env::set_var("XCURSOR_THEME", &appearance.cursor_theme);
        }
        std::env::set_var("XCURSOR_SIZE", appearance.cursor_size.to_string());
        cursor::install_for_apps(&appearance.cursor_theme, appearance.cursor_size);
        gtkcss::apply(appearance.hide_client_titlebars);
    }
    let mut state = Vesper::new(&event_loop, display, config, nested, seat_name);
    state.greeter = greeter;
    state.ptt_refresh();

    if nested {
        backend::winit::init(&event_loop, &mut state).context("starting winit backend")?;
    } else {
        backend::tty::init(&event_loop, &mut state).context("starting tty backend")?;
    }

    let sock = std::env::var_os("VESPER_SOCKET").map(std::path::PathBuf::from).unwrap_or_else(|| {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| "/tmp".into());
        dir.join(format!("vesper-{}.sock", state.socket_name.to_string_lossy()))
    });
    ipc::init(&event_loop, &mut state, &sock).context("starting ipc")?;
    std::env::set_var("VESPER_SOCKET", &sock);

    // Everything we spawn from here on talks to us.
    std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);
    std::env::set_var("XDG_SESSION_TYPE", "wayland");
    std::env::set_var("XDG_CURRENT_DESKTOP", "Vesper");
    std::env::set_var("XDG_SESSION_DESKTOP", "vesper");
    tracing::info!("listening on {:?}", state.socket_name);
    state.start_xwayland();

    // Services the session bus starts on demand (the file portal, ...) need to know where we are.
    let _ = std::process::Command::new("dbus-update-activation-environment")
        .args(["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE", "XDG_SESSION_DESKTOP", "VESPER_SOCKET"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(crate::state::reap);

    if let Some(secs) = exit_after {
        tracing::info!("will exit by itself after {secs}s");
        let timer = Timer::from_duration(Duration::from_secs(secs));
        event_loop.handle().insert_source(timer, |_, _, state| {
            tracing::info!("--exit-after reached");
            state.loop_signal.stop();
            TimeoutAction::Drop
        }).map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    // Once a second: lock after idling, and keep a lock client alive while locked.
    event_loop
        .handle()
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
            state.power_tick();
            state.idle_tick();
            state.watch_files();
            state.ptt_tick();
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if greeter {
        // the login screen: just the greeter, and when it ends (logged in, or crashed) so do we
        let name = "vesper-greeter";
        let sibling = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name)));
        let bin = sibling.filter(|p| p.is_file()).map(|p| p.into_os_string()).unwrap_or_else(|| name.into());
        let mut child = std::process::Command::new(&bin).spawn().with_context(|| format!("starting {}", bin.to_string_lossy()))?;
        let signal = state.loop_signal.clone();
        std::thread::spawn(move || {
            let _ = child.wait();
            signal.stop();
            signal.wakeup();
        });
    }

    state.plugins_sync(); // first, so replaced programs start as the plugin wants
    for (name, enabled) in [
        ("vesper-wallpaper", state.config.shell.wallpaper && !greeter),
        ("vesper-bar", state.config.shell.bar && !greeter),
        ("vesper-notify", state.config.shell.notifications && !greeter),
        ("vesper-polkit", state.config.shell.polkit && !greeter),
    ] {
        if !enabled {
            continue;
        }
        // a plugin's replacement if there is one, else the binary built next to this one (dev builds, relocatable installs), else PATH
        match Vesper::component_command(name, &[]).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(err) => tracing::warn!("could not start {name}: {err}"),
        }
    }

    if !greeter {
        for cmd in state.config.autostart.clone() {
            state.spawn(&cmd);
        }
    }
    state.plugins_sync();

    // Some event source can stay ready without anything to do (a client that keeps committing, a stuck fd). Every pass
    // then runs the whole render check and the loop uses a full core. When passes come back-to-back for a while we
    // sleep a moment per pass: input still gets through within 2 ms, and the CPU use drops to almost nothing.
    let mut last_pass = std::time::Instant::now();
    let mut hot = 0u32;
    let mut warned = false;
    event_loop.run(None, &mut state, move |state| {
        tracing::trace!(target: "vesper::wake", "loop pass");
        let now = std::time::Instant::now();
        let gap = now.duration_since(last_pass);
        // once throttled, only a real pause (5 ms) ends it: our own sleep makes the gaps about 2 ms
        if gap < std::time::Duration::from_micros(300) || (hot > 200 && gap < std::time::Duration::from_millis(5)) {
            hot = hot.saturating_add(1);
        } else {
            hot = 0;
        }
        last_pass = now;
        if hot > 200 {
            if !warned {
                warned = true;
                tracing::warn!("the event loop is spinning (passes come back-to-back); throttling it. Run with RUST_LOG=vesper::wake=trace to see passes");
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        state.space.refresh();
        state.popups.cleanup();
        let _ = state.display_handle.flush_clients();
        state.redraw_all();
    })?;
    state.plugins.stop_all();
    tracing::info!("bye");
    Ok(())
}
