---
description: "Estimating the CO2e and energy of your coding agents' tokens: what providers and researchers have published, why agent-top ships no figure of its own, and how to write ~/.config/agent-top/energy.toml."
---

# Energy and CO2e

`agent-top report --energy` estimates the carbon (CO2e) and energy behind your agents' tokens, by model, harness, project or day. The estimate comes from a table of coefficients that you write. agent-top ships none, because no provider publishes energy per token for the models it prices.

```sh
agent-top report --energy                 # last 30 days, by harness
agent-top report --energy --by model --since 7d
agent-top report --energy --json          # co2e_g and energy_wh as {low, high}
```

Without a table, `--energy` prints the exact activity every estimate is built from: output tokens, uncached prompt tokens (fresh input and cache writes), and tokens read from cache.

## What has been published

Read 2026-11-28. None of these is a measurement of a Claude or GPT model on agent work.

| Source | Figure | What it covers |
|---|---|---|
| Anthropic | none | No energy, emissions or water figure per request or per token. |
| OpenAI ([June 2025](https://blog.samaltman.com/the-gentle-singularity)) | 0.34 Wh per average ChatGPT query | No method, no prompt length. |
| Google ([arXiv 2508.15734](https://arxiv.org/abs/2508.15734)) | 0.24 Wh and 0.03 g CO2e per median Gemini Apps text prompt, May 2025 | Accelerators, host CPU and memory, idle reserved machines, data-centre overhead (PUE 1.09). Market-based emissions. No per-token figure, no prompt length. |
| Mistral ([July 2025](https://mistral.ai/news/our-contribution-to-a-global-environmental-standard-for-ai)) | 1.14 g CO2e per 400-token Le Chat response | A life-cycle study including server manufacturing. No Wh figure. |
| [EcoLogits](https://ecologits.ai/latest/methodology/llm_inference/) | GPU energy per output token from the model's active parameters, times the GPUs needed to hold it, times PUE | A model of inference, not a measurement. Input and cache tokens do not appear. Closed models' sizes are estimates; for Claude, only Claude 3 Opus has one (about 2T parameters, 200B to 600B active). |
| [A Claude Code estimate](https://gist.github.com/mdodkins/9b49624855cc41570c9d1012e0d5d157) (March 2026) | 390 Wh per million input, 490 cache write, 39 cache read, 1,950 output | Scales energy with price. Covers every kind of token. No stated uncertainty. |

The per-prompt figures describe a chat prompt of a few hundred tokens. A coding agent's response re-reads its whole context, often 50k to 900k tokens (see [Reading the context timeline](context-timeline.md)), so they do not transfer.

## What they give on agent work

An invented month of agent work: 15,000 responses, 5B tokens read from cache, 2M fresh input, 38M written to cache, 12M output.

| Method | What it scales with | Energy | CO2e at 94 to 420 g/kWh |
|---|---|---:|---:|
| Per prompt (0.24 to 0.34 Wh) | responses | 3.6 to 5.1 kWh | 0.3 to 2.1 kg |
| EcoLogits, Claude 3 Opus size | output tokens | 121 to 336 kWh | 11 to 141 kg |
| Price-scaled | every kind of token | 238 kWh | 22 to 100 kg |

The two token methods agree within a factor of three, for different reasons. In the price-scaled method, cache reads are 195 kWh, 82% of the total. EcoLogits leaves cache reads out and gets its total from output tokens on 60 to 64 GPUs. Nobody has published a measurement of what a cache read costs.

## Writing the table

Put it at `~/.config/agent-top/energy.toml`, or point `AGENT_TOP_ENERGY` at a file. Figures are watt-hours per million tokens. Any figure can be one number or a `[low, high]` range, and ranges carry through to the result.

```toml
[grid]
gco2e_per_kwh = [94, 420]
source = "Google 2024 market-based (arXiv 2508.15734) to a US grid average"
checked = "2026-11-28"

[models."claude-opus-5-5"]
input = [0, 390]
cache_write = [0, 490]
cache_read = [0, 39]
output = [1950, 28000]
source = "low: price-scaled output only; high: price-scaled input and cache, EcoLogits output at Claude 3 Opus size"
checked = "2026-11-28"
```

- `[grid]` is required, and every entry needs a `source` and a `checked` date as `YYYY-MM-DD`. A file that breaks either rule is refused whole, with a message naming the entry.
- Model ids match exactly, as the report prints them. `claude-opus-5-5` does not cover `claude-opus-5`. Tokens on a model with no entry are listed as unestimated and left out of the estimate.
- CO2e's low bound uses the lowest energy at the cleanest grid figure, and its high bound the highest at the dirtiest.

### Entries for each method

Copy the one whose assumptions you accept, and change the `source` line to say so.

**Price-scaled**, from the March 2026 estimate:

```toml
input = 390
cache_write = 490
cache_read = 39
output = 1950
```

**EcoLogits at Claude 3 Opus size**, output only:

```toml
input = 0
cache_write = 0
cache_read = 0
output = [10100, 28000]
```

These come from EcoLogits' fit, 1.17×10⁻⁶ · e^(−0.0112·64) · P + 4.05×10⁻⁵ Wh per output token per GPU, with P the active parameters in billions:

- At 200B active, 1.55×10⁻⁴ Wh, times 60 GPUs and a PUE of 1.09, gives 10,100 Wh per million.
- At 600B active, 3.83×10⁻⁴ Wh, times 64 GPUs and a PUE of 1.14, gives 28,000 Wh per million.

The server's own power outside the GPUs is left out. Applying a Claude 3 Opus size to another model is a guess about that model.

**Grid:** 94 g CO2e per kWh is Google's 2024 market-based figure, which counts its clean-energy purchases. The March 2026 estimate uses 420 for a US grid average. Use your provider's region if you know it.

## In the JSON

`agent-top report --json` always carries `output_tokens` and `uncached_prompt_tokens` per group. With `--energy` and a usable table, each group also has:

```json
"estimate": {
  "estimated": true,
  "co2e_g": { "low": 2200.0, "high": 231000.0 },
  "energy_wh": { "low": 23400.0, "high": 550400.0 },
  "cache_read_wh": { "low": 0.0, "high": 195000.0 },
  "estimated_tokens": 5052000000,
  "unestimated_tokens": 0
}
```

and the top level has `energy_table` with the path, the grid and every entry with its source.

## Limits

- A session is estimated with its own model. Subagent tokens are counted in their session without a split by model, so a subagent on another model is estimated with the session model's figures.
- Training, the hardware's manufacture, and water are outside the estimate, unless the source you copied includes them.
- The live view and `agent-top ui` do not show the estimate yet.
