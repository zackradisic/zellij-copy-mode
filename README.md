# zellij-copy-mode

A tmux-style **copy mode** for [Zellij](https://zellij.dev), as a plugin. Press a
key, the focused pane's scrollback opens in a floating overlay, and you navigate
it with vim motions, visually select, and yank to the clipboard.

> Status: **working (monochrome).** Core flow — acquire target pane → read its
> scrollback → render with a cursor → vim navigation → visual select → yank — is
> functional. Color is the next step (see Roadmap).

## How it works

1. `Ctrl b` `[` runs `LaunchOrFocusPlugin … { floating true; width "100%"; height
   "100%"; … }`. It launches **floating at full size** (not the default small
   centered size) so the subsequent swap into the slot has no visible "small →
   full" jump.
2. On a `PaneUpdate`, the plugin finds the focused, non-floating, non-plugin
   terminal **in its own tab** as the **target** (the tab restriction
   disambiguates multi-tab / multi-client sessions where several panes report
   `is_focused`).
3. It reads that pane's scrollback via `get_pane_scrollback(target, full)` *while
   the pane is still live* and parses it (through `vte`) into a grid of styled
   cells. (Reading must happen before the swap — `get_pane_scrollback` returns
   "not found" for a suppressed pane.)
4. It swaps into the target's slot in place via `replace_pane_with_existing_pane(
   target, me, suppress=true)` — the original is suppressed behind us, so it
   looks like the pane *became* copy mode.
5. It renders the grid with a block cursor + visual selection + a status bar,
   intercepts keys for vi motions, and `copy_to_clipboard` on yank.
6. On exit, it **un-suppresses the target explicitly** (`show_pane_with_id`) then
   `close_self`. NOTE: `replace_pane_with_existing_pane` keys the suppressed pane
   by its *own* id, so `close_self` alone does **not** auto-restore it (only the
   editor/`pid` path does) — the explicit un-suppress is required.

We render nothing until both the scrollback is loaded *and* our pane has landed
in the slot (no longer floating), to hide the brief floating-launch frame.

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
            x "0"
            y "0"
            width "100%"
            height "100%"
            skip_cache true
        };
        SwitchToMode "Normal"
    }
}
```

(`skip_cache true` is for development — it forces a recompile so rebuilds show
up; drop it once the plugin is stable.)

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
