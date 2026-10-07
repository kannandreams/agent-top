---
description: "agent-top serve --listen receives OpenTelemetry spans from any agent, including one in a container, and keeps them in the local store beside the coding-agent sessions, priced from the same table."
---

# Receive telemetry

An agent you build with an SDK and run yourself writes no transcript agent-top knows. If it emits OpenTelemetry spans, `agent-top serve --listen` receives them and keeps them in the [local store](index.md), where `report` and `sql` count them with the coding-agent sessions.

```sh
agent-top serve --listen 127.0.0.1:4318
```

```text
2026-10-06T19:39:28Z syncing /Users/you/.local/share/agent-top/agent-top.db every 60s
2026-10-06T19:39:28Z receiving OTLP spans at http://127.0.0.1:4318/v1/traces
2026-10-06T19:39:29Z received 3 spans (3 new) for 1 session
```

No port is opened without `--listen`, and there is no default address, config key or environment variable that opens one. Bind to `127.0.0.1` unless the sender is on another machine; any process that can reach the port can send spans.

## Pointing an agent at it

Any OpenTelemetry SDK with an OTLP/HTTP exporter works. With the standard environment variables:

```sh
export OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://127.0.0.1:4318/v1/traces
export OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf   # or http/json
```

Instrumentation libraries that follow the GenAI semantic conventions, such as OpenLLMetry and OpenInference, set the attributes agent-top reads.

## From a container

**Docker Desktop for Mac:** a container reaches a listener on the host's `127.0.0.1` at `http://host.docker.internal:4318/v1/traces`. Nothing else is needed.

**Linux:** a container on the default bridge network cannot reach the host's `127.0.0.1`. Either share the host's network:

```sh
agent-top serve --listen 127.0.0.1:4318
docker run --network host -e OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://127.0.0.1:4318/v1/traces my-agent
```

or bind the listener to the bridge's gateway address, which containers can reach and other machines cannot:

```sh
agent-top serve --listen "$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}'):4318"
docker run --add-host=host.docker.internal:host-gateway \
  -e OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://host.docker.internal:4318/v1/traces my-agent
```

The gateway is usually `172.17.0.1`. These three cases (host network, gateway listener, and a loopback listener that a bridged container cannot reach) are checked on every change by `scripts/container-reachability.sh` in CI on Ubuntu.

## What it accepts

| Request | Answer |
|---|---|
| `POST /v1/traces`, `application/x-protobuf` or `application/json` | stored, `200` |
| `POST /v1/metrics`, `POST /v1/logs` | `200`, not stored |
| a body over 4 MiB | `413`; the exporter retries in smaller batches |
| a compressed body | `415`; set the exporter's compression to none |
| a body that does not decode | `400` |

A batch sent twice, as an exporter retrying does, changes nothing: each span is kept once by its trace and span id.

## What it reads from a span

| Attribute | Used for |
|---|---|
| `service.name` | the session's project |
| `gen_ai.conversation.id` | the session; without it, the trace id |
| `gen_ai.operation.name` | the span's kind: `chat`, `generate_content`, `text_completion` are inferences, `execute_tool` is a tool call, `invoke_agent` and `invoke_workflow` are turns |
| `gen_ai.tool.name` | a tool call's name |
| `gen_ai.response.model`, else `gen_ai.request.model` | the price |
| `gen_ai.usage.input_tokens`, `output_tokens`, `cache_read.input_tokens`, `cache_write.input_tokens` | tokens and cost |

The names are those of the GenAI semantic conventions at schema `gen-ai-dev/1.42.0-dev`, where `input_tokens` includes the cached tokens; agent-top subtracts them to get the fresh input. The older `prompt_tokens` and `completion_tokens` are read too. Tokens come from inference spans; an agent span counts only in a session that sent no inference span with usage, so totals an agent span repeats are not counted twice. Cost comes from the same [price table](../prices.md) as everything else, and a model not in it is unpriced.

Every other attribute is dropped as the request is decoded: prompts, completions, system instructions, tool arguments and tool results never reach the store. Span events are not decoded.

A received session is stored with `attribution = 'telemetry'`, the harness `otel` (or the `agent_top.harness` a sender sets), and the session id `<service.name>/<conversation>`. `report` counts it and says how many sessions it received. The spans are in the `telemetry_spans` table; see the [schema](schema.md).
