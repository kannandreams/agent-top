"""MkDocs hook: content-hash extra_css so a change is never served stale.

mkdocs writes extra_css entries to <link href="..."> verbatim, with no cache
key of their own. A browser or edge cache that already has the old file at
that exact URL can keep serving it for as long as its cache-control header
allows, even after the file on disk changes, because the URL never changed.

This appends "?v=<hash of the file's own bytes>" to every local (non-URL)
extra_css entry at build time, so editing the stylesheet always produces a
new URL and there is nothing to invalidate by hand.
"""

import hashlib
from pathlib import Path


def on_config(config):
    root = Path(config["config_file_path"]).resolve().parent
    docs_dir = Path(config["docs_dir"])
    busted = []
    for href in config["extra_css"]:
        if "://" in href:
            busted.append(href)
            continue
        path = (docs_dir / href).resolve()
        if not path.is_file():
            busted.append(href)
            continue
        digest = hashlib.sha256(path.read_bytes()).hexdigest()[:10]
        busted.append(f"{href}?v={digest}")
    config["extra_css"] = busted
    return config
