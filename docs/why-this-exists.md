# Why agent-top exists

I run several coding agents a day. Claude Code and Codex for most of the work, and OpenCode with the DeepSeek API where cost matters more than speed. I stopped using editor plugins altogether; the terminal is where the agents live now.

That leaves a gap. Each harness shows its own session in its own window: what it is doing, and roughly what it has used. None of them shows the others, and none of them shows the machine underneath: the subagents, the MCP servers, the helper processes, the memory they hold, or what the whole day has cost across all of them. When something goes wrong, a loop that burns tokens, a server that will not die, a session that stalls, I find out late, and I find out by looking in several places.

I wanted one screen for that. The metadata already exists. Every harness writes a transcript with usage records, and the operating system keeps the process table. agent-top reads both and puts the key numbers in one place: who is running and who is waiting, tokens and cost, cache hit rate, tool calls and failures, process trees and leaks, rate limits, and a report of what it all cost over time. It is the view you expect from production monitoring, applied to local coding agents.

That is the direction: observability for the agents on your machine. Metrics, cost and the cross-harness report are there today. Advice on bad deals, failed-tool and slowest-tool views, and rate-limit warnings came next. Alerts, error tracking and a fuller FinOps history are where it goes from here. Throughout, it stays read-only, sends nothing anywhere, and ships as one binary.

The longer view of what agent-top is and is not is in [Vision](vision.md).
