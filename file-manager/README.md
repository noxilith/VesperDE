# File Manager

The file manager of the [Vesper desktop](../README.md).

> **This is a fork of [Ski](https://github.com/ilyamiro/Ski) by [ilyamiro](https://github.com/ilyamiro)**, used under the MIT licence
> (see [LICENSE](LICENSE), unchanged). All the credit for the file manager itself goes to the original author.
> Ski's [wiki](https://github.com/ilyamiro/Ski/wiki) documents the features, which this fork keeps.

## What it does

A fast, keyboard-friendly file manager written in Rust with egui, drawn with OpenGL, Wayland-native (X11 works too).

- Tabs, grid and list views, thumbnails, search, bookmarks, drag and drop, trash, and remote locations (servers)
- Plugins: Git, checksums, tags, duplicate finder, compare folders, quick look, custom actions
- Themes (many bundled, with an editor), icon styles, and translations
- Can be the system's **file chooser** (an XDG desktop portal backend) and implements `org.freedesktop.FileManager1`,
  so "show in folder" from browsers and other apps opens it

## What changed in this fork

- Renamed from Ski to **File Manager**: binary `file-manager`, window class `file-manager`, config in `~/.config/file-manager`.
  Because of that it does not read an existing Ski installation's settings, and it won't touch them.
- The D-Bus names and portal registration use `filemanager` (`org.freedesktop.impl.portal.desktop.filemanager`), and the portal is
  enabled for Vesper sessions (`UseIn=Vesper`). Vesper's `vesper-portals.conf` makes it the file chooser.
- Environment variables for custom actions are `FILE_MANAGER_SELECTED_FILES` and `FILE_MANAGER_CURRENT_DIR` (they were `SKI_...`),
  and tags are stored in the `user.filemanager.*` extended attributes.
- Upstream's Docker, Nix, CI and release packaging were dropped; Vesper's `Makefile` installs it.

Everything else is Ski as of version 0.6.3.

## Building

It is a standalone project with its own `Cargo.lock`, not part of Vesper's workspace:

```sh
cargo build --release --locked --manifest-path file-manager/Cargo.toml   # from the repository root
```

## Licence

MIT, Copyright (c) 2026 ilyamiro. The bundled fonts keep their own licences: Adwaita Sans (SIL OFL) and Symbols Nerd Font Mono (MIT).
