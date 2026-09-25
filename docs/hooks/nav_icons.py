"""MkDocs hook: an icon on each top-level sidebar entry.

Material draws `meta.icon` in front of any nav entry's title, but only a page
can set it, from its front matter. A section heading ("Cost") or an external
link ("GitHub") has no front matter. So the icons are listed by title under
`extra.nav_icons` in mkdocs.yml, and this hook sets `meta.icon` on the matching
top-level entry, whatever kind it is. The names are Material's bundled icon
paths, such as `octicons/home-16`.

Sections and links get theirs in `on_nav`. A page's `meta` is replaced when its
front matter is read, which happens after `on_nav`, so a page gets its icon in
`on_page_markdown` instead; an `icon` in the page's own front matter wins.

Listed before security_scan.py in mkdocs.yml: that hook renames the
"Security scan" entry in `on_nav`, and this one matches on the name as written.
"""

import logging

from mkdocs.structure.pages import Page

log = logging.getLogger("mkdocs.hooks.nav_icons")

_pages: dict[str, str] = {}


def on_nav(nav, config, files):
    icons = config["extra"].get("nav_icons") or {}
    _pages.clear()
    unused = set(icons)
    for item in nav.items:
        icon = icons.get(item.title)
        if not icon:
            continue
        unused.discard(item.title)
        if isinstance(item, Page):
            _pages[item.file.src_uri] = icon
            continue
        item.meta = {**(getattr(item, "meta", None) or {}), "icon": icon}
        # With navigation.indexes, a section with an index page (Blogs) is
        # drawn from that page's meta, not the section's.
        for child in getattr(item, "children", None) or []:
            if isinstance(child, Page) and child.is_index:
                _pages[child.file.src_uri] = icon
                break
    for title in sorted(unused):
        log.warning("nav_icons: no top-level nav entry is titled %r", title)
    return nav


def on_page_markdown(markdown, page, config, files):
    icon = _pages.get(page.file.src_uri)
    if icon:
        page.meta.setdefault("icon", icon)
    return markdown
