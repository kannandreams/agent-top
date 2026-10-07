"""Screenshots of `agent-top ui` for the docs, from a made-up store.

Never point the web view at a real store for a public image: it shows project
names, working directories and session ids. This script builds a store from
nothing, with invented projects and sessions dated over the last 30 days,
starts `agent-top ui` on it and saves the images under docs/screenshots/.

    cargo build --release
    uv run --with playwright python docs/ui-demo.py

Change what the images show by editing the tables below. The random numbers
come from a fixed seed, so a rerun differs only in the dates.
"""

import os
import random
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = os.environ.get("AGENT_TOP_BIN", str(ROOT / "target/release/agent-top"))
OUT = ROOT / "docs/screenshots"
DB_DIR = Path("/tmp/agent-top-demo")

# USD per million tokens: input, output, cache read, cache write. The same
# rows as the built-in table, so a session's cost matches its tokens.
PRICES = {
    "claude-opus-5-5": (4.00, 20.00, 0.20, 5.00),
    "claude-sonnet-5": (2.00, 10.00, 0.20, 2.50),
    "gpt-5.6-sol": (4.00, 20.00, 0.40, 4.00),
    "gpt-5.6-luna": (0.20, 1.20, 0.02, 0.20),
    "deepseek-v4-flash": (0.30, 1.20, 0.006, 0.30),
}
HARNESSES = {
    "claude": {"models": ["claude-opus-5-5", "claude-sonnet-5"], "version": "2.1.291", "weight": 5,
               "tools": ["Bash", "Read", "Edit", "Grep", "Write", "WebFetch", "mcp__github__get_issue", "mcp__docs__search"]},
    "codex": {"models": ["gpt-5.6-sol", "gpt-5.6-luna"], "version": "0.154.0", "weight": 3,
              "tools": ["exec_command", "apply_patch", "web_search", "exec_command"]},
    "opencode": {"models": ["deepseek-v4-flash"], "version": "1.18.15", "weight": 1,
                 "tools": ["bash", "read", "edit", "grep"]},
}
PROJECTS = ["acme/api", "acme/web", "acme/infra", "tools/cli", "lab/evals", "lab/notebooks"]
# Typical durations in ms for a tool call, before the random spread.
TOOL_MS = {"Bash": 2500, "exec_command": 1800, "bash": 2000, "Read": 30, "read": 25, "Edit": 45, "edit": 40,
           "Grep": 120, "grep": 110, "Write": 60, "apply_patch": 90, "WebFetch": 6000, "web_search": 7000,
           "mcp__github__get_issue": 900, "mcp__docs__search": 1400}


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def cost(model, inp, out, cr, cw):
    p = PRICES[model]
    return (inp * p[0] + out * p[1] + cr * p[2] + cw * p[3]) / 1e6


def seed(db):
    rnd = random.Random(7)
    now = int(time.time() * 1000)
    day = 86_400_000
    con = sqlite3.connect(db)
    n = 0
    for d in range(30, -1, -1):
        for _ in range(rnd.choice([0, 1, 2, 2, 3, 3, 4])):
            n += 1
            harness = rnd.choices(list(HARNESSES), weights=[h["weight"] for h in HARNESSES.values()])[0]
            h = HARNESSES[harness]
            model = rnd.choice(h["models"])
            project = rnd.choice(PROJECTS)
            sid = f"{n:08x}-demo-4000-8000-{rnd.getrandbits(48):012x}"
            t = now - d * day - rnd.randint(1, 20) * 3_600_000
            turns = rnd.randint(2, 18)
            spans, seq = [], 0
            for _turn in range(turns):
                turn_seq, turn_start = seq, t
                spans.append([seq, "turn", "turn", t, 0, 0, None])
                seq += 1
                for _step in range(rnd.randint(1, 7)):
                    inf = int(rnd.lognormvariate(8.3, 0.6))
                    spans.append([seq, "inference", "inference", t, inf, 0, turn_seq])
                    seq += 1
                    t += inf
                    tool = rnd.choice(h["tools"])
                    ms = int(TOOL_MS[tool] * rnd.lognormvariate(0, 0.7))
                    spans.append([seq, "tool", tool, t, ms, int(rnd.random() < 0.04), turn_seq])
                    seq += 1
                    t += ms + rnd.randint(5, 60)
                inf = int(rnd.lognormvariate(8.0, 0.5))
                spans.append([seq, "inference", "inference", t, inf, 0, turn_seq])
                seq += 1
                t += inf
                spans[turn_seq][4] = t - turn_start
                t += rnd.randint(20_000, 600_000)
            tools = [s for s in spans if s[1] == "tool"]
            inp = turns * rnd.randint(800, 3_000)
            cr = turns * rnd.randint(150_000, 900_000)
            cw = turns * rnd.randint(8_000, 40_000)
            out = turns * rnd.randint(1_500, 6_000)
            c = cost(model, inp, out, cr, cw)
            started, last = spans[0][3], max(s[3] + s[4] for s in spans)
            con.execute(
                """INSERT INTO sessions (harness, session_id, source_path, cwd, project, model, harness_version, attribution,
                     started_at, last_activity, turns, subagent_turns, tool_calls, web_searches, input, cache_write_5m,
                     cache_write_1h, cache_write_unsplit, cache_read, output, cost_usd, unpriced_tokens, price_source,
                     synced_at, synced_by_version, tool_calls_lower_bound)
                   VALUES (?, ?, ?, ?, ?, ?, ?, 'transcript', ?, ?, ?, 0, ?, 0, ?, ?, 0, 0, ?, ?, ?, 0, 'builtin', ?, 'demo', 0)""",
                (harness, sid, f"/home/dev/.{harness}/{sid}.jsonl", f"/home/dev/{project}", project, model, h["version"],
                 started, last, turns, len(tools), inp, cw, cr, out, c, now),
            )
            con.executemany(
                "INSERT INTO spans VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?)",
                [(harness, sid, s[0], f"s{s[0]}", s[1], s[2], s[3], s[4], s[5], s[6]) for s in spans],
            )
            mcp = {}
            for s in tools:
                if s[2].startswith("mcp__"):
                    server = s[2].split("__")[1]
                    m = mcp.setdefault(server, [0, 0, 0])
                    m[0] += 1
                    m[1] += s[5]
                    m[2] = max(m[2], s[3] + s[4])
            con.executemany("INSERT INTO mcp_calls VALUES (?, ?, ?, ?, ?, ?)",
                            [(harness, sid, k, v[0], v[1], v[2]) for k, v in mcp.items()])
            con.execute("INSERT INTO sources VALUES (?, ?, ?, 0, 0, ?, 'demo')", (f"/home/dev/.{harness}/{sid}.jsonl", harness, sid, now))
    # One agent built with an SDK, received as OpenTelemetry spans.
    for d in (12, 6, 1):
        t = now - d * day
        inp, cr, out = 4_000, 20_000, 900
        con.execute(
            """INSERT INTO sessions (harness, session_id, source_path, project, model, attribution, started_at, last_activity,
                 turns, subagent_turns, tool_calls, web_searches, input, cache_write_5m, cache_write_1h, cache_write_unsplit,
                 cache_read, output, cost_usd, unpriced_tokens, price_source, synced_at, synced_by_version, tool_calls_lower_bound)
               VALUES ('otel', ?, 'otlp:triage-bot', 'triage-bot', 'claude-sonnet-5', 'telemetry', ?, ?, 1, 0, 2, 0, ?, 0, 0, 0, ?, ?, ?, 0,
                       'builtin', ?, 'demo', 0)""",
            (f"triage-bot/run-{d}", t, t + 9_000, inp, cr, out, cost("claude-sonnet-5", inp, out, cr, 0), now),
        )
    con.commit()
    con.close()
    return n


def shoot(port):
    from playwright.sync_api import sync_playwright

    with sync_playwright() as p:
        browser = p.chromium.launch()
        for scheme in ("light", "dark"):
            page = browser.new_page(viewport={"width": 1280, "height": 900}, device_scale_factor=2, color_scheme=scheme)
            page.goto(f"http://127.0.0.1:{port}/")
            page.wait_for_selector("#sessions table")
            page.wait_for_timeout(300)
            suffix = "" if scheme == "light" else "-dark"
            page.screenshot(path=str(OUT / f"ui-overview{suffix}.png"), clip={"x": 0, "y": 0, "width": 1280, "height": 900})
            if scheme == "light":
                # The session with the most turns, opened through its link.
                page.click("#sessions tbody tr:nth-child(3)")
                page.wait_for_selector("#detail-body svg")
                page.wait_for_timeout(300)
                page.locator("#detail").screenshot(path=str(OUT / "ui-session.png"))
            page.close()
        browser.close()


def main():
    if not Path(BIN).exists():
        sys.exit(f"{BIN} not found; run `cargo build --release` first or set AGENT_TOP_BIN")
    shutil.rmtree(DB_DIR, ignore_errors=True)
    DB_DIR.mkdir(parents=True)
    db = DB_DIR / "agent-top.db"
    home = Path(tempfile.mkdtemp())
    env = {**os.environ, "HOME": str(home), "XDG_DATA_HOME": str(home), "AGENT_TOP_NO_UPDATE_CHECK": "1"}
    # An empty home: sync finds no transcripts and only creates the schema.
    subprocess.run([BIN, "sync", "--db", str(db)], env=env, check=True, stdout=subprocess.DEVNULL)
    print(f"seeded {seed(db)} sessions into {db}")
    port = free_port()
    ui = subprocess.Popen([BIN, "ui", "--addr", f"127.0.0.1:{port}", "--db", str(db)], env=env, stdout=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.1).close()
                break
            except OSError:
                time.sleep(0.1)
        shoot(port)
    finally:
        ui.terminate()
        ui.wait()
        shutil.rmtree(home, ignore_errors=True)
    print("wrote", ", ".join(p.name for p in sorted(OUT.glob("ui-*.png"))))


if __name__ == "__main__":
    main()
