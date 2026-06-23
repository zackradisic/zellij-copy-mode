# zellij-copy-mode

A tmux-style **copy mode** for [Zellij](https://zellij.dev), as a plugin. Press a
key, the focused pane's scrollback opens in a floating overlay, and you navigate
it with vim motions, visually select, and yank to the clipboard.

> Status: **working (monochrome).** Core flow — acquire target pane → read its
> scrollback → render with a cursor → vim navigation → visual select → yank — is
> functional. Color is the next step (see Roadmap).

## How it works

1. `Ctrl b` `[` runs `LaunchOrFocusPlugin … { floating true }`, opening the plugin
   as a **floating pane on top** of the current pane. Nothing is suppressed or
   replaced, so it can never strand "ghost" panes.
2. On a `PaneUpdate`, the plugin finds the focused, non-floating, non-plugin
   terminal — the pane under the overlay — as its **target**.
3. It reads that pane's scrollback via `get_pane_scrollback(target, full)` and
   parses it (through `vte`) into a grid of styled cells.
4. It renders the grid with a block cursor + visual selection, intercepts keys
   for vi motions, and `copy_to_clipboard` on yank. Exit just closes the
   floating pane.

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

## Development

Zellij caches the compiled plugin **in the running server's memory**, so a plain
rebuild won't show up — you must reload it. Use the helper:

```bash
./dev-reload.sh   # rebuilds, then `zellij action start-or-reload-plugin …`
```

Run it from inside your Zellij session; then trigger copy mode again to pick up
the new build. No new session required.

## Roadmap

- **Color.** `get_pane_scrollback` returns plain text. The colored path is
  `DumpScreen { ansi: true, include_scrollback: true }` → file → read back, but
  the `/host` ↔ host-path mapping for the dump file still needs to be pinned down.
- Block/rectangle select (`Ctrl-v`), mouse drag-select, count prefixes (`3j`),
  `f/F/t/T`, search-match highlighting, wide-char / line-wrap handling.
- Remove the on-screen debug panel once the above are settled.
