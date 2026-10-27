// Companion to the ignored real-client Skills test. All skills live in its temp fixture.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';

const [fixture, phase] = process.argv.slice(2);
assert(fixture && ['installed', 'disabled', 'enabled', 'removed'].includes(phase));
const name = 'mungbean-skill-check';
const expected = phase === 'installed' || phase === 'enabled';
const codexBinary = process.env.MUNGBEAN_TEST_CODEX_BINARY ?? process.env.CODEX_CLI_PATH;
assert(codexBinary, 'Set MUNGBEAN_TEST_CODEX_BINARY to an installed Codex executable.');

async function discover(profile) {
  const binary = profile === 'cli' ? (process.env.MUNGBEAN_TEST_CODEX_CLI_BINARY ?? codexBinary) : codexBinary;
  const child = spawn(binary, ['app-server', '--stdio'], {
    cwd: path.join(fixture, 'workspace'),
    env: { ...process.env, CODEX_HOME: path.join(fixture, profile) },
    windowsHide: true,
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let nextId = 0;
  const pending = new Map();
  let stderr = '';
  child.stderr.on('data', (data) => { stderr = (stderr + data).slice(-4000); });
  const lines = createInterface({ input: child.stdout });
  lines.on('line', (line) => {
    let message;
    try { message = JSON.parse(line); } catch { return; }
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.error) request.reject(new Error(JSON.stringify(message.error)));
    else request.resolve(message.result);
  });
  const rejectPending = (error) => {
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  };
  child.on('error', rejectPending);
  child.on('exit', (code) => rejectPending(new Error(`Codex exited ${code}: ${stderr}`)));
  const timer = setTimeout(() => {
    rejectPending(new Error(`Codex discovery timed out: ${stderr}`));
    child.kill();
  }, 30000);
  const send = (message) => child.stdin.write(`${JSON.stringify(message)}\n`);
  const request = (method, params) => new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    send({ id, method, params });
  });
  try {
    const initialized = await request('initialize', {
      clientInfo: { name: 'little-mungbean-skill-test', version: '1.0' },
      capabilities: { experimentalApi: true },
    });
    send({ method: 'initialized' });
    const result = await request('skills/list', {
      cwds: [path.join(fixture, 'workspace')], forceReload: true,
    });
    const scopes = result.data ?? [];
    const matching = scopes.flatMap((scope) => scope.skills ?? []).filter((skill) => skill.name === name);
    assert.equal(matching.length, expected ? 1 : 0, `${profile}: ${phase}: ${JSON.stringify(result)}`);
    if (expected) {
      assert.equal(matching[0].enabled, true);
      assert.equal(path.resolve(matching[0].path), path.join(fixture, profile, 'skills', name, 'SKILL.md'));
    }
    return { profile, phase, found: matching.length, userAgent: initialized.userAgent };
  } finally {
    clearTimeout(timer);
    lines.close();
    child.stdin.end();
    child.kill();
    for (const stream of child.stdio) stream?.destroy();
    child.unref();
  }
}

async function invokeCodex() {
  const profile = path.join(fixture, 'codex');
  const proof = (await readFile(path.join(profile, 'skills', name, 'references/proof.txt'), 'utf8')).trim();
  const auth = await readFile(path.join(process.env.CODEX_HOME ?? path.join(os.homedir(), '.codex'), 'auth.json'));
  await writeFile(path.join(profile, 'auth.json'), auth);
  const finalPath = path.join(fixture, 'codex-final.txt');
  const args = [
    '--ask-for-approval', 'never', 'exec', '--json', '--ephemeral',
    '--ignore-user-config', '--ignore-rules', '--skip-git-repo-check', '--sandbox', 'read-only',
    '-c', 'model_reasoning_effort="low"', '--output-last-message', finalPath,
  ];
  if (process.env.MUNGBEAN_TEST_CODEX_MODEL) args.push('--model', process.env.MUNGBEAN_TEST_CODEX_MODEL);
  args.push(`Use the installed $${name} Skill to run the Little-Mungbean Skill check. Follow its instructions and return only its result.`);
  const events = await new Promise((resolve, reject) => {
    const child = spawn(codexBinary, args, {
      cwd: path.join(fixture, 'workspace'),
      env: { ...process.env, CODEX_HOME: profile },
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => {
      child.kill();
      for (const stream of child.stdio) stream?.destroy();
      reject(new Error('Codex Skill invocation timed out.'));
    }, 90000);
    child.stdout.on('data', (data) => { stdout += data; });
    child.stderr.on('data', (data) => { stderr = (stderr + data).slice(-2000); });
    child.on('error', (error) => { clearTimeout(timer); reject(error); });
    child.on('exit', (code) => {
      clearTimeout(timer);
      for (const stream of child.stdio) stream?.destroy();
      if (code !== 0) reject(new Error(`Codex invocation exited ${code}: ${stderr}`));
      else resolve(stdout.trim().split('\n').filter(Boolean).map((line) => JSON.parse(line)));
    });
  });
  const final = (await readFile(finalPath, 'utf8')).trim();
  assert.equal(final, proof, 'Codex must return the bundled proof, which was never included in the prompt.');
  return { invoked: true, proofMatched: true, modelInvoked: true, turnCompleted: events.some((event) => event.type === 'turn.completed') };
}

async function invokeClaude() {
  const binary = process.env.MUNGBEAN_TEST_CLAUDE_BINARY;
  if (!binary) return { invoked: false, reason: 'No Claude Code test executable configured.' };
  const project = path.join(fixture, 'claude-project');
  const proof = (await readFile(path.join(project, '.claude/skills', name, 'references/proof.txt'), 'utf8')).trim();
  // Reuse only connection parameters in memory. Plugins, hooks and user skills
  // stay outside this isolated test profile, and credentials are never logged.
  let connectionEnv = {};
  const settingsFile = process.env.MUNGBEAN_TEST_CLAUDE_SETTINGS
    ?? path.join(os.homedir(), '.claude', 'settings.json');
  try {
    const settings = JSON.parse(await readFile(settingsFile, 'utf8'));
    connectionEnv = Object.fromEntries(Object.entries(settings.env ?? {}).filter(([key]) =>
      /^(ANTHROPIC_[A-Z_]+|CLAUDE_GATEWAY_ALLOW_LOOPBACK|HTTPS?_PROXY|ALL_PROXY|NO_PROXY)$/i.test(key)));
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  const args = [
    '--print', '--no-session-persistence', '--output-format', 'json',
    '--setting-sources', 'project', '--strict-mcp-config',
    '--permission-mode', 'dontAsk', '--tools', 'Skill,Read', '--allowedTools', 'Skill,Read',
    '--max-budget-usd', '0.50', '--effort', 'low',
    `Use the installed /${name} Skill to run the Little-Mungbean Skill check. Follow its instructions and return only its result.`,
  ];
  const result = await new Promise((resolve, reject) => {
    const child = spawn(binary, args, {
      cwd: project,
      env: { ...connectionEnv, ...process.env, CLAUDE_CONFIG_DIR: path.join(fixture, 'claude-config') },
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => { child.kill(); reject(new Error('Claude Code check timed out.')); }, 90000);
    child.stdout.on('data', (data) => { stdout += data; });
    child.stderr.on('data', (data) => { stderr = (stderr + data).slice(-3000); });
    child.on('error', (error) => { clearTimeout(timer); reject(error); });
    child.on('exit', (code) => {
      clearTimeout(timer);
      if (code !== 0) reject(new Error(`Claude Code exited ${code}: ${stderr}\n${stdout}`));
      else resolve(JSON.parse(stdout));
    });
  });
  assert.equal(result.is_error, false, JSON.stringify(result));
  assert.equal(result.result?.trim(), proof, 'Claude must return the bundled proof, which was never included in the prompt.');
  return { invoked: true, proofMatched: true, costUsd: result.total_cost_usd };
}

async function discoverClaude() {
  const binary = process.env.MUNGBEAN_TEST_CLAUDE_BINARY;
  if (!binary) return { checked: false, reason: 'No Claude Code test executable configured.' };
  // A local error endpoint lets the real client emit its discovery metadata
  // without sending a prompt or credentials to any model provider.
  const server = http.createServer((_request, response) => {
    response.writeHead(400, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ type: 'error', error: { type: 'invalid_request_error', message: 'Local skill discovery check; no model invocation.' } }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const stdout = await new Promise((resolve, reject) => {
      const child = spawn(binary, [
        '--print', '--no-session-persistence', '--output-format', 'stream-json', '--verbose',
        '--setting-sources', 'project', '--strict-mcp-config', '--tools', 'Skill,Read',
        '--permission-mode', 'dontAsk', `List whether /${name} is available.`,
      ], {
        cwd: path.join(fixture, 'claude-project'),
        env: {
          ...process.env,
          CLAUDE_CONFIG_DIR: path.join(fixture, 'claude-config-discovery'),
          ANTHROPIC_BASE_URL: `http://127.0.0.1:${server.address().port}`,
          ANTHROPIC_API_KEY: 'local-skill-discovery-only',
          ANTHROPIC_AUTH_TOKEN: '',
          NO_PROXY: '127.0.0.1,localhost',
        },
        windowsHide: true,
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      let output = '';
      let stderr = '';
      const timer = setTimeout(() => { child.kill(); reject(new Error(`Claude discovery timed out: ${stderr}`)); }, 20000);
      child.stdout.on('data', (data) => { output += data; });
      child.stderr.on('data', (data) => { stderr = (stderr + data).slice(-2000); });
      child.on('error', (error) => { clearTimeout(timer); reject(error); });
      child.on('exit', () => { clearTimeout(timer); resolve(output); });
    });
    const events = stdout.trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
    const init = events.find((event) => event.type === 'system' && event.subtype === 'init');
    assert(init, `Claude did not report initialization: ${stdout}`);
    const names = [...(init.skills ?? []), ...(init.slash_commands ?? [])].map((skill) => typeof skill === 'string' ? skill : skill.name);
    assert.equal(names.includes(name), expected, `Claude ${phase}: ${JSON.stringify(init)}`);
    return { checked: true, phase, found: names.includes(name), version: init.claude_code_version, modelInvoked: false };
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
}

const report = {
  phase,
  codex: [await discover('codex'), await discover('cli')],
  claudeCode: await discoverClaude(),
};
if (phase === 'installed' && process.env.MUNGBEAN_TEST_CODEX_LIVE === '1') report.codexInvocation = await invokeCodex();
if (phase === 'installed' && process.env.MUNGBEAN_TEST_CLAUDE_LIVE === '1') report.claudeInvocation = await invokeClaude();
const reportRoot = process.env.MUNGBEAN_SKILLS_REPORT_DIR;
if (reportRoot) {
  await mkdir(reportRoot, { recursive: true });
  await writeFile(path.join(reportRoot, `${phase}.json`), `${JSON.stringify(report, null, 2)}\n`);
}
console.log(JSON.stringify(report));
