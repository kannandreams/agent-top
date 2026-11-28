"""MkDocs hook: the newest blog posts, for the home page's blog cards.

The blog plugin builds its own pages, but a template outside the blog cannot
ask it for the post list. This reads docs/blog/posts/*.md directly: the date,
slug and description from each post's front matter, the title from its first
heading, and the first image in it for the card. The result is
config.extra.recent_posts, newest first, which docs/overrides/home.html draws.
"""

import posixpath
import re
from pathlib import Path

import yaml

COUNT = 3


def on_config(config):
    docs = Path(config["docs_dir"])
    posts = []
    for path in sorted((docs / "blog" / "posts").glob("*.md")):
        text = path.read_text(encoding="utf-8")
        m = re.match(r"---\n(.*?)\n---\n(.*)", text, re.S)
        if not m:
            continue
        meta = yaml.safe_load(m.group(1)) or {}
        body = m.group(2)
        title = re.search(r"^# (.+)$", body, re.M)
        image = re.search(r"!\[([^\]]*)\]\(([^)\s]+)", body)
        image_url = None
        if image:
            # Post images are relative to the post's source file.
            rel = posixpath.normpath(posixpath.join("blog/posts", image.group(2)))
            if (docs / rel).is_file():
                image_url = rel
        posts.append(
            {
                "date": meta["date"],
                "title": title.group(1).strip() if title else meta["slug"],
                "description": meta.get("description", ""),
                "url": f"blog/{meta['slug']}/",
                "image": image_url,
                "alt": image.group(1) if image else "",
            }
        )
    posts.sort(key=lambda p: p["date"], reverse=True)
    config["extra"]["recent_posts"] = posts[:COUNT]
    return config
