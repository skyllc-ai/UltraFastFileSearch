#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.
"""check_doc_links.py: every relative link in the repository's Markdown works.

For each tracked Markdown file, every relative link (inline `[text](target)`,
image, reference definition, HTML `href` / `src`) must point at a file or
directory that is tracked by git: a missing target, or one in a gitignored or
untracked place, is an error.  A link with an anchor (`file.md#anchor`, or
`#anchor` within the same file) must name a heading of the target Markdown
file, by the id GitHub gives it (github-slugger: lower case; every character
that is not a letter, mark, digit, connector punctuation, space or hyphen
dropped; each space a hyphen, runs not collapsed; a repeated id gets -1, -2,
...), or an explicit `id=` / `name=` anchor in its HTML.

External links (any `scheme:`, `//host`) are not checked: no network.

Usage:
    python3 scripts/ci/check_doc_links.py              # check the repository
    python3 scripts/ci/check_doc_links.py --self-test  # its own cases, then the repository

Exit status: 0 when every link works, 1 when any does not (each is printed as
`path:line: message`), 2 when the repository cannot be read.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import unicodedata
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import unquote

MARKDOWN_SUFFIXES = (".md", ".markdown")
# Any indentation: a fence inside a list item is indented past three spaces.
FENCE = re.compile(r"^[ \t]*(`{3,}|~{3,})")
ATX = re.compile(r"^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$")
SETEXT = re.compile(r"^ {0,3}(=+|-+)[ \t]*$")
REFERENCE_DEF = re.compile(r"^ {0,3}\[[^\]]+\]:[ \t]*(<[^>]*>|\S+)")
HTML_TARGET = re.compile(r"""\b(?:href|src)\s*=\s*(?:"([^"]*)"|'([^']*)')""", re.IGNORECASE)
HTML_ANCHOR = re.compile(r"""\b(?:id|name)\s*=\s*(?:"([^"]*)"|'([^']*)')""", re.IGNORECASE)
SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")
LIST_ITEM = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s")
CODE_SPAN = re.compile(r"(`+)(.+?)\1", re.DOTALL)
INLINE_LINK_TEXT = re.compile(r"!?\[([^\]]*)\](?:\([^)]*\)|\[[^\]]*\])")
HTML_TAG = re.compile(r"<[^>]+>")


@dataclass(frozen=True)
class Finding:
    """One link that does not work."""

    path: str
    line: int
    message: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.message}"


def git(root: Path, *args: str) -> str:
    """Run git in `root` and return its standard output."""
    return subprocess.run(
        ["git", *args], cwd=root, check=True, capture_output=True, text=True
    ).stdout


def prose_lines(text: str, blank_code: bool = True) -> list[tuple[int, str]]:
    """The lines of `text` outside fenced code blocks and HTML comments,
    with their 1-based numbers.  Code spans are blanked (unless
    `blank_code` is false) so a link written as code is not a link."""
    out: list[tuple[int, str]] = []
    fence: str | None = None
    in_comment = False
    for number, raw in enumerate(text.splitlines(), start=1):
        if fence is not None:
            stripped = raw.strip()
            if stripped.startswith(fence[0] * len(fence)) and set(stripped) <= {fence[0]}:
                fence = None
            continue
        match = FENCE.match(raw)
        if match and not in_comment:
            fence = match.group(1)
            continue
        line = raw
        if in_comment:
            end = line.find("-->")
            if end < 0:
                continue
            line = line[end + 3 :]
            in_comment = False
        while "<!--" in line:
            start = line.index("<!--")
            end = line.find("-->", start + 4)
            if end < 0:
                line = line[:start]
                in_comment = True
                break
            line = line[:start] + line[end + 3 :]
        if blank_code:
            line = CODE_SPAN.sub(lambda m: " " * len(m.group(0)), line)
        out.append((number, line))
    return out


def link_destinations(line: str) -> list[str]:
    """Every link destination on one prose line."""
    found: list[str] = []
    pos = 0
    while True:
        start = line.find("](", pos)
        if start < 0:
            break
        dest, pos = destination_at(line, start + 2)
        if dest is not None:
            found.append(dest)
    ref = REFERENCE_DEF.match(line)
    if ref:
        found.append(ref.group(1))
    for match in HTML_TARGET.finditer(line):
        found.append(match.group(1) if match.group(1) is not None else match.group(2))
    return found


def destination_at(line: str, index: int) -> tuple[str | None, int]:
    """Parse the destination of an inline link that starts at `index`
    (just after `](`).  Returns it (without a title) and where to resume."""
    rest = line[index:].lstrip()
    offset = len(line) - len(rest)
    if rest.startswith("<"):
        end = rest.find(">")
        if end < 0:
            return None, offset + 1
        return rest[1:end], offset + end + 1
    depth = 0
    for at, char in enumerate(rest):
        if char == "(":
            depth += 1
        elif char == ")":
            if depth == 0:
                return rest[:at], offset + at + 1
            depth -= 1
        elif char.isspace() and depth == 0:
            return rest[:at], offset + at + 1
    return None, len(line)


def heading_text(raw: str) -> str:
    """The text GitHub renders for a heading's source (links, images, code
    and HTML reduced to their text; backslash escapes resolved)."""
    text = CODE_SPAN.sub(lambda m: m.group(2), raw)
    text = INLINE_LINK_TEXT.sub(lambda m: m.group(1), text)
    text = HTML_TAG.sub("", text)
    return re.sub(r"\\(.)", r"\1", text)


def slug_base(text: str) -> str:
    """github-slugger's slug for one heading text (before de-duplication)."""
    kept = []
    for char in text.lower():
        category = unicodedata.category(char)
        if category[0] in "LMN" or category == "Pc" or char in " -":
            kept.append(char)
    return "".join(kept).replace(" ", "-")


def heading_ids(text: str) -> set[str]:
    """Every anchor id a Markdown file defines: its headings, slugged and
    de-duplicated the way github-slugger does, plus explicit HTML ids."""
    occurrences: dict[str, int] = {}
    ids: set[str] = set()

    def add(raw: str) -> None:
        base = slug_base(heading_text(raw.strip()))
        slug = base
        if slug in occurrences:
            while True:
                occurrences[base] += 1
                slug = f"{base}-{occurrences[base]}"
                if slug not in occurrences:
                    break
        occurrences[slug] = 0
        ids.add(slug)

    previous = ""
    for _, line in prose_lines(text, blank_code=False):
        atx = ATX.match(line)
        if atx:
            add(atx.group(2) or "")
            previous = ""
        elif SETEXT.match(line) and is_paragraph_line(previous):
            add(previous)
            previous = ""
        else:
            previous = line
        for match in HTML_ANCHOR.finditer(line):
            ids.add(match.group(1) if match.group(1) is not None else match.group(2))
    return ids


def is_paragraph_line(line: str) -> bool:
    """Whether `line` can be the text of a setext heading."""
    stripped = line.strip()
    return bool(stripped) and not (
        stripped.startswith(("#", ">", "<", "|"))
        or LIST_ITEM.match(line)
        or "|" in stripped
        or line.startswith("    ")
    )


class Repository:
    """The tracked files of a git work tree, and the anchors of its
    Markdown files (read once each)."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.files = set(git(root, "ls-files", "-z").split("\0")) - {""}
        self.dirs = {str(Path(f).parent) for f in self.files}
        for directory in list(self.dirs):
            parent = Path(directory)
            while str(parent) not in ("", "."):
                self.dirs.add(str(parent))
                parent = parent.parent
        self._ids: dict[str, set[str]] = {}

    def markdown_files(self) -> list[str]:
        return sorted(f for f in self.files if f.lower().endswith(MARKDOWN_SUFFIXES))

    def ids(self, path: str) -> set[str]:
        if path not in self._ids:
            text = (self.root / path).read_text(encoding="utf-8")
            self._ids[path] = heading_ids(text)
        return self._ids[path]

    def why_not_tracked(self, path: str) -> str:
        on_disk = (self.root / path).exists()
        if not on_disk:
            return "does not exist"
        ignored = subprocess.run(
            ["git", "check-ignore", "-q", "--", path], cwd=self.root, check=False
        ).returncode == 0
        return "is gitignored" if ignored else "is not tracked by git"


def check_link(repo: Repository, source: str, target: str) -> str | None:
    """Why the link `target` in `source` does not work, or `None`."""
    target = target.strip()
    if not target or SCHEME.match(target) or target.startswith("//"):
        return None
    path_part, _, anchor = target.partition("#")
    path_part = unquote(path_part.split("?", 1)[0])
    if path_part:
        base = Path() if path_part.startswith("/") else Path(source).parent
        joined = os.path.normpath(str(base / path_part.lstrip("/")))
        if joined == ".." or joined.startswith("../"):
            return f"`{target}` points outside the repository"
        resolved = "" if joined == "." else joined
    else:
        resolved = source
    if resolved and resolved not in repo.files and resolved not in repo.dirs:
        return f"`{target}`: {resolved} {repo.why_not_tracked(resolved)}"
    if anchor and resolved.lower().endswith(MARKDOWN_SUFFIXES):
        wanted = unquote(anchor)
        if wanted not in repo.ids(resolved):
            return f"`{target}`: {resolved} has no heading with the id `{wanted}`"
    return None


def check_repository(root: Path) -> list[Finding]:
    """Every broken relative link in the tracked Markdown of `root`."""
    repo = Repository(root)
    findings: list[Finding] = []
    for source in repo.markdown_files():
        text = (root / source).read_text(encoding="utf-8")
        for number, line in prose_lines(text):
            for target in link_destinations(line):
                problem = check_link(repo, source, target)
                if problem:
                    findings.append(Finding(source, number, problem))
    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the checker's own test cases before checking the repository",
    )
    args = parser.parse_args()
    if args.self_test:
        import unittest

        sys.path.insert(0, str(Path(__file__).resolve().parent))
        suite = unittest.defaultTestLoader.loadTestsFromName("check_doc_links_test")
        result = unittest.TextTestRunner(stream=sys.stderr, verbosity=0).run(suite)
        if not result.wasSuccessful():
            return 1
    try:
        root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").strip())
        findings = check_repository(root)
    except (subprocess.CalledProcessError, OSError, UnicodeDecodeError) as err:
        print(f"doc-links: cannot read the repository: {err}", file=sys.stderr)
        return 2
    for finding in findings:
        print(finding)
    if findings:
        print(f"doc-links: {len(findings)} broken link(s)", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
