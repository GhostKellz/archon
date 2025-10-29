SIDEBAR_DIR := extensions/archon-sidebar
SIDEBAR_ZIP := extensions/archon-sidebar.zip
THEME_EXPORT_DIR ?= dist/themes/chromium

.PHONY: sidebar-zip export-themes

sidebar-zip:
	@./tools/scripts/package_sidebar.sh

export-themes:
	@./tools/scripts/export_theme_pack.sh $(THEME_EXPORT_DIR)
