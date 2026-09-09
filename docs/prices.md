# Prices

Prices are data, not code. The table shipped in the binary lives in [`crates/agent-top-core/prices.toml`](https://github.com/kannandreams/agent-top/blob/main/crates/agent-top-core/prices.toml) and carries the vendors' published list prices for Anthropic, OpenAI and Google models. `agent-top --prices` prints the table in use, with the source of every row.

## Overriding a price

Write a file of your own at `~/.config/agent-top/prices.toml`. It is merged over the built-in table at startup, one model at a time, so list only what you want to change.

```toml
# ~/.config/agent-top/prices.toml   (USD per million tokens)

[[model]]
prefix = "gpt-5-codex"
input = 1.25
output = 10.0
cache_read = 0.125
```

- An entry whose `prefix` matches a built-in one replaces it, so a stale price can be corrected without waiting for a release.
- A new prefix is added, which is how a model the project does not ship a price for gets costed at all.
- Cache writes default to Anthropic's multipliers of the input price (1.25x for the five-minute TTL, 2x for the hour) and can be set explicitly with `cache_write_5m` and `cache_write_1h`.

`--prices` marks the rows that came from your file, and the detail pane says "your price file" next to a cost that used one. A price file that cannot be parsed is reported on stderr and ignored; the built-in prices still apply.

## How a model finds its price

The longest matching prefix wins, so `claude-fable-5-1` beats `claude-fable-5`, and a date-suffixed id like `claude-sonnet-4-6-20251114` resolves to its base model. A Codex model with no entry of its own resolves to its base model the same way.

A model with no entry anywhere is never guessed at. Its tokens are counted and reported as unpriced, and any total containing them carries a `+` so an incomplete figure is never read as a cheap one. `--prices` is the quickest way to find out why something shows `n/a`.

## Per-call charges

Web searches are billed per search, on top of tokens, and the rate is in the same file under `[server_tools]`. Codex and Gemini web searches are counted but not priced, because OpenAI's and Google's rates are not in the table.

Gemini models are priced at Google's paid-tier rate for prompts under 200k tokens, with thinking tokens counted as output the way Google bills them. A Gemini CLI signed in with a Google account rather than an API key is on a free quota, so its cost here is what the same session would have cost at list price, not a bill.

OpenCode computes its own cost and agent-top reports that figure rather than repricing it.
