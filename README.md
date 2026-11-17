# Vesper

A Linux desktop environment written in Rust: a Wayland compositor with niri-style scrollable tiling, with a top bar,
app launcher, notification daemon and a file manager (a fork of [Ski](https://github.com/ilyamiro/Ski)) around it. Packaged first for
[SingularitySYS](https://github.com/noxilith/SingularitySYS), buildable on any distro.

> **Status: early.** The compositor, bar, launcher, notifications and file manager run on a real tty; much of it is new and lightly tested. Expect rough edges.

## What works

- Smithay compositor, two backends: **tty** (DRM/KMS + libinput + libseat) and **winit** (a window inside another compositor, for development)
- Scrollable tiling: each workspace is a horizontal strip of columns, the screen scrolls to the focused one
- Vertical workspaces, move windows between them
- Floating windows (dialogs and fixed-size windows float automatically), fullscreen, per-column widths, wlr-layer-shell (bars, docks, wallpapers), server-side xdg-decoration
- xdg-shell, linux-dmabuf (GPU clients such as kitty work), shm, data device (clipboard), xdg-output
- **XWayland**: X11 apps (Steam, older Electron/Java apps, games) run too. The compositor starts Xwayland at login (it needs the `Xwayland` binary, package `xorg-xwayland` or similar), acts as its window manager and sets `DISPLAY` for everything it spawns. X11 windows tile, float, fullscreen and minimize like Wayland ones, show in the bar by their X11 class, get borders and rounded corners; dialogs, tool windows and fixed-size windows float automatically; menus and tooltips (override-redirect windows) draw on top without a border; the clipboard is shared both ways (text and images). `shell.xwayland = false` turns it off (Wayland only); if the binary is missing Vesper logs a warning and carries on
- Built-in pointer cursor, client cursors, hot-plugged connectors, VT switching, session pause/resume
- **vesper-bar**: top bar with workspace pills (click to switch), minimized apps in the centre, and a clock. A small layer-shell client with no GTK dependency; the compositor starts it (`shell.bar = false` to disable)
- **vesper-notify**: the notification daemon (`org.freedesktop.Notifications`). Cards stack in the top-right corner under the bar, newest first. Click a card to dismiss; normal ones expire after a few seconds, critical ones wait. Needs a D-Bus session bus (`vesper-session` starts one), and only one notification daemon can run, so stop mako first. `shell.notifications = false` to disable
- **vesper-polkit**: the polkit authentication agent. When something needs admin rights (`pkexec`, mounting a drive, package tools, GNOME-style apps), polkitd asks the agent and a password dialog styled like the bar opens in the middle of the screen. `Enter` authenticates, `Esc` cancels, a wrong password shakes the field and asks again, `Tab` switches user when polkit allows several (for example every member of `wheel`). It registers `org.freedesktop.PolicyKit1.AuthenticationAgent` on the system bus for your logind session (`$XDG_SESSION_ID`, else asked from logind) and checks the password with polkit's own setuid `polkit-agent-helper-1` (found in the usual `/usr/lib/polkit-1`, `libexec` locations, or `VESPER_POLKIT_HELPER=/path`). Needs polkit installed and its PAM service `/etc/pam.d/polkit-1`; only one agent can be registered per session, so stop other agents (polkit-gnome, lxqt, quickshell) first. `shell.polkit = false` to disable
- **vesper-launcher**: `Mod+Space` opens the launcher at the top of the screen, just under the bar, styled like the bar (its border, corner radius, opacity and accent colour). Three modes, switched with Left/Right or by clicking the chips: **Apps** (search installed apps by name or fuzzy match; pinned apps first, then the ones you used lately; pin with Ctrl+P, right-click or the star, saved in `launcher.pinned`), **Files** (search your home folder by file name, Enter opens with `xdg-open`) and **Run** (type a calculation such as `2+2*3`, `(1+2)^3` or `sqrt(16)` and the answer shows first, Enter copies it; or type any shell command and press Enter; Shift+Enter runs it in your terminal; earlier commands are remembered and listed). Up/Down or Tab to move, the mouse and wheel work, Esc closes. While it is open, windows opening or closing and focus-follows-mouse no longer steal the keyboard from it.
- **System status in the bar**: volume (click to mute, scroll to change; needs `wpctl`), network (Wi-Fi, Ethernet, offline) and battery. The icons are Nerd Font glyphs, so use a Nerd Font in `[bar] font`
- **File Manager** (`Mod+E`): a fork of [Ski](https://github.com/ilyamiro/Ski) by ilyamiro, renamed and wired into Vesper. Tabs, grid and list views, thumbnails, search, trash, remote servers, plugins and themes. It is also the system file chooser: it is registered as an XDG desktop portal backend, so other apps' Open and Save dialogs use it (needs `xdg-desktop-portal` installed), and it answers `org.freedesktop.FileManager1` for "show in folder"
- **Wallpaper** (`vesper-wallpaper`) and **wallpaper changer**: `Mod+W` pops up a grid of the images in `[wallpaper] dir` (default `~/Pictures/Wallpapers`). The selected wallpaper is a large card in the middle of a carousel, with the others as narrower strips to either side. `←` `→` or the mouse wheel slide along it, Enter or a click applies, typing filters by name, and the colour dots (red, orange, green, blue, gray) filter by dominant colour (`↑` `↓` cycle them). `Tab` cycles how it fits (fill, fit, stretch, center), Esc closes. The colours come from your wallpaper (see Colour theming). The wallpaper changes right away and is remembered next time (in `~/.local/state/vesper/wallpaper.toml`, which wins over `[wallpaper] path`). Thumbnails are cached, so it opens instantly after the first time
- **Lock screen** (`vesper-lock`): `Mod+L` or `Ctrl+Alt+L` locks, and the screen locks by itself after `[idle] lock_after` seconds (default 300) of no input. Everything is hidden and all keys are blocked while locked, except switching VT and the volume/brightness keys. Your password is checked with PAM. If the lock screen ever dies, the compositor keeps the session locked and starts it again
- **Screenshots**: `Print` captures the screen, `Alt+Print` the focused window, `Shift+Print` (or `Mod+Shift+S`) freezes the screen and lets you choose what to capture: hover a window to select it and click, or drag to capture your own region (dragging overrides the window under the pointer). The frozen picture is what gets cropped, so moving content (video, a clock, a notification) is captured exactly as it was when you pressed the key. Esc or right-click cancels. Each is saved to `[screenshots] dir` (default `~/Pictures/Screenshots`), copied to the clipboard as an image, and announced with a notification
- **Start-menu tools**: the row of buttons at the top of the start menu (right to left): Settings, the wallpaper changer, a screenshot (the frozen region picker above), **copy text from the screen** and **read a QR code**. The last two use the same frozen picker: drag over some text or a QR code (or click a window) and the result is copied to the clipboard and shown in a notification. QR codes are decoded by Vesper itself; text recognition uses `tesseract` (install it plus a language pack; set `[screenshots] ocr_lang`, for example `"eng+deu"`). The screen is frozen first, so whatever you want to read stays put.
- **Touchpad and mouse settings** (`[input.touchpad]`, `[input.mouse]`): tap to click, natural scrolling, acceleration, and more. **Volume, brightness and media keys** run the commands in `[media]`
- **Colour theming with matugen**: when the wallpaper changes, Vesper runs [matugen](https://github.com/InioX/matugen) and the bar, launcher, notifications, lock screen and wallpaper changer take their colours from it, live. Open the changer (`Mod+W`) and press `F2` (or click **Theme**) to switch it on or off, pick light, dark or smart mode, the colour scheme, contrast and which colour to pull from the image, and to **add, edit or remove your own matugen templates** (each with an input file, an output path and an optional post-hook command), or import them from `~/.config/matugen`. If matugen is not on the PATH of a console or login-manager session, Vesper also looks in `~/.local/bin`, `~/.cargo/bin`, `~/.local/share/omarchy/bin`, the usual system folders and your login shell's PATH; or set `binary = "/full/path/to/matugen"` in `~/.config/vesper/theme.toml`. Vesper uses its own matugen config (`~/.config/vesper/matugen.toml`), so your existing one is never touched. `vesper-theme regenerate` does the same from a terminal
- **Settings** (`Mod+,`, or the Settings entry in the launcher): one window for everything, like a system settings app. Pages for **Display** (resolution, refresh rate, scale, position, on/off and main monitor per screen), **Power & sleep**, **Appearance** (window borders, gaps, animations), **Background & colours** (wallpaper, matugen options, kitty and File Manager theming), **Bar**, **Keyboard & mouse** (layout, touchpad, mouse, media keys), **Notifications**, **Apps** (default terminal and file manager, programs that start at login), **Lock & session**, a list of **Keyboard shortcuts** and **About**. Every change is saved to `~/.config/vesper/config.toml` and applied to the running session within a second, so there is no Apply button. The same options can be edited by hand; unmentioned keys are left alone (comments in the file are not kept when Settings saves)
- **Dynamic workspaces**: `[layout] dynamic_workspaces = true` (Settings > Appearance) keeps workspaces that are in use in the first places. A second after you leave an unused workspace it moves to the end, so with windows on workspaces 1 and 3 you go to 1 and 3 becomes 2, and three busy workspaces are always 1 to 3. The workspace you are on stays where it is, even while it is empty, until you leave it.
- **Workspaces in the bar**: `[bar] workspace_style` = `dots`, `halo` (a filled pill or dot inside a soft ring of the same colour, like a pill-and-dots indicator) or `numbers`; `workspace_mode = "occupied"` shows only workspaces that have windows (plus the current one); `workspace_color` overrides the accent. All in Settings > Bar.
- **System icon**: the SingularitySYS logo ships with Vesper (`assets/logos`, `bundled:singularitysys` picks the dark or light version to suit the bar and is used automatically on SingularitySYS). The bar shows your distribution's logo at the far left, before the workspaces (read from `LOGO=` in os-release, so any `.svg` in your icon folders works). Pick another logo file found on the system, one of the built-in font logos (`glyph:arch`, `glyph:debian`, ...) or any `.svg`/`.png` path in Settings > Bar > System icon; `system_icon_size` and `system_icon_color` (recolour) are there too. Clicking it opens the start menu (it has the Settings gear).
- **Bar items and the start menu**: The `mic` item shows a microphone while the microphone is live (click it to mute; with push to talk it lights up only while you hold the key), and the `media` item shows a music note when no player is running. Settings > Bar > Items lists every item in three sections (left, middle, right). Each has a switch and left/right arrows, and an item at the end of a section moves into the next one, so the system icon, workspaces, minimized apps (from the current workspace), media, tray, keyboard layout, brightness, volume, network, battery, clock and bell can go anywhere (`bar.left_items`, `bar.middle_items`, `bar.right_items`). All are accent-coloured except the clock text and tray icons. The clock supports 12/24-hour, seconds and date; `show_title` adds the focused window title. Right-click the bell for do-not-disturb (it then shows the number of waiting notifications); scroll the brightness icon (uses `brightnessctl`). The **system icon opens the start menu**: a row of tools (settings, wallpapers, screenshot, copy text, read QR), Apps (the launcher), Files, Clipboard (history) and Emoji (the launcher on those tabs), the quick settings (below), live CPU / memory / temperature, and Lock, Sleep, Log out, Restart and Shut down (Restart and Shut down ask for a second click). The old power-button and system-stats items are gone; their jobs live in the start menu.
- **Everyday extras**: closing the lid locks the screen or suspends (`power.lid_close`, ignored while docked); the battery warns at 20%, 10% and 5% and suspends at 3% (`power.critical_battery_suspend`); three-finger touchpad swipes change workspace (up/down) and move focus (left/right) (`input.touchpad.gestures`); `[[rules]]` tables float, size or place windows by app id or title when they open (see `config/default.toml`); a new secured Wi-Fi network asks for its password in the Wi-Fi panel; and Esc now closes the bar's popups.
- **Emoji in other apps**: Discord, browsers and GTK/Qt apps draw emoji with the Noto Color Emoji font through fontconfig, so it has to be installed (Arch: `noto-fonts-emoji`, a dependency of the package) or those apps show empty squares. `packaging/60-vesper-emoji.conf` (installed to `/etc/fonts/conf.d`) puts it behind your normal fonts as the fallback; apps need a restart after the font is installed.
- **One look everywhere**: popups (the start menu and its side panels, the Wi-Fi/battery/calendar popups, the volume popup), the launcher, the window switcher, notifications and the notification centre and the power menu all take the top bar's corner radius, see-through level, border colour/width/blend and accent colour, so changing the bar's look in Settings changes them too.
- **Volume boost and the on-screen popup**: switch on *Boost above 100%* in the Volume panel of the start menu (or `media.volume_boost` in Settings) and the volume keys, slider and bar scrolling go up to 150% (the part above 100% is drawn in red). Changing the volume or brightness with the keys or by scrolling in the bar shows a small popup under the bar (`bar.osd` turns it off).
- **Cursors**: Settings > Appearance > Cursor picks the pointer theme and size with a live preview. Vesper bundles Bibata Modern (Classic, Ice, Amber) and Phinger (dark, light) in `assets/cursors` (installed to `/usr/share/vesper/cursors`), and any cursor theme in `~/.local/share/icons` or `/usr/share/icons` shows up too. The pointer changes at once; apps that are already running keep their old cursor until restarted. Apps that support the cursor-shape protocol (most current GTK, Qt and Chromium/Electron apps) get their pointer drawn from your theme by Vesper; for the others Vesper links the theme into `~/.local/share/icons`, writes `~/.icons/default/index.theme` and the GTK `settings.ini` cursor keys (only if those files are not yours) and sets `XCURSOR_THEME`/`XCURSOR_SIZE`. Licences are in `assets/cursors/README.txt`.
- **Config profiles**: Settings > Profiles keeps several named copies of your whole configuration: save the current settings under a name, switch between profiles with Apply (what you had is kept as "Previous"), delete them, import a config file someone sent you, and **Share** a profile (it is copied to the clipboard and saved as `vesper-<name>.toml` in Downloads, without your monitor layout, wallpaper folders, autostart programs, pinned apps or a custom logo path). Profiles live in `~/.config/vesper/profiles`; a profile is just a config file, so you can also drop one in by hand. Imports are checked first and refused if they contain settings Vesper does not know.
- **Plugins**: Settings > Plugins lists plugins (folders with a `plugin.toml` in `~/.config/vesper/plugins`), turns them on and off, installs one from a folder or `.tar.gz` and removes them. A plugin can run a background program, add widgets to the top bar (the text is the first line a command prints) and add actions to the launcher; its widgets then show up in Settings > Bar > Items. Plugins run with your permissions, so only turn on ones you trust. The guide is `docs/plugins.md` and a ready-to-copy starter is `templates/vesper-plugin-template` (also published as its own repository: [VesperDE-Plugin-Template](https://github.com/noxilith/VesperDE-Plugin-Template)).
- **Push to talk**: turn it on in the Mic panel of the start menu or in Settings > Keyboard & mouse > Media keys (`media.ptt`). The microphone is then muted all the time, including when the session starts, and is only live while the push-to-talk key is held (`media.ptt_key`: an xkb key name such as `XF86AudioMicMute`, or `code:N` for a physical key; on laptops whose mic button does not send the standard code, press **Set the key…** in the Mic panel and then the key you want, Esc cancels). The key is swallowed so apps do not see it; letting go mutes the microphone again.
- **Quick settings and brightness**: the start menu (system icon, or Mod+Escape) contains the quick settings: a brightness slider, a volume slider and switches for **Wi-Fi** (`nmcli`), **Bluetooth** (`bluetoothctl`), **airplane mode** (`rfkill`), **night light**, **sound** and **microphone** mute (`wpctl`), **silent mode** (do not disturb) and **keep awake** (no screen-off, lock or sleep while it is on; it resets at logout). Clicking the brightness icon opens a brightness slider with a night-light button; brightness is set through systemd-logind (no root needed), falling back to `brightnessctl`. Night light sets the monitors' colour ramps (and draws a faint warm tint instead if the driver refuses); strength is in Settings > Appearance. While the battery charges it shows just the charging bolt. In the start menu the Wi-Fi, Bluetooth, Sound, Mic and Night switches have a small arrow (or right-click them) that opens a panel beside the menu: nearby Wi-Fi networks, Bluetooth (an on/off switch, searching for devices that starts when the panel opens, your devices and the ones nearby: click one to connect, disconnect or pair it, right-click to forget it; it talks to BlueZ directly and accepts "just works" pairing), sound outputs, microphones, and the night-light strength; the Brightness and Volume sliders have an arrow as well (Volume: mute and output devices, Brightness: night light).
- **Bar popups**: click the clock (calendar with month arrows), volume (slider you can drag, mute, output device list; right-click the icon to mute, scroll to change), battery (state, time left, power mode via `powerprofilesctl`) or wifi (nearby networks from `nmcli`; click one to connect, saved or open networks only). Click anywhere outside the box or on another bar icon to close it.
- **Media and tray**: add `media` (previous / play-pause / next and the title of the playing MPRIS player, any player that speaks MPRIS) and `tray` (StatusNotifier icons in their own colours; left click activates, right click asks the app for its menu) to `bar.right_items` or switch them on in Settings > Bar > Items. The bar becomes the tray watcher if none is running. `item_icon_size` (default 20) sets one size for all bar icons; the system logo is drawn 15% larger. In Settings > Bar > Items every item has a switch and left/right arrows, listed in the order they appear on the bar.
- **Keyboard layout**: with several layouts set in Settings > Input (for example `us,de`), add `keyboard` to `bar.right_items` to show the active one; clicking it switches to the next (`Command::CycleLayout`). The indicator updates when you switch from the bar; layout switches made with a keyboard shortcut show up on the next bar update.
- **Splitting the bar**: hover the gap between two icons in the bar (a faint line shows where) and click to split there; click the line again to join. Fill, detached and notch get a thin divider line, islands splits the island in two. Saved in `[bar] splits`. Look (`split_style` line, dots or dashes), thickness, height, colour and spacing are in Settings > Bar > Icons & splits, which can also remove them all. Settings pages with many options are split into tabs (the Bar page has Style, Border, Workspaces, Icons & splits, Opacity & blur and Session).
- **Bar styles**: `[bar] style` is `fill` (full width, border along the top and bottom), `detached` (a floating box with a border all round, with `margin_top` and `margin_side` spacing), `notch` (a centred tab that curves into the top of the screen, `notch_width`; it widens if its content needs more room) or `islands` (separate left, middle and right boxes). `border_width`, `border_color` and `radius` apply to all of them; everything is also in Settings > Bar, and blur follows the real shape
- **Window borders, rounded corners and animations**: every window has rounded corners (`[appearance] corner_radius`) and a border in the wallpaper accent colour (muted for unfocused windows), and columns glide into place when you scroll, switch workspaces or open and close windows. Configure under `[appearance]` and `[animations]`, or in Settings
- **Blur**: a frosted-glass background (also for the top bar: `[bar] opacity`, `blur`, `blur_strength`, or Settings > Bar) behind windows that are see-through. It is off for every app until you list it: press `Mod+B` on a window to add or remove its application (or edit `[blur] apps` or use Settings > Appearance). For kitty also set `background_opacity 0.8` in `kitty.conf`. `[blur] strength` (1-100), `brightness`, `saturation` (percent) and `enabled` control it globally; Settings has sliders for them
- **Tiling by dragging**: `Mod`+left-drag moves a tiled window. Drop it on the top or bottom half of another window to split that window (it goes above or below, sharing the column), on a window's left or right edge to put it beside it, or in free space to place the columns on that side of the screen. `Mod`+right-drag resizes like Hyprland: the edge or corner nearest where you grabbed follows the pointer (width, and height inside a stack). `input.warp_mouse_on_focus` makes `Mod`+arrows move the pointer to the focused window; `input.focus_follows_mouse` focuses what you hover
- **Displays**: monitors are detected, given a sensible scale (2× or 1.5× for sharp panels) and placed left to right; override per monitor in `[display.outputs."eDP-1"]` (`mode`, `scale`, `x`, `y`, `enabled`) or in Settings. Fractional scale is supported (`wp_fractional_scale`). The tiled workspaces live on the main monitor (`[display] primary`); the bar, wallpaper and notifications appear on every monitor
- **Power**: the screen switches off (DPMS) after `[power] screen_off_after` and the computer suspends after `suspend_after`; the screen locks before sleep. `Mod+Shift+P`, the power button and the bar's power icon open a power menu (lock, sleep, log out, restart, shut down)
- **Notification centre**: `Mod+N` or the bell in the bar opens the history of past notifications, with a do-not-disturb switch and clear-all. Notifications that come with buttons (actions) show them and report your choice back to the app. `vesper-notify center|clear|dnd on|off` do the same from a terminal
- **Minimize**: `Mod+M` minimizes the focused window into the bar centre (an icon per minimized app); click an icon or press `Mod+Shift+M` to bring it back. Apps can also request it themselves
- IPC: the compositor pushes layout state as JSON lines over a Unix socket (`$VESPER_SOCKET`) and accepts commands; see `crates/vesper-ipc`
- TOML config: `~/.config/vesper/config.toml` (see [config/default.toml](config/default.toml))

Not yet: window borders and shadows, window animations, screen-off/DPMS and suspend, locking before sleep, HiDPI scaling,
multi-GPU, per-monitor workspaces, animations, touch and tablets.

## Keys

`Mod` is Super on a real session, Alt when nested (so the host compositor keeps Super). Override with `input.modifier`.

| Keys | Action |
|---|---|
| `Mod+Return` | terminal |
| `Mod+Space` | app launcher |
| `Mod+.` | emoji picker: the launcher in Emoji mode, type to search by name, arrows move, Enter copies the emoji |
| `Mod+Shift+V` | clipboard history: the launcher in Clipboard mode, Enter copies an entry again |
| `Mod+Tab` | window switcher: Tab / Shift+Tab cycle, letting go of Mod or Enter picks, Esc cancels |
| `Mod+E` | file manager |
| `Mod+M` / `Mod+Shift+M` | minimize / restore the last minimized window |
| `Mod+Q` | close window |
| `Mod+W` | wallpaper changer |
| `Mod+,` | Settings |
| `Mod+B` | blur this window's app on or off |
| `Mod+N` | notification centre |
| `Mod+Shift+P` | power menu |
| `Mod+L` or `Ctrl+Alt+L` | lock the screen |
| `Mod+Escape` | open or close the start menu |
| `Print` / `Alt+Print` / `Shift+Print` | screenshot: screen / window / region (also `Mod+Shift+S` for region) |
| volume, brightness and media keys | run the commands in `[media]` |
| `Mod+R` | cycle column width (1/3, 1/2, 2/3, full) |
| `Mod+-` / `Mod+=` | narrower / wider column |
| `Mod+F` | toggle fullscreen |
| `Mod+V` | float / tile the focused window |
| `Mod+drag` | move a floating window (left button), resize it (right button) |
| `Mod+←/→` or `H` | focus column left / right (`Mod+L` locks) |
| `Mod+Shift+←/→` or `H/L` | move column |
| `Mod+↑/↓` or `K/J` | workspace up / down |
| `Mod+1` … `Mod+9`, `Mod+0` | go to workspace 1–10 |
| `Mod+Shift+1` … `Mod+Shift+0` | send the window to workspace 1–10 (US layout symbols) |
| `Mod+[` / `Mod+]` | put this window into the column on its left (stacked below) / take it out into its own column |
| `Mod+Alt+↑/↓` | focus the window above / below in a stacked column |
| `Mod+Shift+↑/↓` or `K/J` | move window to workspace up / down |
| `Mod+Shift+E` | quit |
| `Ctrl+Alt+F1…F12` | switch virtual terminal (always on) |

## Try it on tty2

You need: Rust, and the dev libraries for `libxkbcommon`, `libinput`, `libseat`, `libdrm`, `gbm`, `mesa`/EGL, `libudev`.

1. `Ctrl+Alt+F2` and log in.
2. Run:
   ```sh
   cd ~/Projects/VesperDE && ./scripts/vesper-dev
   ```
3. `Super+Return` for a terminal. Quit with `Super+Shift+E`. `Ctrl+Alt+F1` goes back to tty1 and leaves Vesper running.

`vesper-dev` also registers your checkout's File Manager as an app (`~/.local/share/applications/file-manager.desktop`, via `scripts/install-dev-entry`) so it shows in the launcher with its icon; `make install` installs the real entry.

`vesper-dev` builds, runs `vesper --tty` under `dbus-run-session`, logs to `~/.local/state/vesper/vesper.log`, and
**exits by itself after 10 minutes** (`VESPER_TIMEOUT=secs`, `0` = never) so a hang can't strand you on tty2.
If a hung compositor holds the VT, switch back to tty1 and `pkill vesper` from there.

To try it without leaving your desktop: `cargo run -- --winit` (uses Alt as the modifier).

## Logging in with SDDM (or another display manager)

Vesper installs a session entry (`/usr/share/wayland-sessions/vesper.desktop`), so a display manager lists it next to your
other desktops. Pick **Vesper**, **Hyprland**, **niri** or anything else from the session menu on the login screen, per login.
The display manager handles the accounts; every user just needs to be able to run `vesper-session`.

```sh
sudo make install                  # installs the session entry and everything else
# then enable SDDM the way your init system does it, for example:
sudo rc-update add sddm default    # OpenRC
sudo systemctl enable sddm         # systemd
```

`vesper-session` picks up your login environment, starts a D-Bus session if needed, and logs to
`~/.local/state/vesper/session.log` when there is no terminal.

**Switching users.** When a display manager is running, the lock screen shows a **Switch user** button. It asks the display manager for a
new login screen, so someone else can log in while your session stays running and locked. For SDDM that is a call to
`org.freedesktop.DisplayManager` (`SwitchToGreeter`), and LightDM uses `dm-tool`. Set `[lock] switch_user = "never"` to hide it, or give
your own command. If the new login screen fails to appear, check that SDDM's `MinimumVT` is above the VTs you use for consoles
(the Haliade default of 7 is fine).

## Screen sharing

Vesper implements `wlr-screencopy` (version 3, shared-memory buffers) for each monitor, so `grim`, `wf-recorder` and
`xdg-desktop-portal-wlr` can capture it. The portal turns that into a PipeWire stream, which is what browsers (Firefox, Chromium,
LibreWolf), Discord, OBS and Zoom use for "share your screen".

1. Install the packages: `xdg-desktop-portal`, `xdg-desktop-portal-wlr`, `pipewire`, `wireplumber` and `slurp` (the monitor picker;
   on Arch `pacman -S xdg-desktop-portal xdg-desktop-portal-wlr pipewire wireplumber slurp`). Optional: `grim` to test.
2. `make install` puts `vesper-portals.conf` in `/usr/share/xdg-desktop-portal/`. It tells `xdg-desktop-portal` to use the `wlr`
   backend for ScreenCast and Screenshot (the session sets `XDG_CURRENT_DESKTOP=Vesper`, which selects that file). Without
   `make install`, copy it to `~/.config/xdg-desktop-portal/vesper-portals.conf`.
3. Log in again (or restart the portals: `systemctl --user restart xdg-desktop-portal xdg-desktop-portal-wlr`). The compositor
   exports `WAYLAND_DISPLAY` and `XDG_CURRENT_DESKTOP` to the D-Bus activation environment, so the portals find it.
4. Check that PipeWire runs (`wpctl status`) and that capturing works: `grim /tmp/test.png`.
5. In the browser or app, pick "share screen"; a monitor chooser appears (`slurp`). To skip it with a single monitor, create
   `~/.config/xdg-desktop-portal-wlr/config`:

   ```ini
   [screencast]
   chooser_type=none
   output_name=eDP-1
   ```

   (`output_name` is the monitor name from `[display.outputs."..."]`, for example `eDP-1`.)

Limits: monitors only (xdg-desktop-portal-wlr cannot share a single window; share the monitor and use "window" capture in OBS
if needed), shm buffers only (no dmabuf zero-copy, so a 4K monitor costs some CPU), no ext-image-copy-capture. Verified with `grim`
in a nested session; sharing in a real browser, Discord, OBS and Zoom still needs a test on a real session.

## Also built in

* **Share one window**: the screen picker (Discord, browsers, OBS) offers single windows as well as monitors, through `ext-image-copy-capture` + `ext-foreign-toplevel-list`. Needs `xdg-desktop-portal-wlr` 0.8 or newer; turn it off in Settings > Session (`share single windows`) if monitor sharing misbehaves.
* **Screen recording**: the record button in the start menu runs `vesper-record` (needs `wf-recorder`; files go to ~/Videos). `vesper-record region` records a picked region.
* **Removable drives**: a bar item (add `drives` to your bar items) appears while a USB stick or card is plugged in; click to mount and open, or eject. Needs `udisks2`.
* **Do-not-disturb schedule**: Settings > Notifications turns do-not-disturb on between two hours of the day.
* **Login screen**: `vesper-greeter` for greetd, see [docs/greeter.md](docs/greeter.md).

## Build and install

```sh
make              # cargo build --release --locked
make test
sudo make install # PREFIX=/usr by default; DESTDIR and PREFIX are honoured for packagers
```

Installs `vesper` (the compositor), `vesper-bar`, `vesper-launcher`, `vesper-notify`, `file-manager`, `vesper-session` (starts a D-Bus
session if needed), a `wayland-sessions` entry for display managers, the portal registration for the file chooser, and
`/etc/vesper/config.toml`. The file manager is built from its own project in `file-manager/` (`make build` does both).

| Target | File |
|---|---|
| Any distro | `Makefile` |
| Arch | [packaging/arch/PKGBUILD](packaging/arch/PKGBUILD) |
| SingularitySYS | [packaging/singularitysys/recipe.toml](packaging/singularitysys/recipe.toml), **draft**, see below |

### SingularitySYS

The recipe can't build yet: the distro has no recipes for `rust`, `mesa`, `wayland`, `libinput`, `libseat`,
`libxkbcommon` or `libdrm`. Those have to land first (the recipe lists them). Vesper also needs a seat provider
at runtime: systemd-logind or elogind, or seatd.

## Layout

```
crates/vesper-config      TOML config
crates/vesper-compositor  the `vesper` binary: state, layout, input, ipc, backends (tty, winit)
crates/vesper-ipc         compositor <-> shell protocol
crates/vesper-bar         the top bar
crates/vesper-apps        desktop entries and icon loading
crates/vesper-launcher    the app launcher
crates/vesper-notify      the notification daemon
crates/vesper-polkit      the polkit authentication agent (password dialog)
crates/vesper-wallpaper   the wallpaper
crates/vesper-wallpaper-picker  the wallpaper changer popup
crates/vesper-theme       matugen theming from the command line
crates/vesper-lock        the lock screen (PAM)
crates/vesper-screenshot  the region picker for screenshots
file-manager/             the file manager: a fork of Ski (own project, own lockfile)
crates/vesper-gfx         shared software drawing (shapes, text, icons)
session/                  vesper-session, vesper.desktop
scripts/vesper-dev        run on a bare tty
packaging/                Arch, SingularitySYS
```

## Roadmap

1. ~~Compositor foundation~~ (this milestone)
2. ~~Window management~~: floating, fullscreen, minimize, resize columns done. Per-monitor workspaces and animations still to do
3. Shell: ~~top bar~~, ~~minimize and restore~~, ~~launcher~~, ~~notifications~~ (actions and history still to do), system controls (volume, network, battery)
4. ~~File manager and the XDG desktop portal backend~~ (done by forking Ski; more portals later: screenshot, screencast, settings)
5. Desktop services and settings; IPC over a Unix socket
6. Theming and distribution

## Credits

- **File Manager** (`file-manager/`) is a fork of **[Ski](https://github.com/ilyamiro/Ski)** by [ilyamiro](https://github.com/ilyamiro),
  renamed and integrated with Vesper. Ski is MIT-licensed; its licence and copyright notice are kept in
  [file-manager/LICENSE](file-manager/LICENSE). Thank you to the original author. See [file-manager/README.md](file-manager/README.md)
  for what changed.
- The compositor is built on [Smithay](https://github.com/Smithay/smithay), and its scrollable tiling is inspired by
  [niri](https://github.com/YaLTeR/niri).

## License

Vesper is licensed under the **GPL-3.0-or-later** (see [LICENSE](LICENSE)). The exception is `file-manager/`, which is
**MIT**, Copyright (c) 2026 ilyamiro, under its own [LICENSE](file-manager/LICENSE).

## Credits

The emoji picker bundles the Twemoji colour pictures (CC-BY 4.0) and emoji names from the Unicode Consortium's `emoji-test.txt`; see `assets/emoji/README.txt`.
