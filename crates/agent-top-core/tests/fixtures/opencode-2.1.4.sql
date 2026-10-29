-- OpenCode 2.1.4 session store (verified read-only against a live
-- opencode.db on 2026-10-27), reduced to the columns and data fields the
-- adapter reads, the way the .jsonl fixtures in this directory are reduced
-- (see the README). Hand-built rather than a reduced real transcript, as
-- the Gemini fixture is: the layout is the harness's and the numbers are
-- chosen, so there is nothing to sanitise.
--
-- One top-level session (`ses_gold`) and one subagent (`ses_gold_sub`) in
-- `session_v2`; their conversation in `session_message`, tool calls nested
-- in the `assistant` rows' `content` arrays. In one session it covers:
--   * a first-class `compaction` row, v2's replacement for the v1
--     summary-flagged reply;
--   * a still-running tool call (no `completed`), so its span stays open;
--   * an in-flight `assistant` row: `time.streamed` and no `tokens` or
--     `cost`, as OpenCode writes it while a reply streams;
--   * MCP calls in both v2 forms: a direct `scratch_fs_list_directory`
--     item, and two `git.*` calls nested in the `execute` item's
--     `state.metadata.toolCalls`.
--
-- `directory` is a placeholder; the golden test repoints it at a temporary
-- project whose `opencode.json` names the MCP servers (`scratch_fs`,
-- `git`), so the parse never depends on whose machine runs the suite.
--
-- Time base 1789000000000 epoch ms.

CREATE TABLE session_v2 (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, directory TEXT, agent TEXT,
 model TEXT, cost REAL DEFAULT 0, tokens_input INTEGER DEFAULT 0, tokens_output INTEGER DEFAULT 0,
 tokens_reasoning INTEGER DEFAULT 0, tokens_cache_read INTEGER DEFAULT 0, tokens_cache_write INTEGER DEFAULT 0,
 time_created INTEGER, time_updated INTEGER, version TEXT);
CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER, time_created INTEGER, data TEXT);

INSERT INTO session_v2 VALUES
 ('ses_gold', 'prj', NULL, '/tmp/v2proj', 'build',
  '{"id":"claude-opus-5","providerID":"anthropic"}', 0.4213, 4200, 380, 40, 185000, 1200, 1789000000000, 1789000060000, '2.1.4'),
 ('ses_gold_sub', 'prj', 'ses_gold', '/tmp/v2proj', 'explore',
  '{"id":"claude-opus-5","providerID":"anthropic"}', 0.0512, 900, 60, 5, 42000, 0, 1789000020000, 1789000050000, '2.1.4');

INSERT INTO session_message VALUES
 ('g_u1', 'ses_gold', 'user', 1, 1789000001000,
  '{"time":{"created":1789000001000}}'),
 ('g_a1', 'ses_gold', 'assistant', 2, 1789000002000,
  '{"time":{"created":1789000002000,"completed":1789000004000},"model":{"id":"claude-opus-5"},"cost":0.10,"tokens":{"input":1200,"output":80,"reasoning":10,"cache":{"read":0,"write":200}},"content":[{"type":"tool","id":"c1","name":"read","executed":true,"state":{"status":"completed"},"time":{"created":1789000002100,"ran":1789000002100,"completed":1789000002600}},{"type":"tool","id":"c2","name":"bash","executed":true,"state":{"status":"error"},"time":{"created":1789000002700,"ran":1789000002700,"completed":1789000003200}}]}'),
 ('g_a2', 'ses_gold', 'assistant', 3, 1789000005000,
  '{"time":{"created":1789000005000,"completed":1789000008000},"model":{"id":"claude-opus-5"},"cost":0.12,"tokens":{"input":1500,"output":120,"reasoning":10,"cache":{"read":1400,"write":0}},"content":[{"type":"tool","id":"c3","name":"scratch_fs_list_directory","executed":true,"state":{"status":"completed"},"time":{"created":1789000005200,"ran":1789000005200,"completed":1789000005400}},{"type":"tool","id":"c4","name":"execute","executed":true,"state":{"status":"completed","metadata":{"toolCalls":[{"tool":"git.git_status","status":"completed"},{"tool":"git.git_diff_unstaged","status":"error"}]}},"time":{"created":1789000005500,"ran":1789000005600,"completed":1789000007800}}]}'),
 ('g_c1', 'ses_gold', 'compaction', 4, 1789000009000,
  '{"time":{"created":1789000009000,"completed":1789000011000},"model":{"id":"claude-opus-5"},"cost":0.08,"tokens":{"input":2600,"output":200,"reasoning":20,"cache":{"read":1200,"write":0}},"content":[]}'),
 ('g_a3', 'ses_gold', 'assistant', 5, 1789000012000,
  '{"time":{"created":1789000012000,"completed":1789000015000},"model":{"id":"claude-opus-5"},"cost":0.12,"tokens":{"input":900,"output":170,"reasoning":0,"cache":{"read":0,"write":0}},"content":[{"type":"tool","id":"c5","name":"webfetch","executed":true,"state":{"status":"completed"},"time":{"created":1789000012500,"ran":1789000012500,"completed":1789000014000}},{"type":"tool","id":"c6","name":"grep","executed":true,"state":{"status":"completed"},"time":{"created":1789000013200,"ran":1789000013200,"completed":1789000014000}}]}'),
 ('g_a4', 'ses_gold', 'assistant', 6, 1789000016000,
  '{"time":{"created":1789000016000,"streamed":1789000016500},"model":{"id":"claude-opus-5"},"content":[{"type":"tool","id":"c7","name":"grep","executed":false,"state":{"status":"running"},"time":{"created":1789000016200,"ran":1789000016200}}]}'),
 ('s_k1', 'ses_gold_sub', 'assistant', 1, 1789000025000,
  '{"time":{"created":1789000025000,"completed":1789000027000},"model":{"id":"claude-opus-5"},"cost":0.0512,"tokens":{"input":900,"output":60,"reasoning":5,"cache":{"read":0,"write":0}},"content":[{"type":"tool","id":"c8","name":"read","executed":true,"state":{"status":"completed"},"time":{"created":1789000025500,"ran":1789000025500,"completed":1789000025800}}]}');
