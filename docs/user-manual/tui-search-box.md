# TUI Search Box

> **Based on code version:** 0.4.11 | **Document revision:** 1.4

The UFFS TUI has two panes: the **Search Box** (query input) and the
**Results Panel** (file listing). A **focus system** determines which pane
receives keyboard input. The focused pane has a **bright cyan border**; the
unfocused pane has a dim gray border.

Press **Esc** to toggle focus between the two panes. The TUI starts with
focus on the Search Box.

The search box supports real-time search-as-you-type, multiple pattern
syntaxes, search mode toggles, and full text-editing capabilities. Results
update live as you type.

![UFFS TUI: launch against a hot daemon, type a query, and browse real NTFS results live](../../assets/demo/uffs-tui.gif)

> *Launch, type, and browse real NTFS results live against a hot daemon. Recorded against the real binary (see the [demo kit](../../scripts/dev/demo/README.md)).*

## 1. Search Patterns

Type directly into the search box. The search engine auto-detects the pattern
type based on syntax.

### 1.1 Substring (literal)

Type plain text with no wildcards. Matches any filename or path containing the
text (case-insensitive by default).

| Input | Matches |
|-------|---------|
| `readme` | `README.md`, `readme.txt`, `C:\docs\readme_v2.txt` |
| `invoice` | `invoice_2024.pdf`, `old_invoices\summary.xlsx` |

Literal patterns match against the **full path** (like Everything and WizFile).

### 1.2 Glob Patterns

Use `*` and `?` wildcards for glob matching.

| Pattern | Meaning | Example matches |
|---------|---------|-----------------|
| `*` | All files (empty search box also shows all) | Everything |
| `*.rs` | Files ending in `.rs` | `main.rs`, `lib.rs` |
| `foo*` | Files starting with `foo` | `foobar.txt`, `foo.cfg` |
| `*bar` | Files ending with `bar` | `sidebar`, `foobar` |
| `*needle*` | Files containing `needle` | `my_needle_file.txt` |
| `foo*bar` | Files starting with `foo` and ending with `bar` | `foobar`, `foo_test_bar` |
| `file?.txt` | Single-character wildcard | `file1.txt`, `fileA.txt` |
| `*.txt\|*.log` | OR operator — matches either pattern | `notes.txt`, `app.log` |

### 1.3 Path / Tree Patterns

Include path separators (`\` or `/`) to search within directory trees.

| Pattern | Meaning |
|---------|---------|
| `\Users\*` | Files directly under any `Users` directory |
| `\Users\**` | Files anywhere under `Users` (recursive) |
| `C:\Temp\*.tmp` | `.tmp` files in `C:\Temp` |
| `/projects/*.rs` | `.rs` files in any `projects` directory |

Forward slashes are automatically converted to backslashes for NTFS matching.

### 1.4 Regex Patterns

Prefix with `>` to use regular expressions.

| Pattern | Meaning |
|---------|---------|
| `>.*\.txt$` | Files ending in `.txt` (regex) |
| `>report_\d{4}` | Files with `report_` followed by 4 digits |
| `>[Rr]eadme` | Case-sensitive regex for `Readme` or `readme` |

Regex patterns are auto-anchored with `$` at the end if not already present,
so `>.*\.jpg` matches files **ending** in `.jpg` (not `.jpg.bak`).

### 1.5 Drive Prefix

Prefix any pattern with a drive letter to restrict search to that drive.

| Pattern | Meaning |
|---------|---------|
| `c:*.exe` | `.exe` files on drive C only |
| `d:/projects/**` | Everything under `projects` on drive D |

### 1.6 Empty Search Box

When the search box is empty, UFFS shows **all files** (equivalent to `*`),
sorted by the current sort column (default: most recently modified first).

## 2. Search Mode Toggles

Toggles modify how the search engine interprets your pattern. Active toggles
appear as **yellow badges** in the search box title bar. Inactive toggles show
their shortcut key as a hint (e.g., `[Cc:Tab]`).

### 2.1 Case-Sensitive `[Cc]`

When active, searches match exact letter casing. When inactive (default),
`readme` matches `README`, `Readme`, `readme`, etc.

### 2.2 Whole Word `[W]`

When active, the pattern must match as a complete word (bounded by word
boundaries). `log` matches the word "log" but not "blog" or "logging".

Internally, whole-word wraps the pattern in `\b...\b` regex word boundaries.

### 2.3 Name-Only `[NAME]`

When active, patterns match only against the **filename** (not the full path).

## 3. Keybindings: Windows Preset (Default)

The Windows preset is the default. Keys follow common Windows conventions
(VS Code, Everything file search) with Ctrl-key backups for terminals where
Alt does not work (e.g., macOS Terminal.app).

### 3.1 Application Keys

| Key | Backup(s) | Action |
|-----|-----------|--------|
| `Ctrl+Q` | | Quit the TUI |
| `Ctrl+R` | | Refresh — reload all drives from MFT / cache |
| `Alt+H` | `Ctrl+G`, `F1` | Cycle help bar pages (Nav → Toggles → Edit → Patterns) |

### 3.2 Search Box: Text Editing

| Key | Action |
|-----|--------|
| *(type)* | Characters are inserted at the cursor; search updates live |
| `Ctrl+U` | Clear the entire search line |
| `Ctrl+Z` | Undo last edit |
| `Ctrl+Y` | Redo last undone edit |
| `Ctrl+A` | Select all text in the search box |
| `Ctrl+C` | Copy selected text (internal clipboard) |
| `Ctrl+V` | Paste from internal clipboard |
| `Ctrl+X` | Cut selected text (handled by the text widget) |
| `Shift+Left/Right` | Select text character by character |
| `Shift+Home/End` | Select to start/end of line |
| `Left/Right` | Move cursor |
| `Home/End` | Jump to start/end of line |
| `Backspace` | Delete character before cursor |
| `Delete` | Delete character after cursor |

> **Note on clipboard:** Copy/Cut/Paste use the text widget's internal
> clipboard (yank buffer), not the system clipboard. To paste from another
> application, use your terminal's paste mechanism (e.g., `Cmd+V` on macOS,
> right-click paste on Windows Terminal).

### 3.3 Search Box: Mode Toggles

| Key | Backup | Action | Title badge |
|-----|--------|--------|-------------|
| `Alt+C` | `Tab` | Toggle case-sensitive search | `[Cc]` |
| `Alt+W` | `Ctrl+W` | Toggle whole-word search | `[W]` |
| `Alt+N` | `Ctrl+F` | Toggle name-only matching | `[NAME]` |
| `Alt+T` | `Ctrl+T` | Cycle filter (All → Files → Dirs) | `[FILES]` / `[DIRS]` |

When a toggle is **inactive**, the title bar badge shows the shortcut hint
(e.g., `[Cc:Tab]`). When **active**, the badge turns yellow: `[Cc]`.

> **macOS note:** The Alt (Option) key inserts special characters in most macOS
> terminals instead of sending a modifier. Use the Ctrl backup keys, or
> configure your terminal to send Alt as Meta/Esc+ (iTerm2: Preferences →
> Profiles → Keys → "Left Option key" → Esc+).

### 3.4 Search Box: History

Search history is saved automatically. When you clear the search box, the
longest pattern from your typing session is committed to history. Each
history entry captures the **full search state**: pattern, case-sensitive,
whole-word, name-only, and filter mode — stored as a `uffs` CLI command.

History persists across sessions in `search_history.txt` alongside the
config file. A default history file with example searches is created on
first launch.

When the **Search Box is focused**, the arrow keys browse history:

| Key | Action |
|-----|--------|
| `↑` | Browse to previous (older) search |
| `↓` | Browse to next (newer) search |

The first press saves your current input. Browsing past the newest entry
restores your original text.

When browsing a history entry, all search toggles (Cc, W, NAME, filter)
are automatically restored to match the state when the search was saved.

#### History File Format

The history file uses CLI command syntax with optional `#` comments:

```bash
# Find all Rust source files
uffs "*.rs" --files-only

# Case-sensitive search for README files
uffs README --case --name-only

# User-generated (no comment)
uffs "*.log"
```

Each non-comment, non-blank line is a `uffs` CLI command. Flags map to
search toggles:

| Flag | Toggle |
|------|--------|
| `--case` | Case-sensitive `[Cc]` |
| `--word` | Whole-word `[W]` |
| `--name-only` | Name-only `[NAME]` |
| `--files-only` | Filter: files only `[FILES]` |
| `--dirs-only` | Filter: dirs only `[DIRS]` |

#### History Comments in the Status Bar

When browsing a history entry that has a `#` comment, the status bar
temporarily changes its title to **"History Note"** and displays the
comment with a position indicator (e.g., `📝 Find all Rust source files
(3/12)`). Entries without comments leave the status bar unchanged.

### 3.5 Focus Switching

| Key | Action |
|-----|--------|
| `Esc` | Toggle focus between Search Box and Results Panel |

When the **Search Box** is focused (default on startup):
- Typing edits the search query
- `↑`/`↓` browse search history
- All search toggles and editing keys are active

When the **Results Panel** is focused:
- `↑`/`↓` navigate result rows
- `PgUp`/`PgDn` page through results
- `Enter` shows the full path of the selected file
- Sort keys (`Ctrl+Shift+S`, `Ctrl+Shift+D`) are active
- Search toggles (case, word, name-only, filter) still work

The focused pane has a **bright cyan border**; the unfocused pane has a
dim gray border.

## 4. Keybindings: Emacs Preset

Switch to the Emacs preset with:

```
uffs-tui --keys emacs
```

The Emacs preset changes text-editing keys to Emacs conventions. Toggle,
focus, and history keys remain the same.

### 4.1 Differences from Windows Preset

| Action | Windows | Emacs |
|--------|---------|-------|
| Clear line | `Ctrl+U` | `Ctrl+K` (kill to end of line) |
| Undo | `Ctrl+Z` | `Ctrl+/` |
| Redo | `Ctrl+Y` | `Ctrl+Shift+/` |
| Help cycle | `Alt+H`, `Ctrl+G`, `F1` | `Alt+H`, `F1` |

All other keys (toggles, focus, copy/paste, quit, refresh) are identical.

> **Note:** The Emacs preset does not include `Ctrl+G` for help because
> Emacs traditionally uses `Ctrl+G` for cancel/abort. Emacs users can
> customize this in `keys.toml` if desired.

## 5. Customizing Keybindings

Keybindings are stored in a TOML config file that is created on first launch.

### 5.1 Config File Location

| Platform | Path |
|----------|------|
| **Windows** | `%APPDATA%\uffs\keys.toml` |
| **macOS** | `~/Library/Application Support/uffs/keys.toml` |
| **Linux** | `~/.config/uffs/keys.toml` |

### 5.2 Switching Presets

```bash
uffs-tui --keys windows   # overwrite config with Windows preset
uffs-tui --keys emacs     # overwrite config with Emacs preset
```

### 5.3 Editing the Config File

Open `keys.toml` in any text editor. The format is:

```toml
[meta]
preset = "windows"

[app]
quit = ["ctrl+q"]
refresh = ["ctrl+r"]
help_cycle = ["alt+h"]

[search_box]
clear_line = ["ctrl+u"]
undo = ["ctrl+z"]
redo = ["ctrl+y"]
select_all = ["ctrl+a"]
copy = ["ctrl+c"]
paste = ["ctrl+v"]
toggle_name_only = ["alt+n", "ctrl+f"]
toggle_filter = ["alt+t", "ctrl+t"]
toggle_case_sensitive = ["alt+c", "tab"]
toggle_whole_word = ["alt+w", "ctrl+w"]
```

Each action accepts an array of key strings. The first entry is the primary
key; additional entries are backups. Supported key string syntax:

| Syntax | Example |
|--------|---------|
| Single key | `"tab"`, `"enter"`, `"up"`, `"down"`, `"pageup"` |
| Modifier + key | `"ctrl+z"`, `"alt+c"`, `"shift+tab"` |
| Multiple modifiers | `"ctrl+shift+s"` |

### 5.4 Auto-Backfill

The `[meta] preset` field tracks which preset your config originated from.
When a new version of UFFS adds new keybindings to the preset, they are
**automatically filled in** on next launch — without overwriting any keys
you have customized. This means you never need to manually update your
config file after upgrading.

## 6. Title Bar Anatomy

The search box title bar shows the current state at a glance:

```
┌ Search NTFS Drives [C D E F] 23,082,056 Files [Cc:Tab] [W:^W] ──────┐
│ your search pattern here                                              │
└───────────────────────────────────────────────────────────────────────┘
```

| Element | Meaning |
|---------|---------|
| `[C D E F]` | Loaded drives (each letter colored uniquely) |
| `23,082,056 Files` | Total indexed file records across all drives |
| `[Cc]` / `[Cc:Tab]` | Case-sensitive toggle (yellow = active, gray = inactive with hint) |
| `[W]` / `[W:^W]` | Whole-word toggle (yellow = active, gray = inactive with hint) |
| `[NAME]` | Name-only mode (shown only when active) |
| `[FILES]` / `[DIRS]` | Filter mode (shown only when active) |

## 7. Status Bar

Below the search box, the status bar shows search results summary:

```
┌ Status ──────────────────────────────────────────────────────────────┐
│ 1,234 matches  │  23ms  │  23,082,056 records across 4 drives       │
└──────────────────────────────────────────────────────────────────────┘
```

This includes match count, search duration, total records scanned, and
number of drives searched.

## 8. Help Bar

Press `Alt+H`, `Ctrl+G`, or `F1` to cycle through four help pages
at the bottom of the screen:

| Page | Content |
|------|---------|
| **Nav** | Focus-aware: history (search) or row nav (results), sort, quit |
| **Toggles** | Name-only, filter, case-sensitive, whole-word, refresh |
| **Edit** | Clear, undo, redo, select, copy, paste |
| **Patterns** | Substring, glob, wildcard, tree, recursive, regex syntax |

> **Dynamic labels:** The help bar reads key labels directly from the active
> keymap at runtime. If you customize bindings in `keys.toml`, the help bar
> automatically reflects your changes. On macOS, Alt-modified keys are
> automatically hidden in favor of Ctrl backup keys, since the Option key
> sends special characters in most macOS terminals.


## Document Revision History

| Revision | Date | Code Version | Changes |
|----------|------|--------------|---------|
| 1.0 | 2026-03-25 | 0.4.11 | Initial document covering search patterns, mode toggles, Windows/Emacs keybindings, customization, title bar anatomy, status bar, help bar. |
| 1.1 | 2026-03-25 | 0.4.11 | Added `F1` and `Ctrl+G` as backup help cycle keys (Windows preset). Added help cycle row to Emacs differences table. Added revision history. |
| 1.2 | 2026-03-25 | 0.4.11 | Help bar is now fully dynamic — reads key labels from the active keymap at runtime. Platform-aware: Alt keys automatically hidden on macOS in favor of Ctrl backups. Removed all hardcoded key strings from help bar rendering. |
| 1.3 | 2026-03-25 | 0.4.11 | Added focus system: Esc toggles between Search Box and Results Panel. ↑/↓ now browse search history when Search Box is focused, navigate rows when Results is focused. Visual focus indicator (cyan border). Removed explicit history_back/history_forward keybindings. Help bar is now focus-aware — Nav page shows context-appropriate keys. Help bar title shows current focus state. |
| 1.4 | 2026-03-25 | 0.4.11 | New history file format: entries stored as `uffs` CLI commands with optional `#` comments. History entries now capture full search state (pattern + all toggles). Browsing history restores toggles. Status bar shows "History Note" with comment text when browsing commented entries. Default history file with example searches shipped on first launch. |

## See also

- [Search Modes](search-modes.md): glob, literal and regex patterns
- [Daemon](daemon.md): the resident index server and how to manage it
- [Getting Started](getting-started.md): your first search and everyday recipes
