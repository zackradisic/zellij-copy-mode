# zellij-copy-mode

A tmux-style **copy mode** for [Zellij](https://zellij.dev), as a plugin. Press a
key and the focused pane's scrollback opens **in place** — same position and
size, in color — and you navigate it with vim motions, visually select, and yank
to the clipboard.

> Status: **working.** Flash-free in-place entry, colored scrollback, vim
> navigation, visual select, yank, and search. Non-destructive — the underlying
> pane is never modified.

## How it works

1. The keybind opens the plugin as a **floating pane** (`LaunchOrFocusPlugin …
   { floating true }`).
2. On a `PaneUpdate`, it finds the focused, non-floating, non-plugin terminal
   **in its own tab** as the **target**. Restricting to our tab disambiguates
   multi-tab / multi-client sessions where several panes report `is_focused`.
   A floating pane doesn't take *tiled* focus, so the underlying terminal still
   reports as focused — that's how we get its id, geometry, and cursor.
3. It resizes its own floating pane to **exactly cover the target**
   (`change_floating_panes_coordinates` with the target's `pane_x/y/columns/rows`,
   borderless). Doing this before the first paint keeps entry **flash-free**, and
   sizing to the pane (not full-screen) means it works with **split layouts**.
   The target is never replaced or suppressed — it stays live behind the overlay,
   so there are no ghost panes and nothing to restore on exit.
4. It reads the scrollback twice: `get_pane_scrollback` gives plain text
   instantly (monochrome), then `DumpScreen { ansi: true, include_scrollback:
   true, pane_id: target }` writes the **colored** scrollback to a file which we
   read back (the plugin's `/host` mount maps to its cwd) and re-parse — upgrading
   the grid to full color. Both go through `vte` into a grid of styled cells.
5. It renders the grid with a block cursor, visual selection, and a status bar,
   intercepts keys for vi-style motions, and `copy_to_clipboard` on yank. The
   cursor and scroll open at the **live position** (from the pane's
   `cursor_coordinates_in_pane`) so entry is seamless.
6. Exit is just `close_self` — non-destructive, nothing to undo.

## Keys

| Key | Action |
|-----|--------|
| `h/j/k/l`, arrows | Move cursor |
| `w` / `b` | Word forward / back |
| `0` / `$` | Line start / end |
| `g` / `G` | Top / bottom |
| `Ctrl-d` / `Ctrl-u` | Half page down / up |
| `Ctrl-f` / `Ctrl-b` | Page down / up |
| `v` / `V` | Char / line visual select |
| `y` / `Enter` | Yank selection → clipboard, exit |
| `/`, `n`, `N` | Search, next / prev |
| `q` / `Esc` | Exit |

## Build & install

```bash
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1
```

Bind it in `~/.config/zellij/config.kdl` (here under the `tmux` mode, so it's
`Ctrl b` then `[`):

```kdl
tmux {
    bind "[" {
        LaunchOrFocusPlugin "file:/ABS/PATH/target/wasm32-wasip1/release/zellij-copy-mode.wasm" {
            floating true
            skip_cache true
        };
        SwitchToMode "Normal"
    }
}
```

Notes:
- Floating `x`/`y`/`width`/`height` in the keybind are **ignored** by
  `LaunchOrFocusPlugin` — the plugin sets its own size at runtime.
- `skip_cache true` is for development (forces a recompile so rebuilds show up);
  drop it once stable.
- On first use you'll be prompted to grant permissions (read/change state, read
  pane contents, write clipboard, and **run actions as user** — the last is
  required for the colored `DumpScreen`).

## Development

Zellij caches the compiled plugin **in the running server's memory**, so a plain
rebuild won't show up — you must reload it. Use the helper:

```bash
./dev-reload.sh   # rebuilds, then `zellij action start-or-reload-plugin …`
```

Run it from inside your Zellij session, then trigger copy mode again to pick up
the new build. No new session required.

## Roadmap

- Re-anchor cursor/scroll precisely when the colored dump replaces the monochrome
  grid (line counts can differ slightly between the two sources).
- Block/rectangle select (`Ctrl-v`), mouse drag-select, count prefixes (`3j`),
  `f/F/t/T`, search-match highlighting, wide-char / line-wrap handling.
- Clean up leftover dead code (the unused file-poll/debug paths).
