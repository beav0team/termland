# Shipping termland as a distro's front end

This is not a window manager for people who already have a desktop. It is meant
to **be** the desktop — the thing you are looking at after you log in, and the
thing on the screen of the live ISO. That reframing decides most of the
architecture, so it is written down here rather than discovered later.

Status: `v0.1` runs on kitty. Everything below is the gap between that and a
distro front end, in the order it has to be closed.

---

## What "front end" means, concretely

On a normal Linux desktop the stack is:

```
firmware → kernel → DRM/KMS → compositor (KWin/Mutter/sway) → toolkit → apps
                                  ↑
                       greeter (SDDM) → session → shell
```

A termland distro collapses that to:

```
firmware → kernel → DRM/KMS → termland → ptys → programs
                        ↑
          login (greetd) or nothing at all
```

That deletion is the entire product. No compositor, no toolkit, no session
manager, no display server, no desktop shell — and therefore no whole classes
of bug, no 600 MB of Qt, and no "why did my desktop stop starting".

It also means **every one of those deleted layers is now termland's job.** The
rest of this file is that list.

---

## The four consequences

### 1. The host cannot be kitty

`v0.1` uses kitty as the drawing surface because kitty already speaks the
graphics protocol, has a working keyboard protocol, and is one `apt install`.
That is a development accelerator, not a shipping target: a distro cannot say
"first install a terminal emulator, then run our terminal inside it".

So the frontend has to be ours, and the boundary between core and frontend has
to be real *before* it is rewritten:

```
        ┌──────────────────────────────────────────┐
        │  core: layout · pty · ipc · keys · app   │   ← reusable, unchanged
        └────────────────┬─────────────────────────┘
                         │  cells + images + input events
        ┌────────────────┴─────────────────────────┐
        │  surface: kitty (now) │ gpu (tier 2) │ drm │
        └──────────────────────────────────────────┘
```

What the core may *never* assume again:

- that drawing means "escape codes into somebody else's terminal"
- that an image is an encoded byte blob (the GPU frontend wants a **texture and
  a rect**; encoding it to sixel and rasterising it back is absurd)
- that input arrives as `crossterm::Event`
- that there is a 500 ms idle tick because a `recv` blocked

That last one matters more than it looks: a shipping frontend needs **frame
pacing**, not event-driven redraws, because cursor blink, animations and
partial-update scrolling all need a clock that is not "a pane printed bytes".

### 2. Everything must be in the image

No "install libchafa-dev first". The build just broke on exactly this: adding
chafa (for sixel) dragged in glib, and glib was not present — a dependency chain
that exists only to encode images for terminals that are not ours. It is now an
**optional feature, off by default**: `cargo build` needs no C library beyond
libc. A distro image should not carry a graphics encoder its own frontend never
calls.

Same rule for fonts. `v0.1` asks the user to have JetBrainsMono Nerd Font
installed. A tier-2 frontend owns its glyph atlas, so it must **bundle** the
font and its license, and rasterise what it needs at whatever DPI the panel
reports.

### 3. It must fail soft

The frontend is what the user sees when nothing else works. If it crashes, the
machine must still be usable — not "black screen with a cursor at login".

Required behaviour:

| failure | must happen |
|---|---|
| termland crashes at boot | append to `$XDG_STATE_HOME/termland/termland.log`; fall back to a plain `getty` on VT1 |
| a pane's child dies | pane stays, marked exited, `Ctrl+Space q` closes it (already works) |
| GPU init fails / wrong driver | start in a software or halfblock mode, never refuse to boot |
| config is invalid | start with defaults, print the error in the status bar |
| the machine loses power mid-session | nothing persistent to corrupt: no session state on disk |

`v0.1` already has the third and fifth properties by construction (no
persistence, halfblocks floor), and now logs panics plus config/registry
errors (the fourth: the message also lands in the status bar). The first —
falling back to a `getty` — still needs a session wrapper.

### 4. It starts before there is a user session

Three viable boot chains, in order of how soon they can ship:

| # | chain | cost | notes |
|---|---|---|---|
| A | `greetd` → `tuigreet` → exec termland | a day | all userspace, works on a normal install today, gets a real login prompt for free |
| B | tty1 `.bash_profile` → `exec termland` | an hour | no PAM, no session; fine for a live ISO, wrong for an installed system |
| C | termland as the systemd session with `seatd`/`libinput` | weeks | no greeter at all; termland *is* the login screen |

A is the right first target: it proves the session integration without owning
input devices. C is where it ends up.

Also needed either way: `Ctrl+Alt+F2` must reach a real VT with a shell on it,
which means termland has to release the seat cleanly rather than holding DRM
master forever.

---

## What stays, what changes

| module | today | for the distro |
|---|---|---|
| `layout.rs` | split tree, geometry, focus | **unchanged** |
| `pty.rs` | ptys + vt100 | **unchanged** (`TERM` stays `xterm-256color`) |
| `ipc.rs` | `tlc` socket, `0600`, `$XDG_RUNTIME_DIR` or `$HOME/.run` | unchanged |
| `keys.rs` | `KeyEvent` → ANSI | keep the encoder; input *source* changes |
| `app.rs` | state + keymap + `draw(&mut Frame)` | keep everything except `draw` |
| `ui.rs` | ratatui cell writer | becomes the **kitty surface**; the GPU surface replaces it |
| `image.rs` | `ratatui-image` protocols | split: decode (keep) vs. encode (surface-specific) |
| — | — | **new:** `surface.rs` trait, frame clock, login; logging and config error reporting landed in v0.1 |

The refactor is deliberately ordered *after* the thing runs: you cannot
generalise a boundary you have not crossed once.

---

## Tier 2: the renderer

`wgpu` + `glyphon` (cosmic-text for shaping and fallback) drawing into a single
full-screen surface, hosted either by:

- **`cage`** — a kiosk wlroots compositor, one app, no desktop. Cheapest path to
  "there is no window manager", costs a Wayland dependency and ~30 MB RAM.
- **direct DRM/KMS** — no compositor at all; `libseat` + `libinput` + a GBM
  surface. Smallest and fastest, and the most code we own. That code is
  well-trodden (foot, kmscon, cage all do it) but it is *our* bug surface.

Recommendation: `cage` first to get a real screenshot of tier 2 on hardware,
then decide whether DRM is worth the ownership.

Hardware reality check: Intel UHD 620, 8 GB RAM, no discrete GPU. Target
budget: **60 fps of a 1920×1080 cell grid at < 8 % of one core**, ~120 MB RSS
including 6 panes. Anything that needs a vulkan extension this GPU lacks is a
non-starter — hence wgpu with a GL fallback rather than a hand-written Vulkan
renderer.

---

## The problems that will actually be hard

Not the renderer. These:

1. **Input methods.** IBus/Fcitx assume a toolkit speaking a Wayland protocol.
   A terminal frontend that owns input has to implement `text-input-v3` (under
   `cage`) or speak the IM protocol directly. Without it, the distro cannot type
   Arabic, Chinese or Japanese — a **hard requirement**, not a nice-to-have.
   This alone may justify staying on a wlroots host longer than planned.
2. **Accessibility.** Screen readers hook AT-SPI, which hooks toolkits. Being
   the terminal means being the toolkit: at minimum an AT-SPI bridge exposing
   pane text, focus and cursor. Decide early whether this is in scope.
3. **Composition/clipboard.** No compositor means implementing the clipboard
   yourself (a wlroots host gives it for free). Same for drag-and-drop.
4. **HiDPI and mixed DPI.** Terminals are integer-cell grids; a 1.5× scale on
   one monitor and 1.0× on another needs per-output cell metrics, not a global
   font size.
5. **VT switching.** `Ctrl+Alt+F2` cannot be swallowed by the frontend; the seat
   has to be released on `SIGUSR1`-style VT signals when running on DRM.
6. **The live ISO's first 30 seconds.** It boots, it asks for a language, it
   opens the installer inline. That installer is a TUI, so it can be a pane.

---

## Milestones

| # | milestone | proves | status |
|---|---|---|---|
| 0 | `v0.1` on kitty | the model works: tree, ptys, images, IPC | **done** |
| 0b | desktop interaction | mouse, right-click menus, a real file manager, Super bindings | **done** |
| 1 | session under greetd | it can be a login session, not an app | |
| 2 | `surface` trait + kitty surface | the frontend boundary is real | **done** |
| 3 | GPU frontend under cage | it can be the whole screen on real hardware | |
| 4 | bundled font, config, logging | it can ship in an image | logging + config done; font pending |
| 4b | desktop feel: frame clock, hover, pointer shapes, image layer | input is a desktop's, not a form's | **done** |
| 4c | desktop surface: wallpaper, `~/Desktop` icons, icon-view file manager | it is a place, not just a tiling area | **done** |
| 5 | ISO that boots to termland | it is a distro | |
| 6 | direct DRM/KMS | no compositor at all | |
| 7 | IME + clipboard (+ AT-SPI?) | it is a desktop people can live in | |

Milestone 5 is the one that turns this from a good idea into a product; 7 is the
one that decides whether it is a *usable* product or a beautiful demo.

**On 4c, and why the desktop only exists on an empty workspace.** A tiling
session has no "behind": panes cover the screen, so a wallpaper underneath them
would never be seen. The desktop is therefore not a layer under everything, it
is *what an untiled workspace is* — and `Super+D` hides the panes to get back
to it. The icons come from the same grid geometry and the same half-block
painting as the file manager's icon view, because a folder should look the same
wherever it is and a second renderer is a second thing to keep correct.

The wallpaper is either a picture sampled into half blocks or a gradient painted
per cell. The picture path is the one place a *file* is allowed to influence the
desktop, and it stays inside the cell grid: `▀` carries two pixel rows per cell,
the pixels are dimmed toward the pane colour, and the icons' names sit on a
darkened pill. Nothing here uses the kitty graphics protocol, so a wallpaper
works in a terminal that has never heard of an image — which is the same rule the
rest of the product follows.

---

## Non-goals

- Running X11 applications. There is no X server; `Xwayland` under `cage` is
  possible and deliberately out of scope for now.
- Being a drop-in replacement for a normal desktop. Different machine.
- Supporting macOS/Windows hosts. The core is portable in principle, but the
  product is a Linux distribution's front end.
