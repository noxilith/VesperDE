# The Vesper login screen

Vesper has its own login screen, `vesper-greeter`. It is a [greetd](https://sr.ht/~kennylevinsen/greetd/) greeter: greetd does
the work of logging you in (PAM, logind, starting your session), and the greeter is what you see and type into.

## Set it up

1. Install `greetd` and `vesper` (the `vesper-greeter` program comes with it).
2. Copy the sample configuration and enable greetd instead of your current display manager:

   ```sh
   sudo cp /usr/share/vesper/greetd-vesper.toml /etc/greetd/config.toml
   sudo systemctl disable sddm        # or gdm, lightdm: whichever you use now
   sudo systemctl enable greetd
   ```

3. Reboot (or switch to the greetd VT). You get a clock, a user name and password field, the session you will start
   (press **F3** or click it to change), and Restart / Shut down.

The greeter remembers the last user and session in `/var/cache/vesper-greeter/last` (make that directory writable by the
`greeter` user, e.g. `sudo install -d -o greeter -g greeter /var/cache/vesper-greeter`).

Sessions come from `/usr/share/wayland-sessions/*.desktop`, so everything installed there (Vesper, niri, Hyprland, ...)
appears in the list.

## How it works

* `vesper --tty --greeter` is Vesper's compositor with no bar, no shortcuts and no autostart: only `vesper-greeter` runs,
  and when it ends (you logged in) the compositor exits so greetd can start your session.
* The greeter talks to greetd over `$GREETD_SOCK` ([greetd-ipc(7)](https://man.sr.ht/~kennylevinsen/greetd/)). Extra
  questions from PAM (one-time codes) are shown as a prompt under the clock.
* Keys: **Tab** switches between user name and password, **Enter** goes on, **Esc** clears, **F3** changes session.

## Trying it without installing

Run a fake greetd and the greeter in a window: see `crates/vesper-greeter/src/login.rs` (the test) for the protocol, or start
`vesper --winit --greeter` with `GREETD_SOCK` pointing at any greetd-compatible socket.

## Look

The background is your desktop wallpaper (`[wallpaper] path`), blurred and darkened. Change it under `[greeter]`
(or Settings > Background > Login screen):

```toml
[greeter]
background = ""   # another image just for the login screen; empty = the desktop wallpaper
blur       = 40   # 0 (sharp) to 100
dim        = 35   # darken, 0 to 90 percent
```

The greeter runs as its own user, so it reads `/etc/vesper/config.toml`, not your personal config. The user field
is a drop-down of the people on the machine (uid from `UID_MIN` in `/etc/login.defs`, a real login shell; `greeter`
and `nobody` are left out). Click it, or press Up/Down while it is focused.

The lock screen (`vesper-lock`) looks the same: same blurred background, clock and layout, with the user and session
shown but fixed. It uses the same `[greeter]` settings, and follows the wallpaper picked in the changer (Mod+W)
unless `background` is set.
