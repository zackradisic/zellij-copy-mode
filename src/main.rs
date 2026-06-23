//! zellij-copy-mode — a tmux-style copy mode for Zellij, as a plugin.
//!
//! Architecture:
//!   1. The keybind opens us as a floating pane. We find the *target* (the
//!      focused, tiled terminal in our tab) from PaneUpdate, then resize our own
//!      floating pane to exactly cover it (`change_floating_panes_coordinates`),
//!      borderless. We never replace/suppress the target — it stays live behind
//!      us — so there are no ghost panes and nothing to restore on exit.
//!      Resizing early (before the first paint) keeps entry flash-free.
//!   2. We read the target's scrollback two ways: `get_pane_scrollback` gives
//!      plain text immediately (monochrome), then `DumpScreen --ansi` writes the
//!      colored scrollback to a file (`/host` == plugin cwd) which we read back
//!      and re-parse — upgrading the grid to full color. Both go through `vte`
//!      into a grid of styled `Cell`s.
//!   3. We render that grid with a block cursor + visual selection + status bar,
//!      intercept keys for vi-style motions, and `copy_to_clipboard` on yank.
//!      The cursor/scroll open at the live position so entry is seamless.

use std::collections::BTreeMap;
use std::path::PathBuf;

use vte::{Params, Parser, Perform};
use zellij_tile::prelude::*;

register_plugin!(State);

// ---------------------------------------------------------------------------
// Styled-cell grid model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Style {
    fg: Color,
    bg: Color,
    bold: bool,
    italic: bool,
    underline: bool,
    reverse: bool,
}

impl Style {
    /// Build the SGR escape that establishes this style from a clean slate
    /// (we emit a reset first, so this only needs to turn things *on*).
    fn sgr(&self) -> String {
        let mut codes: Vec<String> = vec!["0".into()];
        if self.bold {
            codes.push("1".into());
        }
        if self.italic {
            codes.push("3".into());
        }
        if self.underline {
            codes.push("4".into());
        }
        if self.reverse {
            codes.push("7".into());
        }
        match self.fg {
            Color::Default => {}
            Color::Indexed(i) => codes.push(format!("38;5;{i}")),
            Color::Rgb(r, g, b) => codes.push(format!("38;2;{r};{g};{b}")),
        }
        match self.bg {
            Color::Default => {}
            Color::Indexed(i) => codes.push(format!("48;5;{i}")),
            Color::Rgb(r, g, b) => codes.push(format!("48;2;{r};{g};{b}")),
        }
        format!("\x1b[{}m", codes.join(";"))
    }
}

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            style: Style::default(),
        }
    }
}

type Line = Vec<Cell>;

/// vte sink: feed it the raw ANSI dump bytes, get back a `Vec<Line>`.
#[derive(Default)]
struct GridBuilder {
    lines: Vec<Line>,
    cur: Line,
    style: Style,
}

impl GridBuilder {
    fn finish(mut self) -> Vec<Line> {
        if !self.cur.is_empty() {
            self.lines.push(std::mem::take(&mut self.cur));
        }
        self.lines
    }

    fn apply_sgr(&mut self, params: &Params) {
        // Flatten subparams (handles both `;` and `:` separated SGR).
        let mut flat: Vec<u16> = Vec::new();
        for p in params.iter() {
            for &v in p {
                flat.push(v);
            }
        }
        if flat.is_empty() {
            self.style = Style::default();
            return;
        }
        let mut i = 0;
        while i < flat.len() {
            match flat[i] {
                0 => self.style = Style::default(),
                1 => self.style.bold = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                7 => self.style.reverse = true,
                22 => self.style.bold = false,
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                27 => self.style.reverse = false,
                30..=37 => self.style.fg = Color::Indexed((flat[i] - 30) as u8),
                39 => self.style.fg = Color::Default,
                90..=97 => self.style.fg = Color::Indexed((flat[i] - 90 + 8) as u8),
                40..=47 => self.style.bg = Color::Indexed((flat[i] - 40) as u8),
                49 => self.style.bg = Color::Default,
                100..=107 => self.style.bg = Color::Indexed((flat[i] - 100 + 8) as u8),
                38 | 48 => {
                    let is_fg = flat[i] == 38;
                    let color = match flat.get(i + 1) {
                        Some(5) => {
                            let c = Color::Indexed(*flat.get(i + 2).unwrap_or(&0) as u8);
                            i += 2;
                            c
                        }
                        Some(2) => {
                            let r = *flat.get(i + 2).unwrap_or(&0) as u8;
                            let g = *flat.get(i + 3).unwrap_or(&0) as u8;
                            let b = *flat.get(i + 4).unwrap_or(&0) as u8;
                            i += 4;
                            Color::Rgb(r, g, b)
                        }
                        _ => Color::Default,
                    };
                    if is_fg {
                        self.style.fg = color;
                    } else {
                        self.style.bg = color;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

impl Perform for GridBuilder {
    fn print(&mut self, c: char) {
        self.cur.push(Cell {
            ch: c,
            style: self.style,
        });
    }

    fn execute(&mut self, byte: u8) {
        if byte == b'\n' {
            self.lines.push(std::mem::take(&mut self.cur));
        }
        // '\r' and other C0 controls: ignored for our line-grid purposes.
    }

    fn csi_dispatch(&mut self, params: &Params, _inter: &[u8], _ignore: bool, action: char) {
        if action == 'm' {
            self.apply_sgr(params);
        }
        // Other CSI (cursor moves etc.) are not meaningful for a static dump.
    }
}

fn parse_dump(bytes: &[u8]) -> Vec<Line> {
    let mut parser = Parser::new();
    let mut builder = GridBuilder::default();
    for &b in bytes {
        parser.advance(&mut builder, b);
    }
    builder.finish()
}

// ---------------------------------------------------------------------------
// Plugin state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Normal,
    /// character-wise visual select
    Visual,
    /// line-wise visual select
    VisualLine,
    /// incremental search input
    Search,
}

#[derive(Default)]
struct State {
    permissions_granted: bool,

    /// Our own pane id (PaneId::Plugin), from get_plugin_ids().
    self_id: Option<PaneId>,
    /// Host-side cwd; the folder Zellij mounts at `/host` inside our sandbox.
    host_cwd: PathBuf,

    /// The terminal pane we are providing copy mode for.
    target: Option<PaneId>,
    /// Best-effort tracking of the most-recently focused terminal pane, so we
    /// can pick the target even after we've stolen focus. TODO(live): this has
    /// a race; a pipe/keybind that hands us the pane id explicitly is cleaner.
    last_focused_terminal: Option<PaneId>,

    /// True once we've issued the scrollback dump.
    entered: bool,
    /// True once we have (monochrome) scrollback parsed into `grid`.
    loaded: bool,
    /// True once we've upgraded the grid to the colored ANSI dump.
    colored: bool,

    /// Target pane geometry (x, y, columns, rows) captured at acquisition, so we
    /// can size our floating overlay to exactly cover that pane.
    target_geom: Option<(usize, usize, usize, usize)>,
    /// Live cursor position (x, y) of the target pane at entry, if known.
    target_cursor: Option<(usize, usize)>,
    /// Grid row where the target's on-screen viewport begins (i.e. number of
    /// scrollback lines above it). Used to map the live cursor into the grid.
    viewport_start: usize,
    /// Set on load so the first render scrolls to show the bottom (the live view).
    needs_initial_scroll: bool,

    grid: Vec<Line>,
    /// First visible grid row.
    scroll: usize,
    /// Cursor position in grid coords.
    cur_row: usize,
    cur_col: usize,
    /// Last known content size of our pane.
    rows: usize,
    cols: usize,

    mode: Mode,
    /// Selection anchor (set when entering Visual/VisualLine).
    anchor: Option<(usize, usize)>,

    /// Search query buffer + last committed query for n/N.
    search_input: String,
    last_query: String,

    status: String,

    /// How many times we've polled for the dump file.
    dump_attempts: u32,

    // --- live diagnostics ---
    dbg_panes: String,
}

const DUMP_NAME: &str = ".zellij-copy-mode-dump.ansi";

impl State {
    fn dump_host_path(&self) -> String {
        // DumpScreen writes a raw *host* path; we read it back through the `/host`
        // mount. Verified in the server source: `/host` maps to the plugin's cwd
        // (== initial_cwd), so these two resolve to the same file.
        format!("{}/{DUMP_NAME}", self.host_cwd.display())
    }

    fn dump_sandbox_path(&self) -> String {
        format!("/host/{DUMP_NAME}")
    }

    /// Resize our own floating pane to exactly cover the target pane (position +
    /// size), borderless — so copy mode appears in place of that pane, working
    /// correctly even with split layouts.
    fn cover_target(&self) {
        let Some(PaneId::Plugin(id)) = self.self_id else {
            return;
        };
        let Some((x, y, cols, rows)) = self.target_geom else {
            return;
        };
        if let Some(coords) = FloatingPaneCoordinates::new(
            Some(x.to_string()),
            Some(y.to_string()),
            Some(cols.to_string()),
            Some(rows.to_string()),
            None,
            Some(true), // borderless
        ) {
            change_floating_panes_coordinates(vec![(PaneId::Plugin(id), coords)]);
        }
    }

    /// Read the target's scrollback into our grid. We stay a floating pane (sized
    /// to cover the screen, set in load()) — we never replace/suppress the target,
    /// so there's no float→tiled transition (that was the flash) and nothing to
    /// restore on exit.
    fn enter(&mut self) {
        let Some(target) = self.target else {
            return;
        };
        self.entered = true;
        // Size ourselves to exactly cover the target pane (done here, before the
        // first content paint, which is what keeps it flash-free).
        self.cover_target();
        // Grab the keyboard for vi-style motions (so zellij keybinds don't eat them).
        intercept_key_presses();
        // Read the live pane's scrollback. We run the text through the same vte
        // parser, so if it carries ANSI we keep color; if it's plain we render
        // monochrome.
        match get_pane_scrollback(target, true) {
            Ok(c) => {
                // The viewport (what's currently on screen) starts after the
                // scrollback lines above it.
                self.viewport_start = c.lines_above_viewport.len();
                let mut text = c.lines_above_viewport.join("\n");
                if !c.lines_above_viewport.is_empty() {
                    text.push('\n');
                }
                text.push_str(&c.viewport.join("\n"));
                self.ingest_dump(text.into_bytes());

                // Place the cursor where the terminal cursor was: viewport row +
                // the live cursor's (x, y) within the viewport.
                if let Some((cx, cy)) = self.target_cursor {
                    self.cur_row = self.viewport_start.saturating_add(cy);
                    self.cur_col = cx;
                }
                self.clamp_cursor();
                self.needs_initial_scroll = true;
            }
            Err(e) => self.status = format!("scrollback err: {e}"),
        }

        // get_pane_scrollback is plain text (monochrome). Kick off a colored
        // dump in parallel: DumpScreen --ansi writes the full scrollback with
        // SGR escapes to a file we read back and re-parse. The target is live
        // (we never suppress it), so this works.
        let mut ctx = BTreeMap::new();
        ctx.insert("copy_dump".to_string(), "1".to_string());
        run_action(
            actions::Action::DumpScreen {
                file_path: Some(self.dump_host_path()),
                include_scrollback: true,
                pane_id: Some(target),
                ansi: true,
            },
            ctx,
        );
        self.dump_attempts = 0;
        set_timeout(0.05);
    }

    /// Poll for the colored ANSI dump; when it arrives, upgrade the grid to it,
    /// preserving the current cursor/scroll.
    fn poll_dump(&mut self) {
        if self.colored {
            return;
        }
        self.dump_attempts += 1;
        match std::fs::read(self.dump_sandbox_path()) {
            Ok(bytes) if !bytes.is_empty() => {
                let grid = parse_dump(&bytes);
                if !grid.is_empty() {
                    self.grid = grid;
                    self.colored = true;
                    self.clamp_cursor();
                    let _ = std::fs::remove_file(self.dump_sandbox_path());
                    return;
                }
                if self.dump_attempts < 40 {
                    set_timeout(0.05);
                }
            }
            _ => {
                if self.dump_attempts < 40 {
                    set_timeout(0.05);
                }
                // else: give up silently and keep the monochrome grid.
            }
        }
    }

    fn ingest_dump(&mut self, bytes: Vec<u8>) {
        let n = bytes.len();
        self.grid = parse_dump(&bytes);
        if self.grid.is_empty() {
            self.grid.push(Vec::new());
        }
        self.loaded = true;
        // Start at the bottom (live tail), like tmux.
        self.cur_row = self.grid.len().saturating_sub(1);
        self.cur_col = 0;
        self.scroll_to_cursor();
        self.status = format!("COPY ({} lines, {n} bytes)", self.grid.len());
    }

    fn exit(&mut self) {
        // Non-destructive: we never suppressed the target, so just drop the key
        // grab and close our floating pane.
        clear_key_presses_intercepts();
        close_self();
    }

    // --- geometry helpers ---

    fn line_len(&self, row: usize) -> usize {
        self.grid.get(row).map(|l| l.len()).unwrap_or(0)
    }

    fn clamp_cursor(&mut self) {
        if self.grid.is_empty() {
            self.cur_row = 0;
            self.cur_col = 0;
            return;
        }
        self.cur_row = self.cur_row.min(self.grid.len() - 1);
        let max_col = self.line_len(self.cur_row).saturating_sub(1);
        self.cur_col = self.cur_col.min(max_col);
    }

    fn scroll_to_cursor(&mut self) {
        // Visible content height excludes the status bar row.
        let rows = self.rows.saturating_sub(1).max(1);
        if self.cur_row < self.scroll {
            self.scroll = self.cur_row;
        } else if self.cur_row >= self.scroll + rows {
            self.scroll = self.cur_row + 1 - rows;
        }
    }

    // --- selection ---

    /// Normalized (start, end) of the current selection in reading order.
    fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        let cur = (self.cur_row, self.cur_col);
        Some(if anchor <= cur {
            (anchor, cur)
        } else {
            (cur, anchor)
        })
    }

    fn is_selected(&self, row: usize, col: usize) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        match self.mode {
            Mode::VisualLine => row >= start.0 && row <= end.0,
            Mode::Visual => {
                let after_start = row > start.0 || (row == start.0 && col >= start.1);
                let before_end = row < end.0 || (row == end.0 && col <= end.1);
                after_start && before_end
            }
            _ => false,
        }
    }

    fn selected_text(&self) -> String {
        let Some((start, end)) = self.selection() else {
            return String::new();
        };
        let mut out = String::new();
        for row in start.0..=end.0 {
            let line = match self.grid.get(row) {
                Some(l) => l,
                None => continue,
            };
            let (c0, c1) = match self.mode {
                Mode::VisualLine => (0, line.len()),
                Mode::Visual => {
                    let lo = if row == start.0 { start.1 } else { 0 };
                    let hi = if row == end.0 { end.1 + 1 } else { line.len() };
                    (lo, hi.min(line.len()))
                }
                _ => (0, line.len()),
            };
            for cell in &line[c0..c1.max(c0)] {
                out.push(cell.ch);
            }
            if row != end.0 {
                out.push('\n');
            }
        }
        // tmux trims trailing whitespace on each copied line; do the same.
        out.split('\n')
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn yank(&mut self) {
        let text = self.selected_text();
        if !text.is_empty() {
            copy_to_clipboard(text);
        }
        self.exit();
    }

    // --- search (minimal) ---

    fn run_search(&mut self, forward: bool) {
        if self.last_query.is_empty() {
            return;
        }
        let q = self.last_query.to_lowercase();
        let n = self.grid.len();
        if n == 0 {
            return;
        }
        // Search line-by-line starting from the row after/before the cursor.
        let order: Vec<usize> = if forward {
            (0..n).map(|i| (self.cur_row + 1 + i) % n).collect()
        } else {
            (0..n).map(|i| (self.cur_row + n - 1 - i) % n).collect()
        };
        for row in order {
            let hay: String = self.grid[row].iter().map(|c| c.ch).collect();
            if let Some(byte_idx) = hay.to_lowercase().find(&q) {
                // Approximate column as char count up to the match.
                let col = hay[..byte_idx].chars().count();
                self.cur_row = row;
                self.cur_col = col;
                self.scroll_to_cursor();
                return;
            }
        }
        self.status = format!("not found: {}", self.last_query);
    }

    // --- key handling ---

    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        if self.mode == Mode::Search {
            return self.handle_search_key(key);
        }
        let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
        let page = self.rows.max(1);
        let half = (page / 2).max(1);
        match key.bare_key {
            BareKey::Esc => {
                if self.mode == Mode::Normal {
                    self.exit();
                } else {
                    self.mode = Mode::Normal;
                    self.anchor = None;
                }
            }
            BareKey::Char('q') => self.exit(),
            BareKey::Char('h') | BareKey::Left => self.cur_col = self.cur_col.saturating_sub(1),
            BareKey::Char('l') | BareKey::Right => self.cur_col += 1,
            BareKey::Char('j') | BareKey::Down => self.cur_row += 1,
            BareKey::Char('k') | BareKey::Up => self.cur_row = self.cur_row.saturating_sub(1),
            BareKey::Char('0') => self.cur_col = 0,
            BareKey::Char('$') => self.cur_col = self.line_len(self.cur_row).saturating_sub(1),
            // Ctrl-guarded scroll motions must precede the bare 'b'/'f' arms.
            BareKey::Char('d') if ctrl => self.cur_row += half,
            BareKey::Char('u') if ctrl => self.cur_row = self.cur_row.saturating_sub(half),
            BareKey::Char('f') if ctrl => self.cur_row += page,
            BareKey::Char('b') if ctrl => self.cur_row = self.cur_row.saturating_sub(page),
            BareKey::Char('w') => self.word_forward(),
            BareKey::Char('b') => self.word_back(),
            BareKey::Char('g') => self.cur_row = 0,
            BareKey::Char('G') => self.cur_row = self.grid.len().saturating_sub(1),
            BareKey::Char('v') => self.toggle_visual(Mode::Visual),
            BareKey::Char('V') => self.toggle_visual(Mode::VisualLine),
            BareKey::Char('y') | BareKey::Enter => {
                if self.anchor.is_some() {
                    self.yank();
                    return true;
                }
            }
            BareKey::Char('/') => {
                self.mode = Mode::Search;
                self.search_input.clear();
                self.status = "/".into();
            }
            BareKey::Char('n') => self.run_search(true),
            BareKey::Char('N') => self.run_search(false),
            _ => return false,
        }
        self.clamp_cursor();
        self.scroll_to_cursor();
        true
    }

    fn handle_search_key(&mut self, key: KeyWithModifier) -> bool {
        match key.bare_key {
            BareKey::Esc => {
                self.mode = Mode::Normal;
                self.search_input.clear();
                self.status = "COPY".into();
            }
            BareKey::Enter => {
                self.last_query = std::mem::take(&mut self.search_input);
                self.mode = Mode::Normal;
                self.run_search(true);
            }
            BareKey::Backspace => {
                self.search_input.pop();
                self.status = format!("/{}", self.search_input);
            }
            BareKey::Char(c) => {
                self.search_input.push(c);
                self.status = format!("/{}", self.search_input);
            }
            _ => return false,
        }
        true
    }

    fn toggle_visual(&mut self, mode: Mode) {
        if self.mode == mode {
            self.mode = Mode::Normal;
            self.anchor = None;
        } else {
            self.mode = mode;
            self.anchor = Some((self.cur_row, self.cur_col));
        }
    }

    fn word_forward(&mut self) {
        let line: String = self
            .grid
            .get(self.cur_row)
            .map(|l| l.iter().map(|c| c.ch).collect())
            .unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        let mut i = self.cur_col;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() && self.cur_row + 1 < self.grid.len() {
            self.cur_row += 1;
            self.cur_col = 0;
        } else {
            self.cur_col = i;
        }
    }

    fn word_back(&mut self) {
        let line: String = self
            .grid
            .get(self.cur_row)
            .map(|l| l.iter().map(|c| c.ch).collect())
            .unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        let mut i = self.cur_col;
        while i > 0 && chars.get(i - 1).map_or(false, |c| c.is_whitespace()) {
            i -= 1;
        }
        while i > 0 && chars.get(i - 1).map_or(false, |c| !c.is_whitespace()) {
            i -= 1;
        }
        self.cur_col = i;
    }
}

// ---------------------------------------------------------------------------
// ZellijPlugin impl
// ---------------------------------------------------------------------------

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        let ids = get_plugin_ids();
        self.self_id = Some(PaneId::Plugin(ids.plugin_id));
        self.host_cwd = ids.initial_cwd;
        self.mode = Mode::Normal;

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::ReadPaneContents,
            PermissionType::WriteToClipboard,
            // Needed for run_action(DumpScreen) — the colored ANSI dump.
            PermissionType::RunActionsAsUser,
        ]);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
            EventType::Key,
            EventType::ActionComplete,
            EventType::Timer,
        ]);

    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.permissions_granted = true;
                self.maybe_enter();
                true
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.status = "permissions denied".into();
                true
            }
            Event::PaneUpdate(manifest) => {
                // Which tab are we in? (the tab that contains our own plugin pane)
                let my_tab = manifest.panes.iter().find_map(|(tab, panes)| {
                    panes
                        .iter()
                        .any(|p| p.is_plugin && Some(PaneId::Plugin(p.id)) == self.self_id)
                        .then_some(*tab)
                });
                // Target = the focused, tiled, terminal pane *in our tab*.
                // Restricting to our tab disambiguates multi-tab / multi-client
                // sessions where several panes report is_focused (one per tab).
                let mut summary = String::new();
                for (tab, panes) in &manifest.panes {
                    for p in panes {
                        let in_my_tab = Some(*tab) == my_tab;
                        if in_my_tab
                            && p.is_focused
                            && !p.is_plugin
                            && !p.is_suppressed
                            && !p.is_floating
                        {
                            self.last_focused_terminal = Some(PaneId::Terminal(p.id));
                            if self.target.is_none() {
                                self.target = Some(PaneId::Terminal(p.id));
                                // Capture the live cursor so copy mode opens with
                                // the cursor where the terminal cursor was.
                                self.target_cursor = p.cursor_coordinates_in_pane;
                                // Capture geometry so we cover exactly this pane.
                                self.target_geom = Some((
                                    p.pane_x,
                                    p.pane_y,
                                    p.pane_columns,
                                    p.pane_rows,
                                ));
                                // Resize to the target ASAP — if this PaneUpdate
                                // is processed before the first paint, it's
                                // flash-free (same trick as the load() resize).
                                self.cover_target();
                            }
                        }
                        let kind = if p.is_plugin { "P" } else { "T" };
                        let mut flags = String::new();
                        if p.is_focused {
                            flags.push('F');
                        }
                        if p.is_suppressed {
                            flags.push('S');
                        }
                        if p.is_floating {
                            flags.push('f');
                        }
                        summary.push_str(&format!("t{tab}:{kind}{}[{flags}] ", p.id));
                    }
                }
                self.dbg_panes = summary;
                self.maybe_enter();
                !self.loaded
            }
            Event::ActionComplete(..) => {
                // Backup path; the timer poll is the primary mechanism.
                if self.entered && !self.colored {
                    self.poll_dump();
                    return true;
                }
                false
            }
            Event::Timer(_) => {
                if self.entered && !self.colored {
                    self.poll_dump();
                    return true;
                }
                false
            }
            Event::Key(key) | Event::InterceptedKeyPress(key) => {
                if self.loaded {
                    self.handle_key(key)
                } else {
                    // Let the user dismiss the debug pane.
                    if matches!(key.bare_key, BareKey::Char('q') | BareKey::Esc) {
                        self.exit();
                    }
                    false
                }
            }
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.rows = rows;
        self.cols = cols;

        // Render nothing until the scrollback is loaded.
        if !self.loaded {
            return;
        }

        // Reserve the bottom row for the status bar so copy mode is visually
        // unmistakable (it otherwise looks identical to the live pane).
        let content_rows = rows.saturating_sub(1).max(1);

        // On first paint, scroll so the bottom of the scrollback sits at the
        // bottom of our view — i.e. show the same lines the live pane showed,
        // then keep the cursor visible. (Done here because `rows` is only known
        // at render time.)
        if self.needs_initial_scroll {
            self.scroll = self.grid.len().saturating_sub(content_rows);
            self.scroll_to_cursor();
            self.needs_initial_scroll = false;
        }

        let end = (self.scroll + content_rows).min(self.grid.len());
        for row in self.scroll..end {
            let line = &self.grid[row];
            let mut out = String::new();
            let mut last: Option<Style> = None;
            for (col, cell) in line.iter().enumerate().take(cols) {
                let mut style = cell.style;
                let is_cursor = row == self.cur_row && col == self.cur_col;
                if is_cursor || self.is_selected(row, col) {
                    style.reverse = !style.reverse;
                }
                if last != Some(style) {
                    out.push_str(&style.sgr());
                    last = Some(style);
                }
                out.push(cell.ch);
            }
            out.push_str("\x1b[0m");
            println!("{out}\r");
        }
        // Pad out any unused content rows so the status bar sits at the bottom.
        for _ in end..(self.scroll + content_rows) {
            println!("\r");
        }
        print!("{}", self.status_bar(cols));
    }
}

impl State {
    /// A distinctly-colored bottom bar: mode + position + key hints. This is the
    /// "you are in copy mode" signal so the view isn't mistaken for a live pane.
    fn status_bar(&self, cols: usize) -> String {
        let (label, hints) = match self.mode {
            Mode::Normal => ("COPY", "j/k move · v select · y yank · / search · q quit"),
            Mode::Visual => ("VISUAL", "move to extend · y yank · Esc cancel"),
            Mode::VisualLine => ("V-LINE", "move to extend · y yank · Esc cancel"),
            Mode::Search => ("SEARCH", ""),
        };
        let pos = format!("{}/{}", self.cur_row + 1, self.grid.len());
        let mut text = if self.mode == Mode::Search {
            format!(" {label}  /{}", self.search_input)
        } else {
            format!(" {label}  {pos}  {hints}")
        };
        // Pad/truncate to full width.
        let mut width = text.chars().count();
        if width > cols {
            text = text.chars().take(cols).collect();
            width = cols;
        }
        for _ in width..cols {
            text.push(' ');
        }
        // Bright bar: white text on blue background, bold.
        format!("\x1b[1m\x1b[48;5;24m\x1b[38;5;231m{text}\x1b[0m")
    }
}

impl State {
    /// Enter once we have permissions + a target pane, and aren't already in.
    fn maybe_enter(&mut self) {
        if self.permissions_granted && self.target.is_some() && !self.entered {
            self.enter();
        }
    }
}
