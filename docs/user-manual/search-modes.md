# Search Modes

UFFS supports several search modes that determine how your pattern is
interpreted and matched against the NTFS Master File Table.  Choosing the
right mode can dramatically improve both precision and performance.

## 1. Pattern Types at a Glance

| Syntax | Mode | Matches Against | Example |
|--------|------|-----------------|---------|
| `*.txt` | **Glob** | Filename | All `.txt` files |
| `report` | **Literal** | Full path (substring) | Any path containing `report` |
| `>.*\.log$` | **Regex** | Depends on pattern | Files ending in `.log` |
| `c:/pro*` | **Path-aware glob** | Full path | Paths on C: starting with `pro` |
| `*` | **Match-all** | Everything | Every file and directory |

UFFS auto-detects the mode from the pattern you type — no flag needed.

## 2. Glob Patterns (Default)

Any pattern containing wildcard characters (`*`, `?`, `[…]`) is treated as
a **glob**.  Globs are the most common and fastest search mode.

### Wildcards

| Token | Meaning | Example |
|-------|---------|---------|
| `*` | Zero or more characters | `*.rs` — all Rust source files |
| `**` | Zero or more path components | `**\src\**\*.rs` — `.rs` anywhere under a `src` directory |
| `?` | Exactly one character | `file?.txt` — `file1.txt`, `fileA.txt` |
| `[abc]` | One of the listed characters | `[abc].txt` — `a.txt`, `b.txt`, `c.txt` |
| `[a-z]` | Character range | `[a-z]*.dll` — DLLs starting with a lowercase letter |

### Specialised Glob Shortcuts

UFFS analyses every glob and compiles it into the most efficient internal
representation before matching.  You never need to think about this, but
it is why simple globs are so fast:

| Your Pattern | Internal Strategy | Why It Is Fast |
|-------------|-------------------|----------------|
| `*` | **Match-all** | Skips matching entirely |
| `*.txt` | **Extension lookup** | Uses the extension index — O(1) |
| `readme.txt` | **Exact match** | Direct string comparison |
| `foo*` | **Prefix check** | `starts_with("foo")` |
| `*bar` | **Suffix check** | `ends_with("bar")` |
| `*needle*` | **Contains check** | `contains("needle")` |
| `foo*bar` | **Prefix + Suffix** | Two checks, no regex |
| `*hallo*.txt` | **Complex glob → regex** | Compiled regex (still fast) |

### Glob Examples

```bash
# All PDF files on every drive
uffs '*.pdf'

# PowerPoint files on C: drive only
uffs 'c:/*.pptx'

# Any file with "report" in its name and a .xlsx extension
uffs '*report*.xlsx'

# Single-character variant matching
uffs 'log_202?.txt'
```

### Best Practice

- Prefer simple globs (`*.ext`, `prefix*`, `*suffix`) — they are the
  fastest because UFFS avoids regex entirely.
- Add a drive prefix when you know the drive:  `d:/*.iso` is faster than
  `*.iso` across all drives.
- Use `**` only when you genuinely need to match across multiple directory
  levels in a path-aware search.

## 3. Literal Search

A pattern with **no wildcards** is treated as a **literal substring match
against the full path**.  This is the most natural search for "I know part
of the name".

```bash
# Finds any file or directory whose full path contains "invoice"
# e.g.  D:\Clients\Acme\invoices\invoice_2026.pdf
#        C:\Users\me\Desktop\Invoice_Q1.docx
uffs invoice

# Case-insensitive by default — both of these find the same files
uffs readme
uffs README
```

> **Why full-path matching?**  Literal patterns match the **entire path**
> (like Everything and WizFile) so that `invoices` also matches the
> *directory* named `invoices`, not just files with that substring in their
> filename.  Use `--name-only` to restrict matching to the filename only
> (see §7).

### Best Practice

- Literal search is perfect for quick "I know a word in the filename or
  path" lookups.
- Combine with `--ext` or `--files-only` to narrow results fast.

## 4. Regex Search

Prefix your pattern with `>` to enter **regex mode**.  The regex is applied
to filenames by default, or to full paths if the regex contains path
separators.

```bash
# Files ending in .log (regex anchored to end)
uffs '>.*\.log$'

# Files with a date stamp  YYYY-MM-DD  in the name
uffs '>[0-9]{4}-[0-9]{2}-[0-9]{2}'

# Config files on C: drive only
uffs '>C:\\Users\\.*\.config$'

# Case-insensitive regex (default) — finds LOG, Log, log
uffs '>.*\.log$'
```

### Regex Tips

- The `>` prefix is **not** part of the regex — it is the mode selector.
- Quotes around the regex are stripped automatically, so
  `>'"C:\\Temp.*"'` works.
- Drive letters in the regex (e.g. `C:\\…`) are detected and used to
  scope the search to that drive.
- Use `--case` if you need case-sensitive regex matching.
- Regex is the **slowest** mode (patterns cannot use the extension index).
  Whenever possible, combine with `--ext` or other filters to shrink the
  candidate set first.

## 5. Path-Aware Patterns

A pattern is **path-aware** when it contains directory separators (`\` or
`/`).  Path-aware patterns are matched against the **full reconstructed
path** instead of just the filename.

```bash
# All .rs files under any "src" directory on C:
uffs 'c:/src/**/*.rs'

# Any file under a "backup" directory on any drive
uffs '/backup/*'

# Regex pattern matching paths under Users on D:
uffs '>D:\\Users\\.*\.docx$'
```

> Literal patterns (no wildcards) are **always path-aware** by default.
> Glob patterns are path-aware **only** if they contain separators.

## 6. Case Sensitivity

By default, all matching is **case-insensitive** — consistent with Windows
NTFS semantics.  You have two ways to enable case-sensitive matching:

### `--case` (Explicit Case-Sensitive)

```bash
# Only finds "README.md", not "readme.md" or "Readme.md"
uffs README.md --case
```

### `--smart-case` (Auto-Detect)

When `--smart-case` is enabled, UFFS becomes case-sensitive **only if your
pattern contains at least one uppercase letter**.  All-lowercase patterns
remain case-insensitive.  This mirrors the behaviour of `fd` and `ripgrep`.

```bash
# Case-insensitive (all lowercase)
uffs readme --smart-case

# Case-sensitive (contains uppercase "R")
uffs Readme --smart-case
```

### Best Practice

- Leave the default (case-insensitive) for day-to-day searching.
- Use `--smart-case` as a general habit if you prefer the `fd`/`rg`
  convention — it does the right thing most of the time.
- Use `--case` for exact forensic lookups where casing matters.

## 7. Whole-Word Matching (`--word`)

The `--word` flag wraps your pattern in `\b…\b` word-boundary anchors,
turning it into a regex that matches only at word boundaries.

```bash
# Finds "test.txt", "my_test_data.csv" but NOT "testing.log", "latest.doc"
uffs test --word

# Combine with other flags
uffs error --word --ext log --newer 7d
```

This is particularly useful for common words like `test`, `data`, `log`,
or `error` that frequently appear as substrings of longer names.

## 8. Name-Only Matching (`--name-only`)

By default, literal patterns match against the **full path**.  The
`--name-only` flag restricts matching to the **filename component only**
(the last segment of the path).

```bash
# Default: matches paths like D:\projects\hallo_world\readme.txt
#          (because "hallo" appears in a directory name)
uffs hallo

# --name-only: only matches if "hallo" is in the filename itself
# e.g. hallo.txt, hallo_world.py — but NOT D:\hallo\readme.txt
uffs hallo --name-only
```

> `--name-only` is **incompatible** with patterns that contain path
> separators (`\` or `/`) — the search will return an error.

## 9. Match-All Pattern (`*`)

The single wildcard `*` matches **every** file and directory.  It is the
starting point for pure-filter workflows where you don't care about the
name at all.

```bash
# All files larger than 1 GB
uffs '*' --min-size 1GB --files-only

# All empty directories
uffs '*' --dirs-only --max-descendants 0

# Everything modified in the last 24 hours
uffs '*' --newer 1d
```

## 10. Scope Prefixes

UFFS supports **Everything-compatible scope prefixes** that modify how
the pattern is applied.  Prefix the pattern with `path:`, `dir:`, or
`file:` to change the search scope:

| Prefix | Effect | Equivalent |
|--------|--------|------------|
| `path:` | Match against the **full path** (not just filename) | path matching enabled |
| `dir:` | Only search **directories** | `--dirs-only` |
| `file:` | Only search **files** | `--files-only` |

### Examples

```bash
# Find any path containing "projects" (matches directory components)
uffs 'path:projects'

# Find any path containing windows\system32
uffs 'path:*windows\system32*'

# Find directories named "build"
uffs 'dir:build'

# Find files named "README" (no directories)
uffs 'file:README*'
```

> **Implementation:** These are client-side sugar.  `path:` sets
> `match_path=true` on the `SearchParams`.  `dir:` sets `dirs_only=true`.
> `file:` sets `files_only=true`.  The prefix is stripped before sending
> the pattern to the daemon.

## 11. Pattern Sugar Flags

These flags generate patterns from simple keywords, saving you from
writing glob syntax:

| Flag | Effect | Equivalent Pattern |
|------|--------|-------------------|
| `--begins-with <PREFIX>` | Files starting with PREFIX | `PREFIX*` |
| `--ends-with <SUFFIX>` | Files ending with SUFFIX | `*SUFFIX` |
| `--contains <NEEDLE>` | Files containing NEEDLE | `*NEEDLE*` |
| `--not-contains <NEEDLE>` | Exclude files containing NEEDLE | `--exclude '*NEEDLE*'` |

### Examples

```bash
# All files starting with "report"
uffs --begins-with report

# All files ending with "_backup"
uffs --ends-with _backup

# All files containing "invoice" in their name
uffs --contains invoice

# All .log files, but exclude those containing "debug"
uffs '*.log' --not-contains debug
```

> `--begins-with`, `--ends-with`, and `--contains` are mutually exclusive
> with each other and with providing a pattern directly.  `--not-contains`
> can be combined with any of them (it maps to `--exclude`).

## 12. Path Directory Filter

The `--in-path` flag restricts results to files whose **directory path**
matches a glob — independent of the search pattern.  This is different
from `path:` (which matches the pattern against the full path).

```bash
# .rs files only under directories containing "projects"
uffs '*.rs' --in-path '*projects*'

# DLLs only under System32
uffs '*.dll' --in-path '*windows\system32*'
```

> **`path:` vs `--in-path`:** `path:report` matches "report" anywhere in
> the full path (including the filename).  `--in-path '*report*'` matches
> "report" only in the directory portion — the filename is excluded from
> the match.  See [Filters §8](filters.md) for details.

## 13. Drive Scoping

You can scope your search to specific drives using three methods:

### In-Pattern Drive Prefix

```bash
# Search only C: drive
uffs 'c:/*.dll'
```

### `--drive` Flag (Single Drive)

```bash
uffs '*.exe' --drive C
```

### `--drives` Flag (Multiple Drives)

```bash
# Search C: and D: concurrently
uffs '*.pdf' --drives C,D,E
```

Drive letters are case-insensitive.  You may include the colon
(`C:` or `C`) — both work.

## 14. Performance Mental Model

Not all searches are equal.  Here is a rough performance ranking from
fastest to slowest:

| Rank | Pattern Style | Why |
|------|---------------|-----|
| 1 | `*.ext` (extension glob) | Extension index: O(1) lookup |
| 2 | `readme.txt` (exact name) | Direct string comparison |
| 3 | `prefix*` or `*suffix` | Single `starts_with` / `ends_with` |
| 4 | `*needle*` (contains) | Substring scan |
| 5 | Complex glob (`*foo*.bar`) | Compiled regex |
| 6 | `>regex` | Full regex engine |

**General advice:**

- Start with the simplest pattern that describes what you want.
- Add filters (`--ext`, `--min-size`, `--newer`, etc.) instead of making
  the pattern more complex.
- Scope to a single drive when possible — it halves the search space
  per extra drive you exclude.

## 15. Quick Reference

```text
PATTERN SYNTAX
  *.txt                Glob — all .txt files
  report               Literal — paths containing "report"
  >.*\.log$            Regex — files ending in .log
  c:/projects/*        Path-aware glob — on C: under projects
  *                    Match-all — every file and directory

SCOPE PREFIXES (Everything-compatible)
  path:report          Match "report" against full path
  dir:build            Only match directories named "build"
  file:README*         Only match files starting with "README"

MODIFIERS
  --case               Case-sensitive matching
  --smart-case         Auto case-sensitive if pattern has uppercase
  --word               Whole-word boundaries (\b…\b)
  --name-only          Match filename only, not full path

PATTERN SUGAR
  --begins-with PREFIX Sugar for 'PREFIX*'
  --ends-with SUFFIX   Sugar for '*SUFFIX'
  --contains NEEDLE    Sugar for '*NEEDLE*'
  --not-contains NEEDLE Sugar for --exclude '*NEEDLE*'

PATH FILTER
  --in-path <GLOB>     Filter by directory path (not filename)

DRIVE SCOPING
  --drive C            Single drive
  --drives C,D,E       Multiple drives
  c:/*.ext             Drive prefix in pattern
```

## See also

- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Filters](filters.md): narrow results by size, date, type, attributes and more
- [Sorting](sorting.md): sort columns and multi-tier sorts
- [Output Formats](output-formats.md): CSV, JSON and table output, and choosing columns
