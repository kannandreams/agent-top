"""Screenshots of `agent-top ui` for the docs, from a made-up store.

Never point the web view at a real store for a public image: it shows project
names, working directories and session ids. This script builds a store from
nothing, with invented projects and sessions dated over the last 30 days,
starts `agent-top ui` on it and saves the images under docs/screenshots/.

It also writes the hosted demo, docs/demo/ (the page plus one JSON file per
API request, answered by docs/demo/demo.js), and two recordings that need
ffmpeg: docs/ui-demo.gif, a walk through the page, and docs/context-demo.mp4,
a walk through the context timeline. `ui-demo.py context` records only the
second.

    cargo build --release
    uv run --with playwright python docs/ui-demo.py

Change what the images show by editing the tables below. The random numbers
come from a fixed seed, so a rerun differs only in the dates.
"""

import json
import os
import random
import re
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
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
            seed_context(con, harness, sid, main_steps(spans, lambda s: s[1], lambda s: s[2], lambda s: s[3]))
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
    for g in RUN_GRAPHS:
        seed_run_graph(con, rnd, now, g)
    # Codex writes its model's window; the other harnesses do not.
    con.execute("UPDATE sessions SET context_window = 258400 WHERE harness = 'codex'")
    con.commit()
    con.close()
    return n + 1


# Sessions with subagents, for the run graph. The first is behind
# ui-run-graph.png and is where the hosted demo opens. Each lists which turn
# spawns which subagent, when (ms after the turn starts, so two can overlap),
# and what it does.
RUN_GRAPH_ID = "0000beef-demo-4000-8000-00000000cafe"
RUN_GRAPHS = [
    {"harness": "claude", "id": RUN_GRAPH_ID, "model": "claude-opus-5-5", "project": "acme/api", "days_ago": 1,
     "turns": 9, "spawn": "Agent", "main_tools": ["Read", "Grep", "Bash", "Edit"], "mcp": ("docs", "github"),
     "agents": [
         # turn, agent id, type, model, starts at, tools, errors
         (5, "a41c09e7d2b85f13", "Explore", "claude-sonnet-5", 4_000, ["Grep", "Read", "Read", "Grep", "Read", "Bash"], 0),
         (5, "a7730d1be46c2a98", "Explore", "claude-sonnet-5", 4_300, ["Grep", "Grep", "Read", "mcp__docs__search", "Read"], 0),
         (5, "a02e5f8c91d47b60", "Plan", "claude-opus-5-5", 52_000, ["Read", "Read", "Grep", "Read"], 0),
         (8, "a9d4c6e0173fb285", "general-purpose", "claude-opus-5-5", 6_000,
          ["Read", "Edit", "Bash", "Edit", "Bash", "Edit", "Bash", "mcp__github__get_issue", "Bash"], 2),
     ]},
    {"harness": "claude", "id": "0000beef-demo-4000-8000-0000000d0c50", "model": "claude-sonnet-5", "project": "acme/web",
     "days_ago": 4, "turns": 6, "spawn": "Agent", "main_tools": ["Read", "Edit", "Bash"], "mcp": (),
     "agents": [
         (2, "a5b1e8d07c3f9a24", "Explore", "claude-sonnet-5", 3_000, ["Grep", "Read", "Grep", "Read"], 0),
         (4, "a6c3f1a92e7d4b08", "general-purpose", "claude-sonnet-5", 5_000, ["Read", "Edit", "Bash", "Edit", "Bash"], 0),
     ]},
    {"harness": "opencode", "id": "ses_0beefdemo4fan7out2cafe", "model": "deepseek-v4-flash", "project": "tools/cli",
     "days_ago": 2, "turns": 5, "spawn": "task", "main_tools": ["read", "edit", "bash", "grep"], "mcp": (),
     "agents": [
         (2, "ses_0beefdemo4child01explr", "explore", "deepseek-v4-flash", 2_500, ["grep", "read", "grep", "read"], 0),
         (2, "ses_0beefdemo4child02explr", "explore", "deepseek-v4-flash", 2_900, ["grep", "read", "read"], 0),
         (4, "ses_0beefdemo4child03gener", "general", "deepseek-v4-flash", 4_000, ["read", "edit", "bash", "bash"], 1),
     ]},
    {"harness": "claude", "id": "0000beef-demo-4000-8000-00000000e7a1", "model": "claude-opus-5-5", "project": "lab/evals",
     "days_ago": 9, "turns": 7, "spawn": "Agent", "main_tools": ["Read", "Bash", "Edit"], "mcp": (),
     "agents": [
         (3, "a1f0c4b7e9d2a365", "Plan", "claude-opus-5-5", 3_500, ["Read", "Grep", "Read"], 0),
         (6, "a8e2d5c1b4f7a093", "general-purpose", "claude-opus-5-5", 4_500, ["Bash", "Read", "Bash", "Edit", "Bash"], 0),
         (6, "a3d7b9e0c2f5a816", "general-purpose", "claude-opus-5-5", 5_200, ["Bash", "Bash", "Read"], 1),
     ]},
]


# Tokens a tool's result adds to the context, before the random spread.
RESULT_TOKENS = {"Read": 7000, "read": 6000, "Bash": 3000, "bash": 2500, "exec_command": 2800, "Grep": 1500, "grep": 1400,
                 "WebFetch": 9000, "web_search": 6000, "Edit": 600, "edit": 500, "Write": 400, "apply_patch": 700,
                 "mcp__github__get_issue": 4000, "mcp__docs__search": 6000}


def seed_context(con, harness, sid, steps, limit=None, agent=""):
    """The context timeline of one agent in a session ('' the main agent): a
    point per inference, `steps` being (time, the tool whose result it read or
    None), and a compaction where the context passes the session's limit."""
    # Its own generator, so the other invented data stays as it was.
    rnd = random.Random(sid + agent)
    limit = limit or rnd.choice([160_000, 200_000, 240_000])
    live, points, comps, sources = {}, [], [], []
    def add(origin, name, n):
        live[(origin, name)] = live.get((origin, name), 0) + n
    add("other", "other", rnd.randint(30_000, 42_000))
    for seq, (at, tool) in enumerate(steps):
        prompt = sum(live.values())
        if prompt > limit:
            summary = rnd.randint(9_000, 22_000)
            halved = harness == "codex"
            comps.append((harness, sid, agent, seq, at, "unknown" if halved or harness == "opencode" else "auto",
                          "prompt-halved" if halved else "harness", prompt, None if halved else summary))
            top = sorted(live.items(), key=lambda kv: -kv[1])[:10]
            sources += [(harness, sid, agent, seq, o, n, v) for (o, n), v in top]
            live.clear()
            add("other", "other", rnd.randint(34_000, 44_000) + summary)
        if tool:
            n = int(RESULT_TOKENS.get(tool, 2000) * rnd.lognormvariate(0, 0.6))
            add("mcp" if tool.startswith("mcp__") else "tool", tool.split("__")[1] if tool.startswith("mcp__") else tool, n)
        add("other", "other", rnd.randint(300, 2_500))
        by = {"tool": 0, "mcp": 0, "other": 0}
        for (o, _), v in live.items():
            by[o] += v
        points.append((harness, sid, agent, seq, at, sum(by.values()), rnd.randint(200, 3_000), int(sum(by.values()) * 0.9),
                       by["tool"], by["mcp"], by["other"]))
    con.executemany("INSERT INTO context_points VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", points)
    con.executemany("INSERT INTO compactions VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    [c + (points[c[3]][5],) for c in comps])
    con.executemany("INSERT INTO compaction_sources VALUES (?, ?, ?, ?, ?, ?, ?)", sources)


def main_steps(spans, kind, name, at, agent=lambda s: None, who=None):
    """(time, tool read) for each inference of one agent (None the main
    agent), in order."""
    steps, last = [], None
    for s in sorted(spans, key=at):
        if agent(s) != who:
            continue
        if kind(s) == "tool":
            last = name(s)
        elif kind(s) == "inference":
            steps.append((at(s), last))
            last = None
    return steps


def seed_run_graph(con, rnd, now, g):
    """One session whose turns fan out to subagents, as RUN_GRAPHS describes it."""
    harness, sid, model = g["harness"], g["id"], g["model"]
    t = now - g["days_ago"] * 86_400_000 - 3 * 3_600_000
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

    for n in range(1, g["turns"] + 1):
        turn = add("turn", "turn", t, 0)
        end = work(t, rnd.choices(g["main_tools"], k=rnd.randint(2, 5)), turn)
        for (at_turn, agent, kind, agent_model, offset, tools, errors) in g["agents"]:
            if at_turn != n:
                continue
            start = max(t + offset, end if offset > 30_000 else t + offset)
            call = add("tool", g["spawn"], start, 0, turn, span_id=f"toolu_demo_{agent[-6:]}")
            own = add("turn", "turn", start + 200, 0, agent=agent)
            done = work(start + 200, tools, own, agent, errors)
            own["ms"] = done - own["at"]
            call["ms"] = done + 150 - start
            end = max(end, start + call["ms"])
            tokens = len(tools) * rnd.randint(40_000, 90_000)
            runs.append((harness, sid, agent, kind, call["id"], agent_model, own["at"], done, tokens,
                         cost(agent_model, tokens // 50, tokens // 40, tokens - tokens // 50 - tokens // 40, 0), 0, len(tools)))
        end = work(end, g["main_tools"][-2:], turn)
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
    seed_context(con, harness, sid, main_steps(spans, lambda s: s["kind"], lambda s: s["name"], lambda s: s["at"], lambda s: s["agent"]),
                 limit=90_000 if sid == RUN_GRAPH_ID else None)
    for r in runs:
        steps = main_steps(spans, lambda s: s["kind"], lambda s: s["name"], lambda s: s["at"], lambda s: s["agent"], who=r[2])
        seed_context(con, harness, sid, steps, agent=r[2])
    tools = [sp for sp in spans if sp["kind"] == "tool"]
    k = g["turns"] / 9
    inp, cr, cw, out = int(21_000 * k), int(6_400_000 * k), int(310_000 * k), int(48_000 * k)
    con.execute(
        """INSERT INTO sessions (harness, session_id, source_path, cwd, project, model, harness_version, attribution,
             started_at, last_activity, turns, subagent_turns, tool_calls, web_searches, input, cache_write_5m,
             cache_write_1h, cache_write_unsplit, cache_read, output, cost_usd, unpriced_tokens, price_source,
             synced_at, synced_by_version, tool_calls_lower_bound)
           VALUES (?, ?, ?, ?, ?, ?, ?, 'transcript', ?, ?, ?, ?, ?, 0, ?, ?, 0, 0, ?, ?, ?, 0,
                   'builtin', ?, 'demo', 0)""",
        (harness, sid, f"/home/dev/.{harness}/{sid}.jsonl", f"/home/dev/{g['project']}", g["project"], model,
         HARNESSES[harness]["version"], spans[0]["at"], max(sp["at"] + sp["ms"] for sp in spans), g["turns"] + len(runs),
         len(runs), len(tools), inp, cw, cr, out, cost(model, inp, out, cr, cw) + sum(r[9] for r in runs), now),
    )
    for server in g["mcp"]:
        calls = [sp for sp in tools if sp["name"].startswith(f"mcp__{server}__")]
        con.execute("INSERT INTO mcp_calls VALUES (?, ?, ?, ?, ?, ?)",
                    (harness, sid, server, len(calls), sum(sp["error"] for sp in calls), max(sp["at"] for sp in calls)))
    con.execute("INSERT INTO sources VALUES (?, ?, ?, 0, 0, ?, 'demo')", (f"/home/dev/.{harness}/{sid}.jsonl", harness, sid, now))


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
                # The detail opens in a side panel that scrolls on its own, so
                # the panel's contents are shot in a window tall enough to hold them.
                page.set_viewport_size({"width": 1280, "height": 2400})
                page.click("#sessions tbody tr:nth-child(3)")
                page.wait_for_selector("#detail-body svg")
                page.wait_for_timeout(300)
                page.locator("#detail-body").screenshot(path=str(OUT / "ui-session.png"))
                # The session with subagents; the first is selected when it opens.
                page.goto(f"http://127.0.0.1:{port}/#s/claude/{RUN_GRAPH_ID}")
                page.reload()
                page.wait_for_selector("#detail-body .run .waterfall svg[aria-label^='Span waterfall']")
                page.wait_for_timeout(300)
                page.mouse.move(0, 0)
                page.locator("#detail-body .run").screenshot(path=str(OUT / "ui-run-graph.png"))
                # Its context timeline, with the first compaction selected.
                page.locator("#detail-body .ctx tbody tr").first.click()
                page.wait_for_timeout(200)
                page.mouse.move(0, 0)
                page.locator("#detail-body .ctx").screenshot(path=str(OUT / "ui-context.png"))
            page.close()
        browser.close()


DEMO = ROOT / "docs/demo"
GIF = ROOT / "docs/ui-demo.gif"
WEB = ROOT / "crates/agent-top/web"
DAY_MS = 86_400_000


def key(s):
    """The file-name form of a value, the same as `key` in docs/demo/demo.js."""
    return re.sub(r"[^A-Za-z0-9._-]", "_", s or "") or "_"


def export(port):
    """Answer every request the page can make, as files under docs/demo/api/."""
    def get(path, **params):
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/{path}?{urllib.parse.urlencode(params)}") as r:
            return json.load(r)

    def put(name, value):
        f = DEMO / "api" / f"{name}.json"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(json.dumps(value, separators=(",", ":")))

    shutil.rmtree(DEMO / "api", ignore_errors=True)
    meta = get("meta")
    meta["store"] = "demo store, invented sessions"
    put("meta", meta)
    now = int(time.time() * 1000)
    for days in (7, 30, 90, 0):
        for harness in [""] + [h["harness"] for h in meta["harnesses"]]:
            since = now - days * DAY_MS if days else 0
            for path in ("overview", "tools", "mcp", "sessions"):
                put(f"{path}-{days}-{key(harness)}", get(path, since=since, harness=harness))
    files = 0
    for s in get("sessions", since=0, harness=""):
        d = get("session", harness=s["harness"], id=s["session_id"])
        put(f"session/{key(s['harness'])}/{key(s['session_id'])}", d)
        files += 1
        ctx = get("context", harness=s["harness"], id=s["session_id"])
        put(f"context/{key(s['harness'])}/{key(s['session_id'])}", ctx)
        for a in ctx["agents"]:
            if a["agent"]:
                put(f"context/{key(s['harness'])}/{key(s['session_id'])}/{key(a['agent'])}",
                    get("context", harness=s["harness"], id=s["session_id"], agent=a["agent"]))
        for a in (d.get("graph") or {}).get("agents", []):
            spans = get("spans", harness=s["harness"], id=s["session_id"], agent=a["agent_id"])
            put(f"spans/{key(s['harness'])}/{key(s['session_id'])}/{key(a['agent_id'])}", spans)
    # The page itself, with relative links so it works under /demo/ and the shim ahead of app.js.
    html = (WEB / "index.html").read_text()
    # data-open: the session the demo opens with, the one with a run graph.
    html = html.replace('href="/app.css"', 'href="app.css"').replace(
        '<script src="/app.js" defer></script>',
        f'<script src="demo.js" data-open="#s/claude/{RUN_GRAPH_ID}"></script>\n<script src="app.js" defer></script>')
    html = html.replace("<title>agent-top</title>", "<title>agent-top ui demo</title>")
    html = re.sub(r'<footer class="foot" id="foot">.*?</footer>',
                  '<footer class="foot" id="foot">A demo of <code>agent-top ui</code> on invented sessions. '
                  'Run <code>agent-top sync &amp;&amp; agent-top ui</code> to see your own. '
                  '<a href="../store/">About the local store</a></footer>', html, flags=re.S)
    (DEMO / "index.html").write_text(html)
    shutil.copy(WEB / "app.js", DEMO / "app.js")
    shutil.copy(WEB / "app.css", DEMO / "app.css")
    print(f"exported the demo to {DEMO} ({files} sessions)")


def record(port):
    """A walk through the page, recorded as video and turned into docs/ui-demo.gif."""
    from playwright.sync_api import sync_playwright

    if not shutil.which("ffmpeg"):
        print("ffmpeg not found; skipping", GIF.name)
        return
    vid = Path(tempfile.mkdtemp())
    size = {"width": 1280, "height": 800}
    with sync_playwright() as p:
        browser = p.chromium.launch()
        ctx = browser.new_context(viewport=size, record_video_dir=str(vid), record_video_size=size, color_scheme="light")
        page = ctx.new_page()
        page.goto(f"http://127.0.0.1:{port}/")
        page.wait_for_selector("#sessions table")
        page.wait_for_timeout(1800)
        for r in ("7", "90", "30"):
            page.click(f"#range button[data-range='{r}']")
            page.wait_for_timeout(1300)
        page.select_option("#harness", "claude")
        page.wait_for_timeout(1300)
        page.select_option("#harness", "")
        page.wait_for_timeout(800)
        # A jump, not a smooth scroll: every scrolled frame is a new GIF frame.
        page.evaluate("document.getElementById('h-sessions').scrollIntoView({block: 'start'})")
        page.wait_for_timeout(1500)
        # The run graph: open the session with subagents from its row; the
        # side panel opens on its first subagent. Then walk the others.
        page.click(f"#sessions tr[data-key='claude/{RUN_GRAPH_ID}']")
        page.wait_for_selector("#detail-body .run .waterfall svg[aria-label^='Span waterfall']")
        page.wait_for_timeout(2200)
        hits = page.locator("#detail-body .graph .hit")
        for i in range(1, hits.count()):
            hits.nth(i).hover()
            page.wait_for_timeout(500)
            hits.nth(i).click()
            page.wait_for_selector("#detail-body .run .waterfall svg[aria-label^='Span waterfall']")
            page.wait_for_timeout(1600)
        # The table stays beside the panel: pick another session, then close.
        page.mouse.move(100, 300)
        # The panel covers the right of the row, so click its left end.
        page.locator("#sessions tr[data-key^='codex/']").first.click(position={"x": 60, "y": 10})
        page.wait_for_selector("#detail-body svg")
        page.wait_for_timeout(2000)
        page.keyboard.press("Escape")
        page.wait_for_timeout(800)
        page.click("#theme")
        page.wait_for_timeout(1800)
        video = page.video.path()
        ctx.close()
        browser.close()
    ff = ["ffmpeg", "-y", "-loglevel", "error", "-i", video]
    subprocess.run(ff + ["-vf", "fps=8,scale=880:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=64:stats_mode=diff[p];"
                         "[b][p]paletteuse=dither=none:diff_mode=rectangle", str(GIF)], check=True)
    # The same walk as MP4, for posts: smaller and sharper than the GIF.
    mp4 = GIF.with_suffix(".mp4")
    subprocess.run(ff + ["-vf", "fps=30,scale=1280:-2", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "24",
                         "-movflags", "+faststart", "-an", str(mp4)], check=True)
    shutil.rmtree(vid, ignore_errors=True)
    for f in (GIF, mp4):
        print(f"wrote {f} ({f.stat().st_size // 1024} KB)")


CONTEXT_MP4 = ROOT / "docs/context-demo.mp4"
CAPTION_CSS = ("max-width:80%;padding:12px 22px;border-radius:10px;background:#2457d6;color:#fff;"
               "font:600 21px/1.35 'JetBrains Mono',Menlo,monospace;text-align:center;box-shadow:0 4px 18px rgba(0,0,0,.35)")
# The context video is narrower than the page walk: it is shown small in a feed.
CONTEXT_SIZE = {"width": 1040, "height": 780}

# A pointer and a caption for the recording only: headless video draws
# neither, and the page itself has no captions.
DEMO_OVERLAY = """
addEventListener('DOMContentLoaded', () => {
  const s = document.createElement('style');
  s.textContent = `#demo-cursor{position:fixed;z-index:9999;width:18px;height:18px;margin:-9px 0 0 -9px;border-radius:50%;
    background:rgba(224,98,42,.35);border:2px solid #e0622a;pointer-events:none;left:-40px;top:-40px;transition:transform .08s}
    #demo-cursor.down{transform:scale(.7)}
    #demo-cap{position:fixed;z-index:9998;left:50%;bottom:28px;transform:translateX(-50%);CAPTION;
    pointer-events:none;opacity:0;transition:opacity .25s}`;
  document.head.append(s);
  const c = document.createElement('div'); c.id = 'demo-cursor'; document.body.append(c);
  const cap = document.createElement('div'); cap.id = 'demo-cap'; document.body.append(cap);
  addEventListener('mousemove', (e) => { c.style.left = e.clientX + 'px'; c.style.top = e.clientY + 'px'; }, true);
  addEventListener('mousedown', () => c.classList.add('down'), true);
  addEventListener('mouseup', () => c.classList.remove('down'), true);
});
""".replace("CAPTION", CAPTION_CSS)


def record_context(port):
    """A walk through the context timeline, in the web view and then in the
    terminal, as docs/context-demo.mp4."""
    from playwright.sync_api import sync_playwright

    if not shutil.which("ffmpeg"):
        print("ffmpeg not found; skipping", CONTEXT_MP4.name)
        return
    vid = Path(tempfile.mkdtemp())
    size = CONTEXT_SIZE
    with sync_playwright() as p:
        browser = p.chromium.launch()
        # bypass_csp: the page's policy refuses the overlay's inline style.
        ctx = browser.new_context(viewport=size, record_video_dir=str(vid), record_video_size=size, color_scheme="dark",
                                  bypass_csp=True)
        ctx.add_init_script(DEMO_OVERLAY)
        page = ctx.new_page()

        def caption(text, ms=0):
            page.evaluate("t => { const c = document.getElementById('demo-cap'); c.textContent = t; c.style.opacity = t ? 1 : 0; }", text)
            page.wait_for_timeout(ms)

        def glide(x, y, steps=18):
            page.mouse.move(x, y, steps=steps)

        def click(locator, ms=900):
            box = locator.bounding_box()
            glide(box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
            page.wait_for_timeout(250)
            locator.click()
            page.wait_for_timeout(ms)

        def sweep(ms=2600, frm=0.04, to=0.96):
            pane = page.locator("#detail-body .ctx .chart svg").first.bounding_box()
            y = pane["y"] + pane["height"] * 0.55
            glide(pane["x"] + pane["width"] * frm, y, steps=10)
            n = 40
            for i in range(n + 1):
                page.mouse.move(pane["x"] + pane["width"] * (frm + (to - frm) * i / n), y)
                page.wait_for_timeout(ms // n)

        def show_ctx():
            page.evaluate("document.querySelector('#detail-body .ctx').scrollIntoView({block: 'start'})")
            page.wait_for_timeout(400)

        page.goto(f"http://127.0.0.1:{port}/")
        page.wait_for_selector("#sessions table")
        page.mouse.move(640, 400)
        caption("agent-top 0.30: watch your coding agent's context fill up", 2400)
        page.evaluate("document.getElementById('h-sessions').scrollIntoView({block: 'start'})")
        page.wait_for_timeout(600)
        caption("Open a session", 600)
        click(page.locator(f"#sessions tr[data-key='claude/{RUN_GRAPH_ID}']").locator("td").first, 1600)
        page.wait_for_selector("#detail-body .ctx .chart svg")
        show_ctx()
        caption("Each point is one model response and the tokens it sent", 600)
        sweep()
        caption("When the context gets full, the agent summarises it and starts again", 1400)
        rules = page.locator("#detail-body .ctx .rule-hit")
        click(rules.nth(0), 1400)
        caption("Click a compaction to see what filled the context before it", 600)
        page.evaluate("document.querySelector('#detail-body .ctx .ctx-sources').scrollIntoView({block: 'end', behavior: 'smooth'})")
        page.wait_for_timeout(2600)
        show_ctx()
        click(page.locator("#detail-body .ctx tbody tr").nth(1), 1600)
        caption("Split it by source: tool results, MCP results, prompts and replies", 400)
        click(page.locator("#detail-body .ctx .seg button").nth(1), 900)
        sweep(2400)
        click(page.locator("#detail-body .ctx .seg button").nth(0), 600)
        caption("Each subagent starts empty, so it gets its own chart", 400)
        pick = page.locator("#detail-body .ctx select")
        values = pick.locator("option").evaluate_all("os => os.map(o => o.value)")
        for v in values[1:3] + values[:1]:
            box = pick.bounding_box()
            glide(box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
            page.wait_for_timeout(300)
            pick.select_option(v)
            page.wait_for_timeout(500)
            pick = page.locator("#detail-body .ctx select")
            sweep(1500, 0.05, 0.9)
        caption("Codex sessions also show the model's context limit", 400)
        page.mouse.move(100, 300)
        page.evaluate("document.getElementById('h-sessions').scrollIntoView({block: 'start'})")
        page.wait_for_timeout(500)
        click(page.locator("#sessions tr[data-key^='codex/']").first.locator("td").first, 1400)
        page.wait_for_selector("#detail-body .ctx .chart svg")
        show_ctx()
        caption("The dashed line is that limit; hover for how close each response came", 300)
        sweep(3000)
        caption("", 400)
        video = page.video.path()
        ctx.close()
        browser.close()
    web = vid / "web.mp4"
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", video, "-vf", "fps=30,setsar=1", "-c:v", "libx264",
                    "-pix_fmt", "yuv420p", "-crf", "20", "-an", str(web)], check=True)
    parts = [web]
    term = record_terminal(vid)
    if term:
        parts.append(term)
    inputs = [a for f in parts for a in ("-i", str(f))]
    chain = "".join(f"[{i}:v]" for i in range(len(parts))) + f"concat=n={len(parts)}:v=1:a=0[v]"
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", *inputs, "-filter_complex", chain, "-map", "[v]", "-c:v", "libx264",
                    "-pix_fmt", "yuv420p", "-crf", "20", "-movflags", "+faststart", "-an", str(CONTEXT_MP4)], check=True)
    shutil.rmtree(vid, ignore_errors=True)
    print(f"wrote {CONTEXT_MP4} ({CONTEXT_MP4.stat().st_size // 1024} KB)")


def record_terminal(work):
    """The same view in the live terminal UI, recorded with vhs.

    The live view also lists every agent process on the machine it runs on, so
    it runs in a container that has only invented transcripts and no other
    process. It needs Docker, vhs and a Linux build of agent-top in
    AGENT_TOP_LINUX_BIN, for example:

        docker run --rm -v "$PWD":/src -w /src -e CARGO_TARGET_DIR=target/linux \
            rust:1-bookworm cargo build --release -p agent-top
        AGENT_TOP_LINUX_BIN=target/linux/release/agent-top uv run --with playwright python docs/ui-demo.py context
    """
    from playwright.sync_api import sync_playwright

    linux = os.environ.get("AGENT_TOP_LINUX_BIN")
    if not (linux and Path(linux).exists() and shutil.which("docker") and shutil.which("vhs")):
        print("AGENT_TOP_LINUX_BIN, docker or vhs missing; the video has no terminal part")
        return None
    home = work / "home"
    subprocess.run([sys.executable, str(ROOT / "docs/context-demo-transcripts.py"), str(home)], check=True)
    # (keys, seconds to wait after them, caption from here on)
    steps = [
        # Captions sit at the top here: the table under the chart is the point.
        (None, 2.4, "The same view in the terminal: run agent-top and pick a session"),
        ("Down", 1.4, None),
        ("c", 3.2, "Press c for the context timeline"),
        ("Tab", 2.4, "Tab steps through the subagents"),
        ("Tab", 2.2, None),
        ("Tab", 1.4, None),
        ("s", 3.0, "s splits it by source"),
        ("s", 1.0, None),
        ("Escape", 0.8, None),
        ("Up", 0.8, "Codex sessions show the context limit"),
        ("c", 3.6, None),
        ("Escape", 0.6, "brew install kannandreams/tap/agent-top"),
        (None, 3.0, None),
    ]
    tape = [f'Output "{work / "term.mp4"}"', 'Set Shell "bash"', "Set FontSize 15",
            f"Set Width {CONTEXT_SIZE['width']}", f"Set Height {CONTEXT_SIZE['height']}",
            'Set Theme "Catppuccin Mocha"', "Set Padding 14", "Set TypingSpeed 0", "Hide",
            f'Type "docker run --rm -it --hostname demo -e HOME=/root -e TERM=xterm-256color -e AGENT_TOP_NO_UPDATE_CHECK=1 '
            f'-v {home}:/root:ro -v {Path(linux).resolve()}:/usr/local/bin/agent-top:ro debian:bookworm-slim '
            f'agent-top --theme dark --stopped-window-min 600"', "Enter", "Sleep 4s", "Show"]
    cues, t, current = [], 0.0, None
    for keys, wait, cap in steps:
        if cap is not None:
            if current:
                cues.append((current[0], t, current[1]))
            current = (t, cap)
        if keys:
            # Named keys are commands of their own; a letter is typed.
            tape.append(keys if len(keys) > 1 else f'Type "{keys}"')
        tape.append(f"Sleep {wait}s")
        t += wait
    cues.append((current[0], t, current[1]))
    tape += ["Hide", 'Type "q"', "Sleep 1s"]
    (work / "term.tape").write_text("\n".join(tape) + "\n")
    subprocess.run(["vhs", str(work / "term.tape")], check=True, stdout=subprocess.DEVNULL)

    # Captions as images, in the web part's style: this ffmpeg may have no drawtext.
    pngs = []
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport=CONTEXT_SIZE)
        for n, (_, _, text) in enumerate(cues):
            page.set_content(f'<body style="margin:0;background:transparent"><div id="c" style="display:inline-block;{CAPTION_CSS};max-width:880px">'
                             f"{text}</div></body>")
            f = work / f"cap{n}.png"
            page.locator("#c").screenshot(path=str(f), omit_background=True)
            pngs.append(f)
        browser.close()
    inputs = ["-i", str(work / "term.mp4")] + [a for f in pngs for a in ("-i", str(f))]
    chain, prev = [], "0:v"
    for n, (a, b, _) in enumerate(cues):
        out = f"v{n}"
        chain.append(f"[{prev}][{n + 1}:v]overlay=(W-w)/2:22:enable='between(t,{a:.2f},{b:.2f})'[{out}]")
        prev = out
    chain.append(f"[{prev}]fps=30,scale={CONTEXT_SIZE['width']}:{CONTEXT_SIZE['height']},setsar=1[v]")
    term = work / "term-captioned.mp4"
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", *inputs, "-filter_complex", ";".join(chain), "-map", "[v]",
                    "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", "-an", str(term)], check=True)
    return term


def main():
    only_context = sys.argv[1:] == ["context"]
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
        if only_context:
            record_context(port)
            return
        shoot(port)
        export(port)
        record(port)
        record_context(port)
    finally:
        ui.terminate()
        ui.wait()
        shutil.rmtree(home, ignore_errors=True)
    print("wrote", ", ".join(p.name for p in sorted(OUT.glob("ui-*.png"))))


if __name__ == "__main__":
    main()
