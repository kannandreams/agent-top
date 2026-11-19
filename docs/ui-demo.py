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
OUT = Path(os.environ.get("AGENT_TOP_DEMO_OUT", ROOT / "docs/screenshots"))
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
                "INSERT INTO spans VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, NULL)",
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
    seed_run_graph(con, rnd, now)
    con.commit()
    con.close()
    return n + 1


# The session behind ui-run-graph.png: which turn spawns which subagents, when
# (ms after the turn starts, so two can overlap), and what each one does.
RUN_GRAPH_ID = "0000beef-demo-4000-8000-00000000cafe"
RUN_GRAPH_TURNS = 9
RUN_GRAPH_AGENTS = [
    # turn, agent id, type, model, starts at, tools, errors
    (5, "a41c09e7d2b85f13", "Explore", "claude-sonnet-5", 4_000, ["Grep", "Read", "Read", "Grep", "Read", "Bash"], 0),
    (5, "a7730d1be46c2a98", "Explore", "claude-sonnet-5", 4_300, ["Grep", "Grep", "Read", "mcp__docs__search", "Read"], 0),
    (5, "a02e5f8c91d47b60", "Plan", "claude-opus-5-5", 52_000, ["Read", "Read", "Grep", "Read"], 0),
    (8, "a9d4c6e0173fb285", "general-purpose", "claude-opus-5-5", 6_000,
     ["Read", "Edit", "Bash", "Edit", "Bash", "Edit", "Bash", "mcp__github__get_issue", "Bash"], 2),
]


def seed_run_graph(con, rnd, now):
    """One Claude session whose fifth and eighth turns fan out to subagents."""
    harness, sid, model = "claude", RUN_GRAPH_ID, "claude-opus-5-5"
    # Older than the 30 days the overview shows, so the other images keep
    # their sessions; this one is opened by its link.
    t = now - 31 * 86_400_000
    spans, runs = [], []

    def add(kind, name, at, ms, parent=None, agent=None, error=0, span_id=None):
        sp = {"id": span_id or f"x{len(spans)}", "kind": kind, "name": name, "at": at, "ms": ms, "parent": parent,
              "agent": agent, "error": error}
        spans.append(sp)
        return sp

    def work(at, tools, parent, agent=None, errors=0):
        """An inference before each tool call and one after the last; returns when it ends."""
        bad = set(rnd.sample(range(len(tools)), errors))
        for i, tool in enumerate(tools):
            inf = int(rnd.lognormvariate(8.0, 0.4))
            add("inference", "inference", at, inf, parent, agent)
            at += inf
            ms = int(TOOL_MS[tool] * rnd.lognormvariate(0, 0.5))
            add("tool", tool, at, ms, parent, agent, int(i in bad))
            at += ms + 40
        inf = int(rnd.lognormvariate(8.0, 0.4))
        add("inference", "inference", at, inf, parent, agent)
        return at + inf

    for n in range(1, RUN_GRAPH_TURNS + 1):
        turn = add("turn", "turn", t, 0)
        end = work(t, rnd.choices(["Read", "Grep", "Bash", "Edit"], k=rnd.randint(2, 5)), turn)
        for (at_turn, agent, kind, agent_model, offset, tools, errors) in RUN_GRAPH_AGENTS:
            if at_turn != n:
                continue
            start = max(t + offset, end if offset > 30_000 else t + offset)
            call = add("tool", "Agent", start, 0, turn, span_id=f"toolu_demo_{agent[:6]}")
            own = add("turn", "turn", start + 200, 0, agent=agent)
            done = work(start + 200, tools, own, agent, errors)
            own["ms"] = done - own["at"]
            call["ms"] = done + 150 - start
            end = max(end, start + call["ms"])
            tokens = len(tools) * rnd.randint(40_000, 90_000)
            runs.append((harness, sid, agent, kind, call["id"], agent_model, own["at"], done, tokens,
                         cost(agent_model, tokens // 50, tokens // 40, tokens - tokens // 50 - tokens // 40, 0), 0, len(tools)))
        end = work(end, ["Edit", "Bash"], turn)
        turn["ms"] = end - t
        t = end + rnd.randint(60_000, 900_000)

    spans.sort(key=lambda sp: sp["at"])
    seq = {id(sp): i for i, sp in enumerate(spans)}
    con.executemany(
        "INSERT INTO spans VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [(harness, sid, i, sp["id"], sp["kind"], sp["name"], sp["at"], sp["ms"], int(sp["agent"] is not None), sp["error"],
          seq[id(sp["parent"])] if sp["parent"] else None, sp["agent"]) for i, sp in enumerate(spans)],
    )
    con.executemany("INSERT INTO subagents VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", runs)
    tools = [sp for sp in spans if sp["kind"] == "tool"]
    inp, cr, cw, out = 21_000, 6_400_000, 310_000, 48_000
    con.execute(
        """INSERT INTO sessions (harness, session_id, source_path, cwd, project, model, harness_version, attribution,
             started_at, last_activity, turns, subagent_turns, tool_calls, web_searches, input, cache_write_5m,
             cache_write_1h, cache_write_unsplit, cache_read, output, cost_usd, unpriced_tokens, price_source,
             synced_at, synced_by_version, tool_calls_lower_bound)
           VALUES (?, ?, ?, '/home/dev/acme/api', 'acme/api', ?, ?, 'transcript', ?, ?, ?, ?, ?, 0, ?, ?, 0, 0, ?, ?, ?, 0,
                   'builtin', ?, 'demo', 0)""",
        (harness, sid, f"/home/dev/.claude/{sid}.jsonl", model, HARNESSES[harness]["version"], spans[0]["at"],
         max(sp["at"] + sp["ms"] for sp in spans), RUN_GRAPH_TURNS + len(runs), len(runs), len(tools), inp, cw, cr, out,
         cost(model, inp, out, cr, cw) + sum(r[9] for r in runs), now),
    )
    for server in ("docs", "github"):
        calls = [sp for sp in tools if sp["name"].startswith(f"mcp__{server}__")]
        con.execute("INSERT INTO mcp_calls VALUES (?, ?, ?, ?, ?, ?)",
                    (harness, sid, server, len(calls), sum(sp["error"] for sp in calls), max(sp["at"] for sp in calls)))
    con.execute("INSERT INTO sources VALUES (?, ?, ?, 0, 0, ?, 'demo')", (f"/home/dev/.claude/{sid}.jsonl", harness, sid, now))


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
                # The session with subagents, and one of them selected.
                page.goto(f"http://127.0.0.1:{port}/#s/claude/{RUN_GRAPH_ID}")
                page.reload()
                page.wait_for_selector("#detail-body .graph svg")
                page.click("#detail-body .graph .hit >> nth=0")
                page.wait_for_selector("#detail-body .run .waterfall svg[aria-label^='Span waterfall']")
                page.wait_for_timeout(300)
                page.mouse.move(0, 0)
                page.locator("#detail-body .run").screenshot(path=str(OUT / "ui-run-graph.png"))
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
