"""MkDocs hook: include the repository CHANGELOG as a docs page.

docs/changelog.md holds a `<!-- changelog -->` marker. This hook replaces it
with the contents of CHANGELOG.md at the repository root, dropping the `docs/`
prefix from its links, which are written relative to the repository root and
would otherwise be reported as missing in strict mode.
"""

from pathlib import Path

MARKER = "<!-- changelog -->"


def on_page_markdown(markdown, page, config, files):
    if page.file.src_uri != "changelog.md" or MARKER not in markdown:
        return markdown
    root = Path(config["config_file_path"]).resolve().parent
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    # The page supplies its own title; the file's H1 would repeat it.
    if changelog.startswith("# "):
        changelog = changelog.split("\n", 1)[1]
    return markdown.replace(MARKER, changelog.replace("](docs/", "]("))
