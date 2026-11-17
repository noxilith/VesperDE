# Generic install for any distro. Packagers: `make DESTDIR=$pkgdir PREFIX=/usr install`
PREFIX  ?= /usr
DESTDIR ?=
CARGO   ?= cargo
CARGOFLAGS ?= --release --locked

BINS := vesper vesper-bar vesper-launcher vesper-switcher vesper-notify vesper-wallpaper vesper-wallpaper-picker vesper-theme vesper-lock vesper-screenshot vesper-settings vesper-power vesper-polkit vesper-greeter vesper-session file-manager

all: build

# file-manager is a separate project (a fork of Ski) with its own lockfile.
build:
	$(CARGO) build $(CARGOFLAGS)
	$(CARGO) build --manifest-path file-manager/Cargo.toml $(CARGOFLAGS)

test:
	$(CARGO) test --locked
	$(CARGO) test --manifest-path file-manager/Cargo.toml --locked

install:
	install -Dm755 target/release/vesper           $(DESTDIR)$(PREFIX)/bin/vesper
	install -Dm755 target/release/vesper-bar       $(DESTDIR)$(PREFIX)/bin/vesper-bar
	install -Dm755 target/release/vesper-launcher  $(DESTDIR)$(PREFIX)/bin/vesper-launcher
	install -Dm755 target/release/vesper-switcher  $(DESTDIR)$(PREFIX)/bin/vesper-switcher
	install -Dm755 target/release/vesper-notify    $(DESTDIR)$(PREFIX)/bin/vesper-notify
	install -Dm755 scripts/vesper-record           $(DESTDIR)$(PREFIX)/bin/vesper-record
	install -Dm755 target/release/vesper-wallpaper $(DESTDIR)$(PREFIX)/bin/vesper-wallpaper
	install -Dm755 target/release/vesper-wallpaper-picker $(DESTDIR)$(PREFIX)/bin/vesper-wallpaper-picker
	install -Dm755 target/release/vesper-lock      $(DESTDIR)$(PREFIX)/bin/vesper-lock
	install -Dm755 target/release/vesper-theme     $(DESTDIR)$(PREFIX)/bin/vesper-theme
	install -Dm755 target/release/vesper-screenshot $(DESTDIR)$(PREFIX)/bin/vesper-screenshot
	install -Dm755 target/release/vesper-settings   $(DESTDIR)$(PREFIX)/bin/vesper-settings
	install -Dm755 target/release/vesper-power      $(DESTDIR)$(PREFIX)/bin/vesper-power
	install -Dm755 target/release/vesper-polkit     $(DESTDIR)$(PREFIX)/bin/vesper-polkit
	install -Dm755 target/release/vesper-greeter    $(DESTDIR)$(PREFIX)/bin/vesper-greeter
	install -Dm644 session/greetd-vesper.toml       $(DESTDIR)$(PREFIX)/share/vesper/greetd-vesper.toml
	install -Dm644 session/pam/vesper-lock          $(DESTDIR)/etc/pam.d/vesper-lock
	install -Dm755 session/vesper-session          $(DESTDIR)$(PREFIX)/bin/vesper-session
	install -Dm644 session/vesper.desktop          $(DESTDIR)$(PREFIX)/share/wayland-sessions/vesper.desktop
	install -Dm644 session/vesper-portals.conf     $(DESTDIR)$(PREFIX)/share/xdg-desktop-portal/vesper-portals.conf
	install -Dm644 session/xdg-desktop-portal-wlr.config  $(DESTDIR)/etc/xdg/xdg-desktop-portal-wlr/config
	install -Dm644 session/vesper-settings.desktop $(DESTDIR)$(PREFIX)/share/applications/vesper-settings.desktop
	install -Dm644 config/default.toml             $(DESTDIR)/etc/vesper/config.toml
	install -Dm644 LICENSE                         $(DESTDIR)$(PREFIX)/share/licenses/vesper/LICENSE 2>/dev/null || true
	# File Manager (fork of Ski)
	install -Dm755 target/release/file-manager                    $(DESTDIR)$(PREFIX)/bin/file-manager
	install -Dm644 file-manager/assets/file-manager.desktop       $(DESTDIR)$(PREFIX)/share/applications/file-manager.desktop
	install -Dm644 file-manager/assets/logo.svg                   $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/file-manager.svg
	install -Dm644 file-manager/assets/filemanager.portal         $(DESTDIR)$(PREFIX)/share/xdg-desktop-portal/portals/filemanager.portal
	for name in org.freedesktop.impl.portal.desktop.filemanager org.freedesktop.FileManager1; do \
	  install -Dm644 file-manager/assets/$$name.service $(DESTDIR)$(PREFIX)/share/dbus-1/services/$$name.service; \
	  sed -i "s|@out@|$(PREFIX)|" $(DESTDIR)$(PREFIX)/share/dbus-1/services/$$name.service; \
	done
	install -Dm644 file-manager/LICENSE $(DESTDIR)$(PREFIX)/share/licenses/file-manager/LICENSE
	install -Dm644 packaging/60-vesper-emoji.conf                  $(DESTDIR)/etc/fonts/conf.d/60-vesper-emoji.conf
	install -d $(DESTDIR)$(PREFIX)/share/vesper/cursors
	cp -a assets/cursors/. $(DESTDIR)$(PREFIX)/share/vesper/cursors/

uninstall:
	rm -f $(addprefix $(DESTDIR)$(PREFIX)/bin/,$(BINS))
	rm -f $(DESTDIR)$(PREFIX)/share/wayland-sessions/vesper.desktop \
	      $(DESTDIR)$(PREFIX)/share/xdg-desktop-portal/vesper-portals.conf \
	      $(DESTDIR)$(PREFIX)/share/applications/file-manager.desktop \
	      $(DESTDIR)$(PREFIX)/share/applications/vesper-settings.desktop \
	      $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/file-manager.svg \
	      $(DESTDIR)$(PREFIX)/share/xdg-desktop-portal/portals/filemanager.portal \
	      $(DESTDIR)$(PREFIX)/share/dbus-1/services/org.freedesktop.impl.portal.desktop.filemanager.service \
	      $(DESTDIR)$(PREFIX)/share/dbus-1/services/org.freedesktop.FileManager1.service
	rm -rf $(DESTDIR)$(PREFIX)/share/licenses/file-manager
	rm -f $(DESTDIR)/etc/pam.d/vesper-lock

.PHONY: all build test install uninstall
