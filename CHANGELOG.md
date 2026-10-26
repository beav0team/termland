# Changelog

## [v0.1.0] — 2026-10-26

First release (alpha, host tier on kitty).

- Split tree (binary dwindle), directional focus by geometry, workspaces 1–9
- Real image panes (kitty graphics → sixel → iTerm2 → halfblocks fallback)
- Web panes (`w3m`), live monitor pane (`tui-chart` + `sysinfo`)
- Built-in file manager (list + icon grid), desktop with wallpaper + icons (`Super+D`)
- App menu (`Super+R`), mouse with hover / divider resize / OSC 22 pointer shapes
- Control socket `tlc` (zero dependencies, unix socket, line protocol)
- Zero system files; uninstall is a deletion
