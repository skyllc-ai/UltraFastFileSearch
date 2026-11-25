# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.
"""Test cases for check_doc_links.py, run by `check_doc_links.py --self-test`
(the `doc-links` gate) or directly with `python3 scripts/ci/check_doc_links_test.py`.

Each case builds a small git repository in a temporary directory and checks it.
"""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from check_doc_links import check_repository, heading_ids, slug_base


def repository(files: dict[str, str], ignore: str = "") -> tempfile.TemporaryDirectory:
    """A temporary git repository holding `files` (all tracked), with
    `ignore` as its `.gitignore`."""
    tmp = tempfile.TemporaryDirectory()
    root = Path(tmp.name)
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    if ignore:
        (root / ".gitignore").write_text(ignore, encoding="utf-8")
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    subprocess.run(["git", "add", "-A"], cwd=root, check=True)
    return tmp


def messages(files: dict[str, str], ignore: str = "", untracked: dict[str, str] | None = None) -> list[str]:
    """The findings for a repository of `files`, as `path:line: message`."""
    with repository(files, ignore) as tmp:
        for name, text in (untracked or {}).items():
            path = Path(tmp) / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return [str(finding) for finding in check_repository(Path(tmp))]


class DocLinkCases(unittest.TestCase):
    def test_a_good_link_and_anchor_pass(self) -> None:
        found = messages(
            {
                "README.md": "See [the guide](docs/guide.md#getting-started) and [here](#top).\n\n# Top\n",
                "docs/guide.md": "# Guide\n\n## Getting Started\n",
            }
        )
        self.assertEqual(found, [])

    def test_a_broken_file_link_is_found(self) -> None:
        found = messages({"README.md": "Read [this](docs/missing.md).\n"})
        self.assertEqual(len(found), 1)
        self.assertIn("README.md:1:", found[0])
        self.assertIn("does not exist", found[0])

    def test_a_link_into_a_gitignored_folder_is_found(self) -> None:
        found = messages(
            {"README.md": "Notes in [private](private/notes.md).\n"},
            ignore="private/\n",
            untracked={"private/notes.md": "# Notes\n"},
        )
        self.assertEqual(len(found), 1)
        self.assertIn("is gitignored", found[0])

    def test_an_untracked_target_is_found(self) -> None:
        found = messages(
            {"README.md": "[draft](draft.md)\n"}, untracked={"draft.md": "# Draft\n"}
        )
        self.assertEqual(len(found), 1)
        self.assertIn("is not tracked by git", found[0])

    def test_a_broken_anchor_is_found(self) -> None:
        found = messages(
            {
                "README.md": "[setup](guide.md#set-up)\n",
                "guide.md": "# Guide\n\n## Setup\n",
            }
        )
        self.assertEqual(len(found), 1)
        self.assertIn("no heading with the id `set-up`", found[0])

    def test_a_repeated_heading_gets_numbered_ids(self) -> None:
        found = messages(
            {
                "README.md": "[a](#notes) [b](#notes-1) [c](#notes-2) [d](#notes-3)\n\n"
                "## Notes\n\n## Notes\n\nText\n\nNotes\n-----\n",
            }
        )
        self.assertEqual(len(found), 1, found)
        self.assertIn("`notes-3`", found[0])

    def test_external_links_and_code_are_not_checked(self) -> None:
        found = messages(
            {
                "README.md": "[web](https://example.com/x.md) [mail](mailto:a@b.c)\n"
                "`[not a link](missing.md)`\n\n```\n[also not](missing.md)\n```\n"
                "<!-- [hidden](missing.md) -->\n",
            }
        )
        self.assertEqual(found, [])

    def test_directories_and_reference_definitions(self) -> None:
        found = messages(
            {
                "README.md": "[src](src/) and [ref][r]\n\n[r]: docs/gone.md\n",
                "src/lib.rs": "",
            }
        )
        self.assertEqual(len(found), 1)
        self.assertIn("docs/gone.md", found[0])


class SlugCases(unittest.TestCase):
    def test_github_slugger_rules(self) -> None:
        self.assertEqual(slug_base("7. Whole-Word Matching (`--word`)"), "7-whole-word-matching---word")
        self.assertEqual(slug_base("Foo  Bar"), "foo--bar")
        self.assertEqual(slug_base("snake_case & Ünïcode"), "snake_case--ünïcode")
        self.assertEqual(slug_base("Emoji 🚀 here"), "emoji--here")

    def test_heading_text_drops_markup(self) -> None:
        ids = heading_ids("## The [`uffs`](x.md) *CLI*\n\n<a id=\"custom\"></a>\n")
        self.assertIn("the-uffs-cli", ids)
        self.assertIn("custom", ids)


if __name__ == "__main__":
    unittest.main()
