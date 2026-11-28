"""MkDocs hook: the repository's star count at build time.

The header's GitHub button shows the star count. The page asks the GitHub API
for the current number, but that API allows 60 requests an hour per address
without a token, and a reader over the limit would see no number. This fetches
the count once per build and puts it in config.extra.repo_stars, so the button
always has a number to show; the page then replaces it with the live one when
it can. A failed fetch leaves the value unset and the build carries on.
"""

import json
import os
import re
import urllib.request


def on_config(config):
    m = re.search(r"github\.com/([^/]+/[^/]+?)/?$", config.get("repo_url") or "")
    if not m:
        return config
    req = urllib.request.Request(
        f"https://api.github.com/repos/{m.group(1)}",
        headers={"Accept": "application/vnd.github+json", "User-Agent": "agent-top-docs-build"},
    )
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            stars = json.load(r).get("stargazers_count")
    except (OSError, ValueError):
        return config
    if isinstance(stars, int):
        config["extra"]["repo_stars"] = stars
    return config
