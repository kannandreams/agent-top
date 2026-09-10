#!/usr/bin/env python3
"""Run cargo-audit and record what it found in docs/data/security.json.

The site shows a vulnerability count, so that count has to come from a real
scan rather than a claim someone typed. This runs the scan, writes the parts
worth publishing, and in --check mode fails when the committed figures no
longer match what a fresh scan says. CI runs --check, so the number on
agenttop.dev cannot quietly go stale while still being presented as current.

    python3 scripts/security_report.py            # rewrite the data file
    python3 scripts/security_report.py --check    # verify it, change nothing

Standard library only, so it needs no environment of its own.
"""

import json
import subprocess
import sys
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "docs" / "data" / "security.json"


def scan() -> dict:
    """cargo-audit's report, reduced to the fields the site publishes."""
    proc = subprocess.run(
        ["cargo", "audit", "--json"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    # cargo-audit exits non-zero when it finds something, which is a result,
    # not a failure. Unparseable output is the real failure.
    try:
        report = json.loads(proc.stdout)
    except json.JSONDecodeError:
        sys.exit(f"cargo audit produced no JSON report (exit {proc.returncode}):\n{proc.stderr.strip()}")

    db = report.get("database", {})
    warnings = report.get("warnings") or {}
    # `cargo audit --version` prints "cargo-audit-audit <version>": the binary
    # name with the subcommand appended. Keep the version, name the tool once.
    raw = subprocess.run(
        ["cargo", "audit", "--version"], cwd=ROOT, capture_output=True, text=True
    ).stdout.split()
    version = f"cargo-audit {raw[-1]}" if raw else "cargo-audit"

    return {
        "vulnerabilities": report["vulnerabilities"]["count"],
        "warnings": sum(len(v) for v in warnings.values()),
        "dependencies": report.get("lockfile", {}).get("dependency-count", 0),
        "advisory_db": {
            "advisories": db.get("advisory-count", 0),
            "commit": (db.get("last-commit") or "")[:12],
            "updated": (db.get("last-updated") or "")[:10],
        },
        "checked_at": date.today().isoformat(),
        "tool": version or "cargo-audit",
    }


def main() -> None:
    checking = "--check" in sys.argv[1:]
    fresh = scan()

    if not checking:
        DATA.parent.mkdir(parents=True, exist_ok=True)
        DATA.write_text(json.dumps(fresh, indent=2) + "\n")
        print(f"{DATA.relative_to(ROOT)}: {fresh['vulnerabilities']} vulnerabilities, "
              f"{fresh['warnings']} warnings, {fresh['dependencies']} dependencies")
        return

    if not DATA.is_file():
        sys.exit(f"{DATA.relative_to(ROOT)} does not exist; run scripts/security_report.py")

    published = json.loads(DATA.read_text())
    # Only the security claim itself is enforced. The dependency count and the
    # advisory-database revision move with every routine change and are
    # refreshed by the next run; letting them fail CI would turn an honest
    # number into a chore.
    for field in ("vulnerabilities", "warnings"):
        if published.get(field) != fresh[field]:
            sys.exit(
                f"{DATA.relative_to(ROOT)} publishes {field}={published.get(field)}, "
                f"a fresh scan says {fresh[field]}. Run scripts/security_report.py."
            )
    print(f"published figures match a fresh scan: {fresh['vulnerabilities']} vulnerabilities, "
          f"{fresh['warnings']} warnings")


if __name__ == "__main__":
    main()
