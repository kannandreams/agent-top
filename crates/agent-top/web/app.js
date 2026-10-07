// agent-top ui: reads the JSON API and draws the page. No dependencies.
// Every label from the store is set with textContent, never as HTML.
"use strict";

const DAY = 86400000;
// Fixed colour slot per harness, so a colour always means the same harness.
// A harness not listed shares the last slot under "other".
const HARNESS_SLOTS = ["claude", "codex", "opencode", "gemini", "kodelet", "otel"];
const state = { range: "30", harness: "" };

// ---- small helpers -------------------------------------------------------

function el(tag, attrs, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v);
  }
  for (const k of kids) if (k != null) e.append(k);
  return e;
}

function svg(tag, attrs) {
  const e = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [k, v] of Object.entries(attrs || {})) e.setAttribute(k, v);
  return e;
}

function slot(h) {
  const i = HARNESS_SLOTS.indexOf(h);
  return i < 0 ? "var(--h6)" : `var(--h${i + 1})`;
}

function harnessLabel(h) {
  return HARNESS_SLOTS.includes(h) ? h : "other";
}

const usd = (v) => {
  const n = Number(v) || 0;
  if (n >= 1000) return "$" + n.toLocaleString("en-US", { maximumFractionDigits: 0 });
  return "$" + n.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
};
const compact = (v) => {
  const n = Number(v) || 0;
  for (const [d, s] of [[1e9, "B"], [1e6, "M"], [1e3, "K"]]) if (n >= d) return (n / d).toFixed(n / d >= 100 ? 0 : 1) + s;
  return String(Math.round(n));
};
const dur = (ms) => {
  if (ms == null) return "open";
  if (ms < 1000) return ms + " ms";
  const s = ms / 1000;
  if (s < 60) return s.toFixed(s < 10 ? 1 : 0) + " s";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${String(Math.round(s % 60)).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, "0")}m`;
};
const when = (ms) => (ms ? new Date(ms).toISOString().replace("T", " ").slice(0, 16) + " UTC" : "-");

async function api(path, params) {
  const q = new URLSearchParams(params || {});
  const r = await fetch(`/api/${path}?${q}`);
  if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text()}`);
  return r.json();
}

function filterParams() {
  const days = Number(state.range);
  return { since: days ? Date.now() - days * DAY : 0, harness: state.harness };
}

// ---- tooltip -------------------------------------------------------------

const tip = () => document.getElementById("tip");
function showTip(evt, value, rows) {
  const t = tip();
  t.replaceChildren(el("div", { class: "tv", text: value }), ...rows.map(([color, text]) => {
    const lk = el("span", { class: "lk" });
    if (color) lk.style.background = color;
    return el("div", { class: "tr" }, lk, el("span", { text }));
  }));
  t.hidden = false;
  const r = evt.target.getBoundingClientRect();
  const x = evt.clientX ?? r.left + r.width / 2;
  const y = evt.clientY ?? r.top;
  const w = t.offsetWidth, h = t.offsetHeight;
  t.style.left = Math.min(window.innerWidth - w - 8, Math.max(8, x + 12)) + "px";
  t.style.top = Math.max(8, y - h - 12) + "px";
}
const hideTip = () => { tip().hidden = true; };

function hover(node, value, rows) {
  node.setAttribute("tabindex", "0");
  node.addEventListener("pointermove", (e) => showTip(e, value, rows));
  node.addEventListener("pointerleave", hideTip);
  node.addEventListener("focus", (e) => showTip(e, value, rows));
  node.addEventListener("blur", hideTip);
}

// ---- tables --------------------------------------------------------------

function table(cols, rows, onRow) {
  const head = el("tr", {}, ...cols.map((c) => el("th", { class: c.num ? "num" : "", text: c.label })));
  const body = rows.map((r) => {
    const tr = el("tr", {}, ...cols.map((c) => {
      const v = c.render ? c.render(r) : r[c.key];
      const td = el("td", { class: [c.num ? "num" : "", c.cls || ""].join(" ").trim() });
      if (v instanceof Node) td.append(v); else td.textContent = v == null ? "" : String(v);
      return td;
    }));
    if (onRow) {
      tr.className = "click";
      tr.tabIndex = 0;
      tr.addEventListener("click", () => onRow(r));
      tr.addEventListener("keydown", (e) => { if (e.key === "Enter") onRow(r); });
    }
    return tr;
  });
  return el("div", { class: "table-wrap" }, el("table", {}, el("thead", {}, head), el("tbody", {}, ...body)));
}

// ---- tiles ---------------------------------------------------------------

function tiles(t) {
  const box = document.getElementById("tiles");
  const hit = t.prompt > 0 ? Math.round((t.cache_read / t.prompt) * 100) + "%" : "-";
  const days = Number(state.range);
  const tile = (label, value, sub, hero) =>
    el("div", { class: "tile" + (hero ? " hero" : "") }, el("div", { class: "label", text: label }),
      el("div", { class: "value", text: value }), sub ? el("div", { class: "sub", text: sub }) : null);
  box.replaceChildren(
    tile("Cost", usd(t.cost_usd) + (t.unpriced_tokens > 0 ? "+" : ""), days ? `last ${days} days` : "all time", true),
    tile("Sessions", Number(t.sessions).toLocaleString("en-US"), `${compact(t.turns)} turns`),
    tile("Tokens", compact(t.tokens), `${compact(t.tool_calls)} tool calls`),
    tile("Cache hit", hit, "of the prompt"),
    t.unpriced_tokens > 0 ? tile("Unpriced tokens", compact(t.unpriced_tokens), "no price in the table") : null,
  );
}

// ---- cost by day: stacked columns ----------------------------------------

// A tick step of 1, 2 or 5 times a power of ten, so four or five gridlines
// land on round values.
function niceStep(max) {
  const raw = Math.max(max, 0.01) / 4;
  const p = Math.pow(10, Math.floor(Math.log10(raw)));
  for (const m of [1, 2, 5, 10]) if (m * p >= raw) return m * p;
  return 10 * p;
}

function daily(rows) {
  const box = document.getElementById("chart-daily");
  const tableBox = document.getElementById("table-daily");
  const legend = document.getElementById("legend-daily");
  if (!rows.length) {
    box.replaceChildren(el("div", { class: "empty", text: "No sessions in this range." }));
    legend.replaceChildren(); tableBox.replaceChildren();
    return;
  }
  // Every day in the range, so gaps show as gaps.
  const first = Date.parse(rows[0].day + "T00:00:00Z");
  const last = Date.parse(rows[rows.length - 1].day + "T00:00:00Z");
  const days = [];
  for (let d = first; d <= last; d += DAY) days.push(new Date(d).toISOString().slice(0, 10));
  const harnesses = [...new Set(rows.map((r) => harnessLabel(r.harness)))].sort((a, b) => {
    const ia = HARNESS_SLOTS.indexOf(a), ib = HARNESS_SLOTS.indexOf(b);
    return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
  });
  const by = new Map();
  for (const r of rows) {
    const k = r.day + "|" + harnessLabel(r.harness);
    const prev = by.get(k) || { cost: 0, sessions: 0 };
    by.set(k, { cost: prev.cost + (r.cost_usd || 0), sessions: prev.sessions + r.sessions });
  }
  const totals = days.map((d) => harnesses.reduce((s, h) => s + ((by.get(d + "|" + h) || {}).cost || 0), 0));
  const stepV = niceStep(Math.max(...totals));
  const ticks = Math.max(1, Math.ceil(Math.max(...totals) / stepV));
  const max = stepV * ticks;

  const W = Math.max(320, box.clientWidth || 800), H = 260, L = 56, R = 8, T = 12, B = 28;
  const band = (W - L - R) / days.length;
  const bw = Math.max(2, Math.min(24, band - 2));
  const y = (v) => T + (H - T - B) * (1 - v / max);
  const root = svg("svg", { viewBox: `0 0 ${W} ${H}`, role: "img", "aria-label": "Cost by day, stacked by harness" });
  const grid = svg("g", { class: "grid" }), axis = svg("g", { class: "axis" });
  for (let i = 0; i <= ticks; i++) {
    const v = stepV * i, yy = y(v);
    if (i > 0) grid.append(svg("line", { x1: L, x2: W - R, y1: yy, y2: yy }));
    const t = svg("text", { x: L - 8, y: yy + 4, "text-anchor": "end" });
    t.textContent = v === 0 ? "$0" : v >= 1 ? "$" + v.toLocaleString("en-US", { maximumFractionDigits: 0 }) : usd(v);
    axis.append(t);
  }
  const step = Math.ceil(days.length / Math.max(1, Math.floor((W - L - R) / 72)));
  days.forEach((d, i) => {
    if (i % step) return;
    const t = svg("text", { x: L + band * i + band / 2, y: H - 8, "text-anchor": "middle" });
    t.textContent = d.slice(5);
    axis.append(t);
  });
  root.append(grid, axis);
  const marks = svg("g", {});
  days.forEach((d, i) => {
    let acc = 0;
    const x = L + band * i + (band - bw) / 2;
    const present = harnesses.filter((h) => (by.get(d + "|" + h) || {}).cost > 0);
    present.forEach((h, j) => {
      const v = by.get(d + "|" + h);
      const y0 = y(acc), y1 = y(acc + v.cost);
      acc += v.cost;
      // 2px surface gap between stacked segments.
      const gap = j > 0 ? 2 : 0;
      const hgt = Math.max(1, y0 - y1 - gap);
      const top = j === present.length - 1;
      const r = top ? Math.min(4, hgt / 2, bw / 2) : 0;
      const yTop = y1, yBot = y1 + hgt;
      const path = svg("path", {
        class: "seg-mark",
        d: `M${x},${yBot} V${yTop + r} Q${x},${yTop} ${x + r},${yTop} H${x + bw - r} Q${x + bw},${yTop} ${x + bw},${yTop + r} V${yBot} Z`,
      });
      path.style.fill = slot(h);
      hover(path, usd(v.cost), [[slot(h), `${h}, ${v.sessions} sessions`], [null, `${d} total ${usd(totals[i])}`]]);
      marks.append(path);
    });
  });
  root.append(marks, svg("line", { class: "base", x1: L, x2: W - R, y1: y(0), y2: y(0) }));
  box.replaceChildren(root);

  legend.replaceChildren(...harnesses.map((h) => {
    const i = el("i"); i.style.background = slot(h);
    return el("span", {}, i, h);
  }));
  const trows = days.filter((d, i) => totals[i] > 0).reverse().map((d) => {
    const r = { day: d, total: usd(totals[days.indexOf(d)]) };
    for (const h of harnesses) r[h] = usd((by.get(d + "|" + h) || {}).cost || 0);
    return r;
  });
  tableBox.replaceChildren(table([{ key: "day", label: "Day" }, ...harnesses.map((h) => ({ key: h, label: h, num: true })), { key: "total", label: "Total", num: true }], trows));
}

// ---- horizontal bars -------------------------------------------------------

function bars(id, rows) {
  const box = document.getElementById(id);
  if (!rows.length) { box.replaceChildren(el("div", { class: "empty", text: "No sessions in this range." })); return; }
  const max = Math.max(...rows.map((r) => r.cost_usd || 0), 0.0001);
  box.replaceChildren(...rows.map((r) => {
    const fill = el("div", { class: "fill" });
    fill.style.width = Math.max(0.5, ((r.cost_usd || 0) / max) * 100) + "%";
    const priced = r.unpriced_tokens > 0 ? "+" : "";
    const row = el("div", { class: "row" }, el("div", { class: "name", text: r.name, title: r.name }),
      el("div", { class: "track" }, el("div", { class: "lane" }, fill), el("span", { class: "val", text: usd(r.cost_usd) + priced })));
    hover(row, usd(r.cost_usd) + priced, [[null, r.name], [null, `${compact(r.tokens)} tokens, ${r.sessions} sessions`]]);
    return row;
  }));
}

// ---- tools, mcp, sessions ----------------------------------------------------

function tools(rows) {
  const box = document.getElementById("tools");
  if (!rows.length) { box.replaceChildren(el("div", { class: "empty", text: "No finished tool calls in this range." })); return; }
  const max = Math.max(...rows.map((r) => r.p95_ms || 0), 1);
  const latBar = (v, cls) => {
    const s = el("span", { class: "lat" + (cls ? " " + cls : "") });
    s.style.width = Math.max(2, (v / max) * 120) + "px";
    return el("span", {}, s, dur(v));
  };
  box.replaceChildren(table([
    { key: "name", label: "Tool", cls: "mono trunc" },
    { key: "calls", label: "Calls", num: true, render: (r) => Number(r.calls).toLocaleString("en-US") },
    { key: "errors", label: "Errors", num: true, render: (r) => r.errors ? el("span", { class: "err", text: `${r.errors} (${((r.errors / r.calls) * 100).toFixed(1)}%)` }) : "0" },
    { key: "p50_ms", label: "p50", render: (r) => latBar(r.p50_ms) },
    { key: "p95_ms", label: "p95", render: (r) => latBar(r.p95_ms, "p95") },
    { key: "max_ms", label: "Max", num: true, render: (r) => dur(r.max_ms) },
  ], rows));
}

function mcp(rows) {
  const card = document.getElementById("mcp-card");
  card.hidden = !rows.length;
  if (!rows.length) return;
  document.getElementById("mcp").replaceChildren(table([
    { key: "server", label: "Server", cls: "mono" },
    { key: "sessions", label: "Sessions", num: true },
    { key: "calls", label: "Calls", num: true },
    { key: "errors", label: "Errors", num: true, render: (r) => r.errors ? el("span", { class: "err", text: String(r.errors) }) : "0" },
    { key: "last_call_at", label: "Last call", render: (r) => when(r.last_call_at) },
  ], rows));
}

function sessions(rows) {
  const box = document.getElementById("sessions");
  if (!rows.length) { box.replaceChildren(el("div", { class: "empty", text: "No sessions in this range." })); return; }
  box.replaceChildren(table([
    { key: "last_activity", label: "Last activity", render: (r) => when(r.last_activity) },
    { key: "harness", label: "Harness", render: (r) => { const k = el("span", { class: "key" }); k.style.background = slot(r.harness); return el("span", {}, k, " " + r.harness); } },
    { key: "project", label: "Project", cls: "mono trunc" },
    { key: "model", label: "Model", cls: "mono trunc" },
    { key: "turns", label: "Turns", num: true },
    { key: "tokens", label: "Tokens", num: true, render: (r) => compact(r.tokens) },
    { key: "cost_usd", label: "Cost", num: true, render: (r) => usd(r.cost_usd) + (r.unpriced_tokens > 0 ? "+" : "") },
    { key: "attribution", label: "Source", render: (r) => el("span", { class: "badge", text: r.attribution }) },
  ], rows, openSession));
}

// ---- session detail: facts and span waterfall --------------------------------

const KIND = { turn: "var(--turn)", inference: "var(--h1)", tool: "var(--h2)" };

async function openSession(r) {
  const link = `#s/${encodeURIComponent(r.harness)}/${encodeURIComponent(r.session_id)}`;
  if (location.hash !== link) history.replaceState(null, "", link);
  const card = document.getElementById("detail");
  const body = document.getElementById("detail-body");
  card.hidden = false;
  body.replaceChildren(el("div", { class: "empty", text: "Loading..." }));
  card.scrollIntoView({ behavior: "smooth", block: "start" });
  let d;
  try { d = await api("session", { harness: r.harness, id: r.session_id }); } catch (e) {
    body.replaceChildren(el("div", { class: "empty", text: String(e.message || e) }));
    return;
  }
  const s = d.session;
  document.getElementById("h-detail").textContent = "Session";
  const fact = (k, v) => el("div", {}, el("div", { class: "k", text: k }), el("div", { class: "v", text: v == null || v === "" ? "-" : String(v) }));
  const facts = el("div", { class: "facts" },
    fact("session id", s.session_id),
    fact("harness", `${s.harness}${s.harness_version ? " " + s.harness_version : ""}`), fact("project", s.project), fact("model", s.model),
    fact("cost", usd(s.cost_usd) + (s.unpriced_tokens > 0 ? `+ (${compact(s.unpriced_tokens)} unpriced)` : "")),
    fact("tokens", `${compact(s.tokens)} (in ${compact(s.input)}, cache read ${compact(s.cache_read)}, cache write ${compact(s.cache_write_5m + s.cache_write_1h + s.cache_write_unsplit)}, out ${compact(s.output)})`),
    fact("turns / tools", `${s.turns} / ${s.tool_calls}`), fact("started", when(s.started_at)), fact("last activity", when(s.last_activity)),
    fact("source", s.attribution), s.harness_cost_usd != null ? fact("harness's own figure", usd(s.harness_cost_usd)) : null,
    s.parent_session_id ? fact("parent session", s.parent_session_id) : null, fact("cwd", s.cwd));
  const parts = [facts, turnsView(d.spans)];
  if (d.mcp.length) parts.push(el("h2", { text: "MCP servers" }), table([
    { key: "server", label: "Server", cls: "mono" }, { key: "calls", label: "Calls", num: true }, { key: "errors", label: "Errors", num: true },
    { key: "last_call_at", label: "Last call", render: (m) => when(m.last_call_at) }], d.mcp));
  body.replaceChildren(...parts);
}

// A session can run for days, so one waterfall over all of it squashes every
// span to a sliver. The session is shown as its turns; a turn's spans get the
// waterfall, on that turn's own time axis.
function turnsView(spans) {
  const wrap = el("div", {});
  if (!spans.length) { wrap.append(el("div", { class: "empty", text: "No spans stored for this session." })); return wrap; }
  const turns = spans.filter((s) => s.kind === "turn");
  const inTurn = new Map(turns.map((t) => [t.seq, []]));
  const loose = [];
  for (const s of spans) {
    if (s.kind === "turn") continue;
    if (s.parent_seq != null && inTurn.has(s.parent_seq)) inTurn.get(s.parent_seq).push(s); else loose.push(s);
  }
  const groups = turns.map((t, i) => ({ label: `turn ${i + 1}`, turn: t, spans: inTurn.get(t.seq) }));
  if (loose.length) groups.push({ label: "outside a turn", turn: null, spans: loose });
  const rows = groups.slice().reverse().slice(0, 100).map((g) => {
    const start = g.turn ? g.turn.started_at : Math.min(...g.spans.map((s) => s.started_at));
    return {
      g, label: g.label, start, dur: g.turn ? g.turn.duration_ms : null,
      tools: g.spans.filter((s) => s.kind === "tool").length,
      inferences: g.spans.filter((s) => s.kind === "inference").length,
      errors: g.spans.filter((s) => s.error).length,
    };
  });
  const chart = el("div", { class: "waterfall" });
  const pick = (r) => chart.replaceChildren(el("h2", { text: `${r.label}, ${when(r.start)}` }), waterfall(r.g.turn, r.g.spans));
  wrap.append(
    el("h2", { text: `Turns (${turns.length})` }),
    table([
      { key: "label", label: "Turn", cls: "mono" },
      { key: "start", label: "Started", render: (r) => when(r.start) },
      { key: "dur", label: "Duration", num: true, render: (r) => (r.g.turn ? dur(r.dur) : "-") },
      { key: "inferences", label: "Inferences", num: true },
      { key: "tools", label: "Tool calls", num: true },
      { key: "errors", label: "Errors", num: true, render: (r) => (r.errors ? el("span", { class: "err", text: String(r.errors) }) : "0") },
    ], rows, pick),
    chart,
  );
  // The newest finished turn; a running one has nothing to draw yet.
  const first = rows.find((r) => r.g.turn && r.g.turn.duration_ms != null && r.g.spans.length) || rows[0];
  if (first) pick(first);
  return wrap;
}

function waterfall(turn, inner) {
  const wrap = el("div", {});
  const spans = (turn ? [turn] : []).concat(inner.slice().sort((a, b) => a.started_at - b.started_at));
  if (!spans.length) { wrap.append(el("div", { class: "empty", text: "No spans in this turn." })); return wrap; }
  const shown = spans.slice(0, 300);
  const t0 = Math.min(...shown.map((s) => s.started_at));
  const t1 = Math.max(...shown.map((s) => s.started_at + (s.duration_ms || 0)));
  const total = Math.max(1, t1 - t0);
  // The card's content width: main, less the card's padding and border.
  const ROW = 18, L = 200, T = 22, W = Math.max(560, document.getElementById("detail-body").clientWidth || 900), H = shown.length * ROW + T + 4;
  // Room on the right for the duration printed after the longest bar.
  const x = (t) => L + ((t - t0) / total) * (W - L - 72);
  const root = svg("svg", { width: W, height: H, viewBox: `0 0 ${W} ${H}`, role: "img", "aria-label": "Span waterfall for one turn" });
  // Time axis: five ticks from the turn's start.
  for (let i = 0; i <= 4; i++) {
    const tt = t0 + (total / 4) * i, xx = x(tt);
    root.append(svg("line", { x1: xx, x2: xx, y1: T - 4, y2: H, stroke: "currentColor", "stroke-opacity": "0.12" }));
    const lab = svg("text", { x: xx, y: 12, "text-anchor": i === 0 ? "start" : i === 4 ? "end" : "middle" });
    lab.textContent = i === 0 ? "0" : total < 4 ? "" : dur(Math.round(tt - t0));
    root.append(lab);
  }
  shown.forEach((s, i) => {
    const yy = T + i * ROW;
    const label = svg("text", { x: L - 10, y: yy + 12, "text-anchor": "end" });
    label.textContent = s.name.length > 28 ? s.name.slice(0, 27) + "…" : s.name;
    const w = Math.max(2, x(s.started_at + (s.duration_ms || 0)) - x(s.started_at));
    const bar = svg("rect", { x: x(s.started_at), y: yy + 3, width: w, height: ROW - 6, rx: 2 });
    bar.style.fill = KIND[s.kind] || "var(--other)";
    if (s.kind === "turn") bar.style.stroke = "var(--axis)";
    hover(bar, dur(s.duration_ms), [[KIND[s.kind], `${s.kind}: ${s.name}`], [null, `starts at +${dur(s.started_at - t0)}`], [null, s.error ? "reported an error" : s.sidechain ? "subagent" : "main agent"]]);
    root.append(label, bar);
    const tail = svg("text", { x: x(s.started_at) + w + 6, y: yy + 12 });
    tail.textContent = (s.error ? "! " : "") + dur(s.duration_ms);
    if (s.error) tail.style.fill = "var(--critical)";
    root.append(tail);
  });
  const legend = el("div", { class: "legend" }, ...["turn", "inference", "tool"].map((k) => { const i = el("i"); i.style.background = KIND[k]; return el("span", {}, i, k); }),
    el("span", { class: "err", text: "! error" }));
  const note = spans.length > shown.length ? el("div", { class: "note", text: `showing the first ${shown.length} of ${spans.length} spans` }) : null;
  wrap.append(legend, note || "", el("div", { class: "waterfall" }, root));
  return wrap;
}

// ---- wiring ------------------------------------------------------------------

async function load() {
  const main = document.getElementById("main");
  main.classList.add("loading");
  try {
    const p = filterParams();
    const [ov, tl, mc, ss] = await Promise.all([api("overview", p), api("tools", p), api("mcp", p), api("sessions", p)]);
    tiles(ov.totals || {});
    daily(ov.by_day);
    bars("chart-project", ov.by_project);
    bars("chart-model", ov.by_model);
    tools(tl);
    mcp(mc);
    sessions(ss);
  } catch (e) {
    document.getElementById("tiles").replaceChildren(el("div", { class: "empty", text: String(e.message || e) }));
  } finally {
    main.classList.remove("loading");
  }
}

async function meta() {
  const m = await api("meta");
  const parts = [m.store];
  if (m.last_sync && m.last_sync.at) parts.push("synced " + when(m.last_sync.at));
  if (m.last_received && m.last_received.at) parts.push("received " + when(m.last_received.at));
  document.getElementById("meta").textContent = parts.join(" · ");
  const sel = document.getElementById("harness");
  for (const h of m.harnesses) sel.append(el("option", { value: h.harness, text: h.harness }));
}

function remember(key, value) {
  try { if (value == null) localStorage.removeItem(key); else localStorage.setItem(key, value); } catch (_) { /* storage unavailable */ }
}
function recall(key) {
  try { return localStorage.getItem(key); } catch (_) { return null; }
}

function setRange(v) {
  state.range = v;
  for (const b of document.querySelectorAll("#range button")) b.setAttribute("aria-checked", String(b.dataset.range === v));
  remember("agent-top-range", v);
}

document.addEventListener("DOMContentLoaded", async () => {
  setRange(recall("agent-top-range") || "30");
  const theme = recall("agent-top-theme");
  if (theme) document.documentElement.dataset.theme = theme;
  document.getElementById("range").addEventListener("click", (e) => {
    const b = e.target.closest("button"); if (!b) return;
    setRange(b.dataset.range); load();
  });
  document.getElementById("harness").addEventListener("change", (e) => { state.harness = e.target.value; load(); });
  document.getElementById("theme").addEventListener("click", () => {
    const dark = getComputedStyle(document.documentElement).colorScheme === "dark";
    const next = dark ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    remember("agent-top-theme", next);
    load();
  });
  for (const b of document.querySelectorAll("[data-table]")) {
    b.setAttribute("aria-pressed", "false");
    b.addEventListener("click", () => {
      const on = b.getAttribute("aria-pressed") !== "true";
      b.setAttribute("aria-pressed", String(on));
      document.getElementById("table-" + b.dataset.table).hidden = !on;
      document.getElementById("chart-" + b.dataset.table).hidden = on;
    });
  }
  document.getElementById("close-detail").addEventListener("click", () => {
    document.getElementById("detail").hidden = true;
    history.replaceState(null, "", location.pathname);
  });
  window.addEventListener("resize", () => { clearTimeout(window.__rt); window.__rt = setTimeout(load, 200); });
  try { await meta(); } catch (e) { document.getElementById("meta").textContent = String(e.message || e); }
  await load();
  const m = location.hash.match(/^#s\/([^/]+)\/(.+)$/);
  if (m) openSession({ harness: decodeURIComponent(m[1]), session_id: decodeURIComponent(m[2]) });
});
