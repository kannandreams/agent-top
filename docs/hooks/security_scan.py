"""MkDocs hook: publish the vulnerability count from a real scan.

The site claims a number of known vulnerabilities, so the number has to come
from somewhere checkable. `scripts/security_report.py` runs cargo-audit and
writes docs/data/security.json; this reads that file and puts the figures in
two places at build time:

  * the "Security scan" item at the bottom of the sidebar, whose title becomes
    the count itself rather than a static label;
  * the <!--security-summary--> marker on the Security page, which becomes a
    panel with the full provenance: how many dependencies were scanned,
    against which revision of the RustSec advisory database, by which tool,
    on which day.

Nothing here runs cargo or opens a socket: it reads one committed JSON file,
so a docs build needs no Rust toolchain. When the file is missing the sidebar
keeps its plain label and the panel says so, rather than reporting a zero
nobody measured.
"""

import json
import logging
from pathlib import Path

log = logging.getLogger("mkdocs.hooks.security_scan")

# The nav label in mkdocs.yml that this hook rewrites, and the marker it
# replaces in the page body.
NAV_LABEL = "Security scan"
MARKER = "<!--security-summary-->"

_data: dict | None = None


def on_config(config):
    global _data
    path = Path(config["docs_dir"]) / "data" / "security.json"
    try:
        _data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        _data = None
        log.info("security_scan: no scan data (%s); the count will not be shown", e)
    return config


def _phrase(n: int) -> str:
    return "1 vulnerability" if n == 1 else f"{n} vulnerabilities"


def on_nav(nav, config, files):
    """Rewrite the sidebar label to the count a scan actually found.

    "advisories" rather than "vulnerabilities": it is what the RustSec
    database calls its entries, and it is short enough to stay on one line in
    the sidebar, which "0 vulnerabilities" is not.
    """
    if not _data:
        return nav
    n = _data["vulnerabilities"]
    for item in nav.items:
        if item.title == NAV_LABEL:
            item.title = f"Security · {n} advisor{'y' if n == 1 else 'ies'}"
            # The stylesheet colours this link green, and it cannot read the
            # count to know whether green is the truth. So the state goes into
            # the target instead: a clean scan keeps the plain page URL, a
            # scan with findings points at the panel, which the stylesheet
            # matches separately and colours as a warning.
            if n:
                item.url = "/security/#what-the-scans-say"
    return nav


def on_page_markdown(markdown, page, config, files):
    if MARKER not in markdown:
        return markdown
    if not _data:
        return markdown.replace(
            MARKER,
            "No scan is recorded in this build. Run `mise run security:report` to produce one.",
        )

    count = _data["vulnerabilities"]
    db = _data.get("advisory_db", {})
    # The class carries the colour, and it is chosen here rather than in CSS
    # because only this side knows the count: a green panel over a non-zero
    # number would be worse than no panel at all.
    state = "clean" if count == 0 and _data.get("warnings", 0) == 0 else "flagged"
    warnings = _data.get("warnings", 0)
    warning_note = (
        "" if warnings == 0 else f" {warnings} advisory warning(s) (unmaintained or yanked crates) were also reported."
    )

    panel = f"""<div class="at-scan at-scan--{state}">
<span class="at-scan__count">{count}</span>
<span class="at-scan__label">known vulnerabilities</span>
<p class="at-scan__detail">{_data.get('dependencies', 0)} dependencies scanned against {db.get('advisories', 0):,} RustSec advisories (database <code>{db.get('commit', '?')}</code>, {db.get('updated', '?')}) by {_data.get('tool', 'cargo-audit')} on {_data.get('checked_at', '?')}.{warning_note} The same figures are published as <a href="../data/security.json">JSON</a>.</p>
</div>"""
    return markdown.replace(MARKER, panel)
