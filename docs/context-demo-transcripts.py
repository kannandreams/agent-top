"""Invented transcripts for the terminal part of docs/context-demo.mp4.

The live view reads transcripts, not the store, so the terminal recording needs
a home directory of its own: one Claude Code session with compactions and two
subagents, and one Codex session that writes its context window. Nothing here
comes from a real session. Times end at the moment the script runs, so the
sessions show as recently stopped.

    python3 docs/context-demo-transcripts.py OUT

writes OUT/.claude/projects/... and OUT/.codex/sessions/..., for HOME=OUT.
"""

import json
import random
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

CLAUDE_ID = "0000beef-demo-4000-8000-00000000c0de"
CODEX_ID = "01000000-demo-7000-8000-00000000c0de"
VERSION = "2.1.308"
# Tokens a tool's result adds to the context, as a range.
GROWTH = {"Read": (5_000, 16_000), "Bash": (1_500, 7_000), "Grep": (800, 3_000), "Edit": (400, 1_200),
          "mcp__github__get_issue": (2_500, 6_000)}
TOOLS = ["Read", "Bash", "Grep", "Read", "Edit", "Bash", "mcp__github__get_issue"]


def ts(t):
    return t.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def claude(home, rnd, end):
    cwd = "/home/dev/acme/api"
    proj = home / ".claude/projects" / cwd.replace("/", "-")
    proj.mkdir(parents=True, exist_ok=True)
    base = {"version": VERSION, "isSidechain": False, "cwd": cwd, "sessionId": CLAUDE_ID}
    lines, n, t = [], 0, end - timedelta(minutes=26)
    lines.append({"type": "user", "timestamp": ts(t), **base, "message": {"role": "user", "content": "Fix the flaky retry test"}})
    floor, prompt, cached = 42_000, 30_000, 0
    subagents = []

    def respond(name, tool_id, prompt, cached, stop="tool_use"):
        usage = {"input_tokens": 4, "cache_read_input_tokens": cached, "cache_creation_input_tokens": prompt - cached - 4,
                 "output_tokens": rnd.randint(150, 900)}
        content = [{"type": "tool_use", "id": tool_id, "name": name}] if name else []
        return {"type": "assistant", "timestamp": ts(t), **base,
                "message": {"id": f"msg_{tool_id}", "model": "claude-opus-5-5", "stop_reason": stop, "usage": usage, "content": content}}

    while n < 64:
        n += 1
        t += timedelta(seconds=rnd.randint(9, 20))
        if n in (14, 40):
            tid = f"toolu_agent{n}"
            lines.append(respond("Agent", tid, prompt, cached))
            subagents.append((f"a{n:02d}demo{n}", "Explore" if n == 14 else "Plan", tid, t))
            t += timedelta(seconds=rnd.randint(40, 70))
            lines.append({"type": "user", "timestamp": ts(t), **base,
                          "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid}]}})
            cached, prompt = prompt, prompt + rnd.randint(2_000, 4_000)
            continue
        name = TOOLS[(n * 5 + rnd.randint(0, 6)) % len(TOOLS)]
        tid = f"toolu_{n:04d}"
        lines.append(respond(name, tid, prompt, cached, "end_turn" if n == 64 else "tool_use"))
        t += timedelta(seconds=rnd.randint(1, 6))
        lines.append({"type": "user", "timestamp": ts(t), **base,
                      "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid}]}})
        cached = prompt
        prompt += rnd.randint(*GROWTH[name]) + rnd.randint(200, 800)
        if prompt > 140_000:
            summary = rnd.randint(11_000, 17_000)
            t += timedelta(seconds=rnd.randint(20, 40))
            lines.append({"type": "system", "subtype": "compact_boundary", "timestamp": ts(t), **base, "isMeta": False,
                          "compactMetadata": {"trigger": "auto", "preTokens": prompt, "postTokens": summary}})
            lines.append({"type": "user", "timestamp": ts(t), **base, "isCompactSummary": True,
                          "message": {"role": "user", "content": "Summary of the session so far."}})
            prompt, cached = floor + summary, 0
    (proj / f"{CLAUDE_ID}.jsonl").write_text("".join(json.dumps(l) + "\n" for l in lines))

    sub_dir = proj / CLAUDE_ID / "subagents"
    sub_dir.mkdir(parents=True, exist_ok=True)
    for agent, kind, spawned_by, start in subagents:
        (sub_dir / f"agent-{agent}.meta.json").write_text(json.dumps({"agentType": kind, "toolUseId": spawned_by, "spawnDepth": 1}))
        sb = {**base, "isSidechain": True, "agentId": agent}
        out, at, p, c = [], start, 14_000, 0
        steps = 9 if kind == "Explore" else 7
        for i in range(steps):
            at += timedelta(seconds=rnd.randint(3, 7))
            tid = f"toolu_{agent}_{i}"
            tool = ["Grep", "Read", "Read", "Grep", "Bash"][i % 5]
            usage = {"input_tokens": 4, "cache_read_input_tokens": c, "cache_creation_input_tokens": p - c - 4, "output_tokens": rnd.randint(100, 500)}
            last = i == steps - 1
            out.append({"type": "assistant", "timestamp": ts(at), **sb,
                        "message": {"id": f"msg_{tid}", "model": "claude-sonnet-5", "stop_reason": "end_turn" if last else "tool_use",
                                    "usage": usage, "content": [] if last else [{"type": "tool_use", "id": tid, "name": tool}]}})
            if not last:
                at += timedelta(seconds=rnd.randint(1, 3))
                out.append({"type": "user", "timestamp": ts(at), **sb,
                            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid}]}})
            c, p = p, p + rnd.randint(*GROWTH[tool])
        (sub_dir / f"agent-{agent}.jsonl").write_text("".join(json.dumps(l) + "\n" for l in out))


def codex(home, rnd, end):
    cwd = "/home/dev/lab/notebooks"
    t = end - timedelta(minutes=18)
    day = home / ".codex/sessions" / t.strftime("%Y/%m/%d")
    day.mkdir(parents=True, exist_ok=True)
    lines = [
        {"timestamp": ts(t), "type": "session_meta", "payload": {"id": CODEX_ID, "cwd": cwd, "cli_version": "0.158.0", "originator": "codex-tui"}},
        {"timestamp": ts(t), "type": "turn_context", "payload": {"model": "gpt-5.6-sol", "cwd": cwd}},
        {"timestamp": ts(t), "type": "event_msg", "payload": {"type": "task_started"}},
        {"timestamp": ts(t), "type": "response_item", "payload": {"type": "message", "role": "user"}},
    ]
    prompt, total_in, total_out = 24_000, 0, 0
    for n in range(1, 71):
        t += timedelta(seconds=rnd.randint(6, 14))
        call = f"call_demo_{n:04d}"
        out = rnd.randint(200, 1_200)
        total_in += prompt
        total_out += out
        usage = lambda i, o: {"input_tokens": i, "cached_input_tokens": int(i * 0.9), "output_tokens": o, "reasoning_output_tokens": o // 4, "total_tokens": i + o}
        lines.append({"timestamp": ts(t), "type": "response_item", "payload": {"type": "function_call", "call_id": call, "name": "exec_command"}})
        lines.append({"timestamp": ts(t), "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": usage(total_in, total_out), "last_token_usage": usage(prompt, out), "model_context_window": 258_400}}})
        t += timedelta(seconds=rnd.randint(1, 4))
        lines.append({"timestamp": ts(t), "type": "response_item", "payload": {"type": "function_call_output", "call_id": call}})
        prompt += rnd.randint(1_500, 6_500)
        if prompt > 225_000:
            prompt = rnd.randint(52_000, 60_000)
    (day / f"rollout-{t.strftime('%Y-%m-%dT%H-%M-%S')}-{CODEX_ID}.jsonl").write_text("".join(json.dumps(l) + "\n" for l in lines))


def main():
    home = Path(sys.argv[1])
    rnd = random.Random(110)
    end = datetime.now(timezone.utc) - timedelta(minutes=1)
    claude(home, rnd, end)
    codex(home, rnd, end - timedelta(minutes=3))
    print(f"wrote invented transcripts under {home}")


if __name__ == "__main__":
    main()
