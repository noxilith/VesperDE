# Writing Vesper plugins

A plugin is a folder with a `plugin.toml`. Vesper looks for plugin folders in

1. `~/.config/vesper/plugins` (what *Settings > Plugins > Install* uses),
2. `/usr/share/vesper/plugins` (installed with a package).

A plugin does nothing until the user turns it on in *Settings > Plugins* (that adds its folder name to
`plugins.enabled` in the config). **Plugins run with the user's own permissions**, so keep them readable and say
what they do.

The quickest start is the template: `templates/vesper-plugin-template` (published as its own repository).

## plugin.toml

```toml
name        = "Hello plugin"      # required
version     = "0.1.0"
author      = "Your name"
description = "One line for the Settings list"
license     = "MIT"
homepage    = "https://github.com/you/hello-plugin"
api         = 1                   # the plugin API this was written for (Vesper refuses newer ones)
run         = "./daemon.sh"       # optional background program

[[bar_item]]                      # zero or more
id       = "greeting"            # letters, digits, - and _
label    = "Hello widget"         # name in Settings > Bar > Items
command  = "./status.sh"
interval = 10                     # seconds, 1 to 3600 (default 10 if left out)
on_click = "./clicked.sh"         # optional

[[action]]                        # zero or more
name    = "Say hello"             # shows in the launcher's Apps list
command = "./clicked.sh"
```

Unknown keys are an error, so typos are caught when you install the plugin.

### Where commands run

Every command (`run`, `command`, `on_click`, an action) runs through `sh -c` **inside the plugin's folder**, so
`./script.sh` works. The environment has

| variable | meaning |
|---|---|
| `VESPER_PLUGIN_ID` | the plugin's folder name |
| `VESPER_PLUGIN_DIR` | the plugin's folder |
| `VESPER_SOCKET` | the desktop's IPC socket (see below) |
| `XDG_*`, `WAYLAND_DISPLAY` | as usual in the session |

### Bar widgets

`command` is run every `interval` seconds; the **first line** it prints is shown in the accent colour. An empty line
(or a non-zero exit) hides the widget. `glyph|text` draws a Nerd Font glyph before the text, for example
`|14:05`. Text is cut at 60 characters. Widgets only run while they are placed on the bar: the user puts them
there in *Settings > Bar > Items* (they are listed as "From the plugin ..."). `on_click` runs when the widget is clicked.

### Launcher actions

Each `[[action]]` is added to the launcher's Apps list (and found by typing its name). Picking it runs its command.

### The background program

`run` is started when the session starts, or when the plugin is turned on, and stopped (with everything it
started) when it is turned off or the session ends. It is not restarted if it exits on its own. Its output goes to
`~/.local/state/vesper/plugin-<folder>.log`.

## Talking to the desktop

The desktop has a Unix socket at `$VESPER_SOCKET`. It speaks newline-delimited JSON.

**State** (the desktop sends one line when you connect and one whenever something changes):

```json
{"workspaces":[{"windows":[{"app_id":"kitty","title":"~"}]}],"active":0,"focused":{"app_id":"kitty","title":"~"},
 "minimized":[],"outputs":[{"name":"eDP-1","mode":"1920x1080@60","scale":1.0,"x":0,"y":0,"w":1920,"h":1080}],
 "kb_layout":"us","kb_layouts":1, ...}
```

**Commands** (send one JSON object per line; `cmd` names the command):

```json
{"cmd":"workspace","index":2}
{"cmd":"focus_window","workspace":0,"index":1}
{"cmd":"launcher"}
{"cmd":"launcher_mode","mode":"emoji"}
{"cmd":"lock"}
{"cmd":"suspend"}
{"cmd":"settings","page":"bar"}
{"cmd":"show_osd","kind":"volume"}
{"cmd":"clipboard","text":"copied text","summary":"Notification title","body":"Notification text"}
{"cmd":"screenshot","kind":"screen"}
```

The full list is the `Command` enum in `crates/vesper-ipc/src/lib.rs`. Example in Python:

```python
import json, os, socket
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ["VESPER_SOCKET"])
f = s.makefile("rw")
state = json.loads(f.readline())            # the current state
print("active workspace", state["active"])
f.write(json.dumps({"cmd": "workspace", "index": 1}) + "\n"); f.flush()
for line in f:                              # keeps arriving as things change
    state = json.loads(line)
```

For notifications use the normal `notify-send` (Vesper shows them). Plugins can also read and write the user's config
(`~/.config/vesper/config.toml`) but should not change settings the user did not ask for.

## Testing your plugin

1. Copy the folder to `~/.config/vesper/plugins/<name>` and turn it on in *Settings > Plugins*.
2. Place its widget in *Settings > Bar > Items*.
3. Watch `~/.local/state/vesper/plugin-<folder>.log` and `~/.local/state/vesper/vesper.log`.
4. Run your scripts by hand with the variables above set to check they print what you expect.

## Sharing

Make a `.tar.gz` (the template's `package.sh` does it) and publish it, for example as a GitHub release. People install
it with *Settings > Plugins > Install from a folder or .tar.gz*.

## Versions

`api = 1` is the first plugin API (this document). Anything that changes how existing plugins behave bumps it.
