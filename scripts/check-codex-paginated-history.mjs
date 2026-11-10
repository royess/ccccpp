// Optional real-Codex compatibility check. No credentials or model turns.
// CCCC_CODEX_TEST_BINARY=/path/to/codex node scripts/check-codex-paginated-history.mjs
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { appendFileSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const binary = process.env.CCCC_CODEX_TEST_BINARY;
assert(binary, 'Set CCCC_CODEX_TEST_BINARY to opt into the isolated real-Codex check');
const fixtureTurns = Number(process.env.CCCC_CODEX_TEST_HISTORY_TURNS || 200);
assert(Number.isInteger(fixtureTurns) && fixtureTurns >= 4 && fixtureTurns <= 10000);
const root = mkdtempSync(join(tmpdir(), 'cccc-history-fixture-'));
const env = { ...process.env, CODEX_HOME: root, CODEX_SQLITE_HOME: root, TERM: 'xterm-256color' };
writeFileSync(join(root, 'config.toml'), `
model_provider = "fixture"
model = "fixture-model"
check_for_update_on_startup = false
[model_providers.fixture]
name = "Isolated fixture"
base_url = "http://127.0.0.1:9/v1"
wire_api = "responses"
requires_openai_auth = false
`);

function deadline(promise, label, milliseconds = 15000) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => { timer = setTimeout(() => reject(Error(`${label} timeout`)), milliseconds); }),
  ]).finally(() => clearTimeout(timer));
}

// A real PTY is required by the stock TUI. Answer terminal cursor queries only;
// never send a prompt, Enter, approval, or business command.
const ptyDriver = String.raw`import os,pty,select,subprocess,sys,time,fcntl,termios,struct,signal
master,slave=pty.openpty()
fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,120,0,0))
p=subprocess.Popen(sys.argv[1:],stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
os.close(slave);data=b'';end=time.monotonic()+10
try:
 while time.monotonic()<end:
  if select.select([master],[],[],.1)[0]:
   try:b=os.read(master,65536)
   except OSError:break
   data+=b
   if b'\x1b[6n' in b:os.write(master,b'\x1b[1;1R')
   if b'state=idle:app=codex' in data:break
  if p.poll() is not None:break
finally:
 os.killpg(p.pid,signal.SIGTERM)
 try:p.wait(timeout=2)
 except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait()
 try:os.killpg(p.pid,signal.SIGKILL)
 except ProcessLookupError:pass
 os.close(master)
sys.stdout.buffer.write(data)
`;

async function check(mode, resumeId, expectedTurns = 0) {
  const server = spawn(binary, ['app-server', '--listen', 'ws://127.0.0.1:0'], { cwd: root, env, detached: true });
  let ws;
  try {
    const endpoint = await deadline(new Promise((resolve, reject) => {
      let output = '';
      const receive = bytes => {
        output += bytes;
        const match = output.match(/ws:\/\/127\.0\.0\.1:\d+/);
        if (match) resolve(match[0]);
      };
      server.stdout.on('data', receive);
      server.stderr.on('data', receive);
      server.once('error', reject);
      server.once('exit', () => reject(Error('app-server exited during startup')));
    }), 'app-server startup');
    ws = new WebSocket(endpoint);
    await deadline(new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; }), 'connect');
    let sequence = 0;
    const pending = new Map();
    ws.onmessage = event => {
      const message = JSON.parse(event.data);
      const request = pending.get(message.id);
      if (!request) return;
      pending.delete(message.id);
      if (message.error) request.reject(Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    };
    const call = (method, params) => {
      const id = ++sequence;
      return deadline(new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        ws.send(JSON.stringify({ id, method, params }));
      }), method).finally(() => pending.delete(id));
    };
    await call('initialize', { clientInfo: { name: 'fixture', version: '1' }, capabilities: { experimentalApi: true } });
    const params = { cwd: root, sandbox: 'read-only', approvalPolicy: 'on-request' };
    const result = resumeId
      ? await call('thread/resume', { ...params, threadId: resumeId, excludeTurns: true })
      : await call('thread/start', mode === 'legacy' ? { ...params, historyMode: 'legacy' } : params);
    const thread = result.thread.id;
    if (resumeId) assert.equal(thread, resumeId);
    else await call('thread/name/set', { threadId: thread, name: 'History fixture' });
    const metadata = await call('thread/read', { threadId: thread, includeTurns: false });
    assert.equal(metadata.thread.historyMode, mode);
    assert(metadata.thread.path);
    const page = await call('thread/turns/list', { threadId: thread, limit: 2, itemsView: 'summary', sortDirection: 'desc' });
    assert.equal(page.data.length, Math.min(expectedTurns, 2));
    if (expectedTurns > 2) {
      assert(page.nextCursor, 'nonempty history must expose another page');
      const next = await call('thread/turns/list', { threadId: thread, limit: 2, itemsView: 'summary', sortDirection: 'desc', cursor: page.nextCursor });
      assert.equal(next.data.length, 2);
      assert(!next.data.some(turn => page.data.some(previous => previous.id === turn.id)), 'history cursor repeated a page');
      const items = await call('thread/items/list', { threadId: thread, turnId: page.data[0].id, limit: 10 });
      assert(JSON.stringify(items).includes(`Fixture answer ${expectedTurns - 1}:`), `latest answer was not retained: ${JSON.stringify({page,items}).slice(0,1500)}`);
    }
    const tui = spawn('python3', ['-c', ptyDriver, binary, '--remote', endpoint, 'resume', thread, '--no-alt-screen'], { cwd: root, env });
    let screen = '';
    tui.stdout.on('data', bytes => { screen += bytes; });
    tui.stderr.on('data', bytes => { screen += bytes; });
    await deadline(new Promise((resolve, reject) => { tui.once('exit', resolve); tui.once('error', reject); }), 'native TUI', 15000)
      .finally(() => { if (tui.exitCode === null) tui.kill('SIGKILL'); });
    assert(screen.includes('state=idle:app=codex'), `Native TUI did not reach idle: ${screen.slice(-2500)}`);
    console.log(`${mode} ${resumeId ? 'same-thread resume' : 'new thread'}: metadata, page and native TUI passed`);
    return { thread, path: metadata.thread.path };
  } finally {
    ws?.close();
    try { process.kill(-server.pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    await new Promise(resolve => { if (server.exitCode !== null || server.signalCode) resolve(); else server.once('exit', resolve); });
  }
}

try {
  const paginated = await check('paginated');
  await check('paginated', paginated.thread);
  const legacy = await check('legacy');
  // The legacy writer is stopped above. Add synthetic completed turns so the
  // migration and paging checks verify retained messages, not an empty thread.
  for (let i = 0; i < fixtureTurns; i++) {
    const timestamp = '2026-01-01T00:00:00Z';
    const turnId = `fixture-turn-${i}`;
    const events = [
      { type: 'event_msg', payload: { type: 'task_started', turn_id: turnId, model_context_window: null, collaboration_mode_kind: 'default' } },
      { type: 'event_msg', payload: { type: 'user_message', message: `Fixture question ${i}`, images: [], local_images: [], text_elements: [] } },
      { type: 'response_item', payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: `Fixture question ${i}` }] } },
      { type: 'response_item', payload: { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: `Fixture answer ${i}: ${'x'.repeat(4096)}` }] } },
      { type: 'event_msg', payload: { type: 'agent_message', message: `Fixture answer ${i}: ${'x'.repeat(4096)}`, phase: 'final_answer' } },
      { type: 'event_msg', payload: { type: 'task_complete', turn_id: turnId, last_agent_message: `Fixture answer ${i}` } },
    ];
    appendFileSync(legacy.path, events.map(event => JSON.stringify({ timestamp, ...event })).join('\n') + '\n');
  }
  const migration = spawnSync(binary, ['migrate-rollouts', '--thread', legacy.thread, '--apply', '--json'], { cwd: root, env, encoding: 'utf8', timeout: 30000 });
  assert.equal(migration.status, 0, migration.stderr);
  const outcomes = JSON.parse(migration.stdout).outcomes;
  assert(outcomes.some(outcome => outcome.thread_id === legacy.thread && outcome.status === 'migrated'), migration.stdout);
  await check('paginated', legacy.thread, fixtureTurns);
} finally {
  rmSync(root, { recursive: true, force: true });
}
