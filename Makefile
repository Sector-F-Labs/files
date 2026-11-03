.DEFAULT_GOAL := help

APP_ID := com.sectorflabs.Files
PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin
APPSDIR := $(PREFIX)/share/applications
ICONDIR := $(PREFIX)/share/icons/hicolor
DBUSDIR := $(PREFIX)/share/dbus-1/services
PORTAL_SERVICE := org.freedesktop.impl.portal.desktop.patina
# xdg-desktop-portal reads its .portal files and configuration only from
# system folders, so install-portal needs root (SUDO=doas where that's it).
PORTALDIR := /usr/share/xdg-desktop-portal
SUDO ?= sudo

.PHONY: help run build test check install uninstall default install-portal uninstall-portal app clean screenshots icon

help: ## Show this help
	@echo "Usage: make <target>"
	@echo
	@awk 'BEGIN { FS = ":.*## " } /^[a-z-]+:.*## / { printf "  \033[1m%-14s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

run: ## Run Files
	cargo run

build: ## Build a release binary
	cargo build --release

test: ## Run the tests
	cargo test

screenshots: build ## Rewrite docs/screenshots on made-up data (docs/demo/)
	docs/demo/screenshots.sh

icon: ## docs/icon.png: the app icon at 120 px, for the README
	cargo run -q --example render_icon -- 120 docs/icon.png

check: test ## Run the tests and clippy
	cargo clippy --all-targets -- -D warnings

# For the current user; no sudo. The launcher's Exec gets the absolute path,
# so it works even if $(BINDIR) isn't on the session's PATH.
install: build ## Install Files and its launcher for the current user (PREFIX=~/.local)
	install -Dm755 target/release/files $(BINDIR)/files
	install -Dm644 packaging/linux/files.svg $(ICONDIR)/scalable/apps/$(APP_ID).svg
	install -d $(APPSDIR)
	sed 's|^Exec=files|Exec=$(BINDIR)/files|' packaging/linux/$(APP_ID).desktop > $(APPSDIR)/$(APP_ID).desktop
	install -d $(DBUSDIR)
	sed 's|@BINDIR@|$(BINDIR)|' packaging/portal/$(PORTAL_SERVICE).service > $(DBUSDIR)/$(PORTAL_SERVICE).service
	-update-desktop-database -q $(APPSDIR)
	-@test ! -f $(ICONDIR)/icon-theme.cache || gtk-update-icon-cache -q -t -f $(ICONDIR)
	@echo "Installed $(BINDIR)/files and the $(APP_ID) launcher"

default: ## Make Files the app that opens folders (xdg-mime)
	xdg-mime default $(APP_ID).desktop inode/directory
	@echo "Folders now open in Files; 'xdg-mime default org.gnome.Nautilus.desktop inode/directory' undoes it"

install-portal: install ## Also be the file chooser in the Patina session, for every app using the portal (asks for root)
	@test "$$(id -u)" != 0 || { echo "Run make install-portal without sudo; it asks itself."; exit 1; }
	$(SUDO) install -Dm644 packaging/portal/patina.portal $(PORTALDIR)/portals/patina.portal
	$(SUDO) install -Dm644 packaging/portal/patina-portals.conf $(PORTALDIR)/patina-portals.conf
	@echo "Installed. Apps in the next Patina session (XDG_CURRENT_DESKTOP=Patina) get Files' chooser."

uninstall-portal: ## Remove the portal files (asks for root)
	$(SUDO) rm -f $(PORTALDIR)/portals/patina.portal $(PORTALDIR)/patina-portals.conf

uninstall: ## Remove what make install put in place
	rm -f $(BINDIR)/files $(APPSDIR)/$(APP_ID).desktop $(ICONDIR)/scalable/apps/$(APP_ID).svg $(DBUSDIR)/$(PORTAL_SERVICE).service
	-update-desktop-database -q $(APPSDIR)
	-@test ! -f $(ICONDIR)/icon-theme.cache || gtk-update-icon-cache -q -t -f $(ICONDIR)

app: build ## Make target/Files.app, a folder app for ~/Apps
	rm -rf target/Files.app && mkdir -p target/Files.app/bin
	install -m755 target/release/files target/Files.app/bin/files
	install -m644 packaging/linux/files.svg target/Files.app/icon.svg
	printf 'format = 1\nname = "Files"\nexec = "bin/files"\nid = "$(APP_ID)"\nsummary = "File Manager"\nkeywords = ["folder", "file manager"]\ncategories = ["System", "Utility"]\nicon = "icon.svg"\n' > target/Files.app/app.toml
	@echo "Made target/Files.app"

clean: ## Remove build output
	cargo clean
