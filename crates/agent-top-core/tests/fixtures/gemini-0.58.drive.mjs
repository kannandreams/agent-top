// Drives Gemini CLI 0.58.0's own ChatRecordingService with a scripted
// conversation and a fake clock, so the fixture is written by the harness's
// recorder rather than by hand. Nothing in it is real: placeholder text,
// empty arguments, a made-up project path.
import fs from 'node:fs';
import path from 'node:path';

const RealDate = Date;
let now = RealDate.parse('2026-09-05T09:00:00.000Z');
globalThis.Date = class extends RealDate {
  constructor(...a) { if (a.length === 0) super(now); else super(...a); }
  static now() { return now; }
};
const at = (secs) => { now = RealDate.parse('2026-09-05T09:00:00.000Z') + Math.round(secs * 1000); };
const iso = () => new Date().toISOString();

const { ChatRecordingService } = await import('@google/gemini-cli-core/dist/src/services/chatRecordingService.js');

const root = process.argv[2];
fs.rmSync(root, { recursive: true, force: true });
const project = path.join(root, 'tmp', 'example');
fs.mkdirSync(project, { recursive: true });
fs.writeFileSync(path.join(project, '.project_root'), '/Users/dev/code/example');

const MAIN = '0a1b2c3d-0000-4000-8000-000000000001';
const SUB = 'b2c3d4e5-0000-4000-8000-000000000002';
const ctx = (promptId, parentSessionId) => ({
  promptId,
  parentSessionId,
  toolRegistry: { getTool: () => undefined },
  config: {
    getProjectRoot: () => '/Users/dev/code/example',
    storage: { getProjectTempDir: () => project },
    getWorkspaceContext: () => ({ getDirectories: () => ['/Users/dev/code/example'] }),
  },
});

const tokens = (input, output, cached, thoughts, tool = 0) => ({
  promptTokenCount: input, candidatesTokenCount: output, cachedContentTokenCount: cached,
  thoughtsTokenCount: thoughts, toolUsePromptTokenCount: tool, totalTokenCount: input + output + thoughts + tool,
});
const call = (id, name, status, secs, extra = {}) => { at(secs); return { id, name, args: {}, result: null, status, timestamp: iso(), ...extra }; };
const fnResponse = (id, name) => [{ functionResponse: { id, name, response: { output: '' } } }];

const main = new ChatRecordingService(ctx(MAIN));
at(0); await main.initialize(undefined, 'main');

// Turn 1: a prompt, one tool call, a final reply.
at(2); main.recordMessage({ model: 'gemini-2.5-pro', type: 'user', content: [{ text: 'prompt 1' }] });
at(5); main.recordThought({ subject: 'thinking', description: '' });
main.recordMessageTokens(tokens(12000, 40, 9000, 300));
main.recordMessage({ model: 'gemini-2.5-pro', type: 'gemini', content: '' });
const c1 = call('call-1', 'read_file', 'success', 9);
at(9); main.recordToolCalls('gemini-2.5-pro', [c1]);
at(9.1); main.recordSyntheticMessage('user', fnResponse('call-1', 'read_file'));
at(14); main.recordMessageTokens(tokens(12500, 200, 12000, 100));
main.recordMessage({ model: 'gemini-2.5-pro', type: 'gemini', content: 'reply 1' });

// Turn 2: two tool calls in parallel, one of them failing, one a web search.
at(60); main.recordMessage({ model: 'gemini-2.5-pro', type: 'user', content: [{ text: 'prompt 2' }] });
at(63); main.recordMessageTokens(tokens(12800, 60, 12500, 250));
main.recordMessage({ model: 'gemini-2.5-pro', type: 'gemini', content: '' });
const c2 = call('call-2', 'google_web_search', 'success', 66);
const c3 = call('call-3', 'run_shell_command', 'error', 70);
at(70); main.recordToolCalls('gemini-2.5-pro', [c2, c3]);
at(70.1); main.recordSyntheticMessage('user', [...fnResponse('call-2', 'google_web_search'), ...fnResponse('call-3', 'run_shell_command')]);
at(75); main.recordMessageTokens(tokens(14000, 500, 12800, 400, 120));
main.recordMessage({ model: 'gemini-2.5-pro', type: 'gemini', content: 'reply 2' });

// Turn 3: a subagent, with its own transcript under the parent's id.
at(120); main.recordMessage({ model: 'gemini-2.5-pro', type: 'user', content: [{ text: 'prompt 3' }] });
at(124); main.recordMessageTokens(tokens(14300, 30, 14000, 150));
main.recordMessage({ model: 'gemini-2.5-pro', type: 'gemini', content: '' });
const sub = new ChatRecordingService(ctx(SUB, MAIN));
at(124.5); await sub.initialize(undefined, 'subagent');
at(124.6); sub.recordMessage({ model: 'gemini-2.5-flash', type: 'user', content: [{ text: 'task' }] });
at(128); sub.recordMessageTokens(tokens(3000, 20, 0, 80));
sub.recordMessage({ model: 'gemini-2.5-flash', type: 'gemini', content: '' });
const c5 = call('call-5', 'grep_search', 'success', 130);
at(130); sub.recordToolCalls('gemini-2.5-flash', [c5]);
at(130.1); sub.recordSyntheticMessage('user', fnResponse('call-5', 'grep_search'));
at(134); sub.recordMessageTokens(tokens(3600, 300, 3000, 60));
sub.recordMessage({ model: 'gemini-2.5-flash', type: 'gemini', content: 'done' });
const c4 = call('call-4', 'codebase_investigator', 'success', 135, { agentId: SUB });
at(135); main.recordToolCalls('gemini-2.5-pro', [c4]);
at(135.1); main.recordSyntheticMessage('user', fnResponse('call-4', 'codebase_investigator'));
at(140); main.recordMessageTokens(tokens(15000, 350, 14300, 200));
main.recordMessage({ model: 'gemini-2.5-pro', type: 'gemini', content: 'reply 3' });

// The user rewinds to before turn 3 and asks again; the tokens are spent either way.
const rewindTo = main.getConversation().messages.find((m) => m.type === 'user' && m.timestamp === '2026-09-05T09:02:00.000Z').id;
at(180); main.rewindTo(rewindTo);
at(181); main.recordMessage({ model: 'gemini-2.5-pro', type: 'user', content: [{ text: 'prompt 4' }] });
at(186); main.recordMessageTokens(tokens(15200, 120, 14000, 90));
main.recordMessage({ model: 'gemini-2.5-pro', type: 'gemini', content: 'reply 4' });
main.saveSummary('summary');

console.log(main.getConversationFilePath());
console.log(sub.getConversationFilePath());
