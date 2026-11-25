# Search Modes

UFFS supports several search modes that determine how your pattern is
interpreted and matched against the NTFS Master File Table.  Choosing the
right mode can dramatically improve both precision and performance.

## 1. Pattern Types at a Glance

| Syntax | Mode | Matches Against | Example |
|--------|------|-----------------|---------|
| `*.txt` | **Glob** | File name | All `.txt` files |
| `report` | **Literal** | File name (substring) | Any name containing `report` |
| `>.*\.log$` | **Regex** | File name | Names ending in `.log` |
| `src\*.rs` | **Path-aware glob** | Directory names, then the file name | `.rs` entries directly inside any directory named `src` |
| `*` | **Match-all** | Everything | Every file and directory |
| `C:` or `C:\` | **Match-all on a drive** | Everything on that drive | Every entry on C: |

UFFS auto-detects the mode from the pattern you type — no flag needed.
Only a pattern that contains `\` or `/` looks at directory names; every
other pattern is matched against the **file name** alone.

A pattern cannot begin with `-`: it would be read as a flag.  To find
names that start with `-` or `--`, use `--begins-with` (its value may
start with a dash), or put a wildcard first:

```bash
uffs --begins-with --draft     # Names starting with "--draft"
uffs '*--draft*'               # Names containing "--draft"
```

## 2. Glob Patterns (Default)

Any pattern containing the wildcard characters `*` or `?` is treated as
a **glob**.  Globs are the most common and fastest search mode.

### Wildcards

| Token | Meaning | Example |
|-------|---------|---------|
| `*` | Zero or more characters | `*.rs` — all Rust source files |
| `?` | Exactly one character | `file?.txt` — `file1.txt`, `fileA.txt` |
| `a\|b` | Either pattern (OR) | `*.rs\|*.toml` — Rust sources and TOML files |
| `**` | One or more directory levels (path-aware patterns only, §5) | `src\**\*.rs` — `.rs` files in folders below a `src` directory |

Character classes such as `[abc]` or `[a-z]` are **not** glob syntax in
UFFS; the brackets are matched as ordinary characters.  Use a regex (§4)
when you need them.

### How Globs Are Run

UFFS picks the fastest way to run each pattern before matching.  You
never need to think about this, but it is why simple globs are so fast:

| Your Pattern | Internal Strategy | Why It Is Fast |
|-------------|-------------------|----------------|
| `*` | **Match-all** | Skips name matching entirely |
| `*.txt` | **Extension lookup** | Uses the extension index: visits only records with that extension |
| `foo*` (3+ characters before the `*`) | **Prefix search** | Index pre-filter, then a starts-with check |
| `readme` (no wildcards) | **Substring** | Index pre-filter, then a contains check on the name |
| `*bar`, `*needle*`, `foo*bar`, `*hallo*.txt` | **Wildcard match** | Index pre-filter on the longest plain run of text, then a wildcard matcher (no regex) |
| `*.rs\|*.toml` | **OR of globs** | Each alternative is tried on every name (no index pre-filter) |

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

# Rust and TOML files in one search
uffs '*.rs|*.toml'
```

### Best Practice

- Prefer simple globs (`*.ext`, `prefix*`) — they are the fastest.
- Add a drive prefix when you know the drive:  `d:/*.iso` is faster than
  `*.iso` across all drives.
- Use `**` only when you genuinely need to match across multiple directory
  levels in a path-aware search.

## 3. Literal Search

A pattern with **no wildcards and no `\` or `/`** is a **case-insensitive
substring match on the file name**.  This is the most natural search for
"I know part of the name".

```bash
# Finds any file or directory whose name contains "invoice"
# e.g.  D:\Clients\Acme\invoice_2026.pdf
#       C:\Users\me\Desktop\Invoice_Q1.docx
uffs invoice

# Case-insensitive by default — both of these find the same files
uffs readme
uffs README
```

> **Names, not paths.**  `uffs invoices` finds the *directory* named
> `invoices` itself, but not the files inside it.  To also get everything
> under each matching directory, use the `path:` prefix (§10); to filter
> by the folder a file lives in, use `--in-path` (§12).

### Best Practice

- Literal search is perfect for quick "I know a word in the filename"
  lookups.
- Combine with `--ext` or `--files-only` to narrow results fast.

## 4. Regex Search

Prefix your pattern with `>` to enter **regex mode**.  The regex is always
matched against the **file name** only.  To limit a regex to a folder or a
drive, add `--in-path` or `--drive`.

```bash
# Files ending in .log (regex anchored to end)
uffs '>.*\.log$'

# Files with a date stamp  YYYY-MM-DD  in the name
uffs '>[0-9]{4}-[0-9]{2}-[0-9]{2}'

# .config files under C:\Users
uffs '>\.config$' --drive C --in-path '*\users\*'

# Case-insensitive regex (default) — finds LOG, Log, log
uffs '>.*\.log$'
```

### Regex Tips

- The `>` prefix is **not** part of the regex — it is the mode selector.
- A drive letter or a path inside the regex is not understood as a scope:
  use `--drive` and `--in-path` instead.
- Use `--case` if you need case-sensitive regex matching.
- Write character classes out (`[0-9]`, `[a-z0-9_]`, `[ ]`) instead of the
  escapes `\d`, `\w`, `\s` and `\b`: in the default case-insensitive mode
  these escapes currently do not match what they should.  With `--case`
  they work as usual.
- Regex is usually the **slowest** mode: every name is tested.  One shape
  is fast: an extension list anchored at the end, such as
  `>.*\.(jpg|png|heic)$` or `>\.rs$`, is run as an extension lookup (the
  same as `--ext jpg,png,heic`).  Otherwise, combine the regex with
  `--ext` or other filters to shrink the work.

## 5. Path-Aware Patterns

A pattern is **path-aware** when it contains directory separators (`\` or
`/`, which are equivalent).  UFFS splits it into parts and walks the
directory tree:

- The first part matches the **name of a directory anywhere** on the
  drive (the pattern is not anchored at the drive root).
- Each following part matches a **direct child directory** by its exact
  name (or a glob).
- `**` stands for one or more directory levels.
- The last part matches the entries inside those directories, like a
  normal name pattern (a plain word is a substring match).

```bash
# .rs files directly inside any directory named "src" on C:
uffs 'c:/src/*.rs'

# .rs files anywhere below a "src" directory (one level down or deeper)
uffs 'c:/src/**/*.rs'

# Everything directly inside any directory named "backup", on any drive
uffs '/backup/*'
```

> A drive prefix with a **single** part after it (`c:/*.dll`,
> `C:\report`) is not path-aware: it is a name pattern on that drive.

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

The command line accepts `--word`, but it **currently has no effect**:
the search runs exactly as it would without it.

Where whole-word matching is available (the MCP server's search tool),
it means the **whole file name** must equal the pattern (or match the glob
as a whole) instead of merely containing it.  On the command line you get
the same result with an exact glob, or by anchoring a regex:

```bash
# Names that are exactly "test.txt" (any case)
uffs '>^test\.txt$'
```

## 8. Name-Only Matching (`--name-only`)

Patterns are matched against the **file name** by default, so
`--name-only` does not change which entries a name pattern finds.  What it
adds is a safety check: a pattern that contains `\` or `/` (which would
otherwise walk directories, §5) is rejected with an error.

```bash
# Same results with or without --name-only
uffs hallo
uffs hallo --name-only

# Error: a path pattern cannot be combined with --name-only
uffs 'projects\hallo' --name-only
```

## 9. Match-All Pattern (`*`)

The single wildcard `*` matches **every** file and directory.  It is the
starting point for pure-filter workflows where you don't care about the
name at all.

```bash
# All files larger than 1 GB
uffs '*' --min-size 1GB --files-only

# All empty directories (an empty directory counts 1: itself)
uffs '*' --dirs-only --max-descendants 1

# Everything modified in the last 24 hours
uffs '*' --newer 1d

# Everything on drive D:
uffs D:
```

## 10. Scope Prefixes

UFFS supports **Everything-compatible scope prefixes** that modify how
the pattern is applied.  Prefix the pattern with `path:`, `dir:`, or
`file:` to change the search scope:

| Prefix | Effect | Equivalent |
|--------|--------|------------|
| `path:` | Match names as usual, and also return **everything under** each matching directory | — |
| `dir:` | Only search **directories** | `--dirs-only` |
| `file:` | Only search **files** | `--files-only` |

### Examples

```bash
# Every entry whose name contains "projects", plus everything inside
# the directories among them
uffs 'path:projects'

# Directories whose name contains "build"
uffs 'dir:build'

# Files whose name starts with "README" (no directories)
uffs 'file:README*'
```

> The prefix is stripped before the search runs.  `path:` adds the
> contents of matching directories for plain name patterns (`report`,
> `*report*`); it has no extra effect on path-aware patterns (§5), on
> prefix patterns such as `report*`, or on regex.

## 11. Pattern Sugar Flags

These flags generate patterns from simple keywords, saving you from
writing glob syntax:

| Flag | Effect | Equivalent Pattern |
|------|--------|-------------------|
| `--begins-with <PREFIX>` | Names starting with PREFIX | `PREFIX*` |
| `--ends-with <SUFFIX>` | Names ending with SUFFIX | `*SUFFIX` |
| `--contains <NEEDLE>` | Names containing NEEDLE | `*NEEDLE*` |
| `--not-contains <NEEDLE>` | Exclude names containing NEEDLE | `--exclude '*NEEDLE*'` |

### Examples

```bash
# All names starting with "report"
uffs --begins-with report

# All names ending with "_backup"
uffs --ends-with _backup

# All names containing "invoice"
uffs --contains invoice

# All .log files, but exclude those containing "debug"
uffs '*.log' --not-contains debug
```

> Only one pattern is used.  If you give several, the first in this order
> wins and the others are ignored: the pattern itself, `--begins-with`,
> `--ends-with`, `--contains`.  `--not-contains` combines with any of
> them, but use it **instead of** `--exclude`, not together with it: the
> two currently do not combine correctly.

## 12. Path Directory Filter

The `--in-path` flag restricts results to entries whose **directory path**
matches a glob — independent of the search pattern.  This is different
from `path:` (which adds the contents of matching directories).

```bash
# .rs files only under directories containing "projects"
uffs '*.rs' --in-path '*projects*'

# DLLs only under System32
uffs '*.dll' --in-path '*windows\system32*'

# Logs, but not from any folder containing "cache" or "temp"
uffs '*.log' --not-in-path '*cache*,*temp*'
```

> **`path:` vs `--in-path`:** `path:report` finds entries whose *name*
> contains "report", plus everything inside the directories among them.
> `--in-path '*report*'` keeps entries whose *folder path* contains
> "report" — the file name is not part of that match.  `--not-in-path`
> takes a comma-separated list and drops entries whose folder path matches
> any of them.  See [Filters §8](filters.md#8-path-filter) for details.

## 13. Drive Scoping

You can scope your search to specific drives using three methods:

### In-Pattern Drive Prefix

```bash
# Search only C: drive
uffs 'c:/*.dll'

# Everything on C: (a bare drive, or its root, matches all)
uffs C:
uffs 'C:\'
```

### `--drive` Flag (Single Drive)

```bash
uffs '*.exe' --drive C
uffs '*.exe' -d C
```

### `--drives` Flag (Multiple Drives)

```bash
# Search C:, D: and E: concurrently
uffs '*.pdf' --drives C,D,E
```

Drive letters are case-insensitive.  You may include the colon
(`C:` or `C`) — both work.  An explicit `--drive` or `--drives` wins over
a drive prefix in the pattern.

## 14. Performance Mental Model

Not all searches are equal.  Here is a rough performance ranking from
fastest to slowest:

| Rank | Pattern Style | Why |
|------|---------------|-----|
| 1 | `*.ext` (extension glob) | Extension index: visits only matching records |
| 2 | `prefix*` (3+ characters) | Index pre-filter, then a starts-with check |
| 3 | `readme` (plain word, 3+ characters) | Index pre-filter, then a substring check |
| 4 | Other globs (`*suffix`, `*foo*.bar`) | Index pre-filter on the longest plain run, then a wildcard match |
| 5 | OR globs (`*.a\|*.b`) and very short words | Every name is checked |
| 6 | `>regex` | Every name is run through the regex engine (except extension-list shapes, §4) |

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
  *.rs|*.toml          Glob OR — either pattern
  report               Literal — names containing "report"
  >.*\.log$            Regex — names ending in .log
  c:/projects/*        Path-aware glob — entries inside directories named "projects" on C:
  *                    Match-all — every file and directory
  C:                   Match-all on one drive
  --begins-with --x    Names starting with a dash (a pattern cannot start with -)

SCOPE PREFIXES (Everything-compatible)
  path:report          Names containing "report", plus everything inside those directories
  dir:build            Only directories whose name contains "build"
  file:README*         Only files whose name starts with "README"

MODIFIERS
  --case               Case-sensitive matching
  --smart-case         Auto case-sensitive if pattern has uppercase
  --word               Accepted, currently has no effect
  --name-only          Reject patterns that contain \ or /

PATTERN SUGAR
  --begins-with PREFIX Sugar for 'PREFIX*'
  --ends-with SUFFIX   Sugar for '*SUFFIX'
  --contains NEEDLE    Sugar for '*NEEDLE*'
  --not-contains NEEDLE Sugar for --exclude '*NEEDLE*'

PATH FILTER
  --in-path <GLOB>     Filter by directory path (not filename)
  --not-in-path <GLOBS> Drop entries whose directory path matches any glob

DRIVE SCOPING
  --drive C, -d C      Single drive
  --drives C,D,E       Multiple drives
  c:/*.ext             Drive prefix in pattern
```

## See also

- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Filters](filters.md): narrow results by size, date, type, attributes and more
- [Sorting](sorting.md): sort columns and multi-tier sorts
- [Output Formats](output-formats.md): CSV, JSON and table output, and choosing columns
