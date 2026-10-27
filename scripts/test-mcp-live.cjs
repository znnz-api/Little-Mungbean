// Opt-in Windows integration check. Dependencies and all client homes are isolated.
// npm install --prefix <deps> @modelcontextprotocol/sdk@1.32.1
//   @modelcontextprotocol/server-filesystem@1.0.0 mcp-remote@0.15.1
// MUNGBEAN_MCP_LIVE_DEPS=<deps> node scripts/test-mcp-live.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const readline = require('node:readline');
const { spawn } = require('node:child_process');
const { createRequire } = require('node:module');
const deps = process.env.MUNGBEAN_MCP_LIVE_DEPS;
assert(deps && path.isAbsolute(deps), 'Set MUNGBEAN_MCP_LIVE_DEPS to the isolated npm prefix');
const dependency = createRequire(path.join(deps, 'package.json'));
const { Client } = dependency('@modelcontextprotocol/sdk/client/index.js');
const { StdioClientTransport } = dependency('@modelcontextprotocol/sdk/client/stdio.js');
const { StreamableHTTPClientTransport } = dependency('@modelcontextprotocol/sdk/client/streamableHttp.js');
const { SSEClientTransport } = dependency('@modelcontextprotocol/sdk/client/sse.js');
const { McpServer } = dependency('@modelcontextprotocol/sdk/server/mcp.js');
const { StreamableHTTPServerTransport } = dependency('@modelcontextprotocol/sdk/server/streamableHttp.js');
const { SSEServerTransport } = dependency('@modelcontextprotocol/sdk/server/sse.js');
const { createMcpExpressApp } = dependency('@modelcontextprotocol/sdk/server/express.js');
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'mungbean-mcp-live-'));
const project = path.resolve(__dirname, '..');
const results = [];
const requests = [];
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

async function check(name, action) {
    try {
        const detail = await action();
        results.push({ name, passed: true, detail });
        console.log(`PASS ${name}${detail ? `: ${JSON.stringify(detail)}` : ''}`);
    } catch (error) {
        results.push({ name, passed: false, error: error.stack });
        console.error(`FAIL ${name}: ${error.message}`);
    }
}

function run(command, args, options = {}) {
    return new Promise((resolve, reject) => {
        const child = spawn(command, args, { cwd: deps, windowsHide: true, ...options });
        let stdout = '', stderr = '';
        const timer = setTimeout(() => {
            child.kill();
            reject(new Error(`Timed out: ${command} ${args.join(' ')}\n${stderr.slice(-3000)}`));
        }, 90000);
        child.stdout.on('data', bytes => { stdout += bytes; });
        child.stderr.on('data', bytes => { stderr += bytes; });
        child.on('error', error => { clearTimeout(timer); reject(error); });
        child.on('exit', code => {
            clearTimeout(timer);
            if (code !== 0) reject(new Error(`Exit ${code}: ${stderr}\n${stdout}`));
            else resolve({ stdout, stderr });
        });
    });
}

async function protocolCheck(config, marker, isFilesystem) {
    const client = new Client({ name: 'little-mungbean-live-check', version: '1.0.0' });
    const transport = config.command
        ? new StdioClientTransport({
            command: config.command, args: config.args, env: { ...process.env, ...config.env },
            cwd: deps, stderr: 'pipe',
        })
        : new (config.type === 'sse' ? SSEClientTransport : StreamableHTTPClientTransport)(new URL(config.url), {
            requestInit: { headers: config.headers || config.http_headers },
        });
    let stderr = '';
    transport.stderr?.on('data', bytes => { stderr += bytes; });
    try {
        await client.connect(transport, { timeout: 45000 });
        const tools = await client.listTools({}, { timeout: 15000 });
        assert(tools.tools.length > 0);
        const name = isFilesystem ? 'read_text_file' : 'probe';
        assert(tools.tools.some(tool => tool.name === name));
        const answer = await client.callTool({ name, arguments: isFilesystem ? { path: marker } : {} }, undefined, { timeout: 15000 });
        assert(!answer.isError, JSON.stringify(answer));
        assert(answer.content.some(content => content.text?.includes(isFilesystem
            ? 'Little Mungbean real MCP tool call succeeded.' : 'http-probe-ok')));
        if (isFilesystem) {
            const denied = await client.callTool({ name, arguments: { path: path.join(root, 'outside-allowed.txt') } });
            assert(denied.isError, 'Filesystem access outside the selected folder must be denied');
        }
        return { tools: tools.tools.length, tool: name, result: 'verified' };
    } catch (error) {
        throw new Error(`${error.message}\n${stderr.slice(-3000)}`, { cause: error });
    } finally {
        await client.close();
    }
}

async function codexStatus(command, home, expected, disabled = false) {
    const child = spawn(command, ['app-server', '--stdio'], {
        cwd: deps, env: { ...process.env, CODEX_HOME: home }, windowsHide: true,
    });
    const waiting = new Map();
    let serial = 0, stderr = '';
    child.stderr.on('data', data => { stderr += data; });
    readline.createInterface({ input: child.stdout }).on('line', line => {
        let message;
        try { message = JSON.parse(line); } catch { return; }
        const request = waiting.get(message.id);
        if (request) {
            waiting.delete(message.id);
            clearTimeout(request.timer);
            if (message.error) request.reject(new Error(JSON.stringify(message.error)));
            else request.resolve(message.result);
        }
    });
    function rpc(method, params) {
        return new Promise((resolve, reject) => {
            const id = ++serial;
            const timer = setTimeout(() => {
                waiting.delete(id);
                reject(new Error(`Codex ${method} timed out\n${stderr.slice(-4000)}`));
            }, 60000);
            waiting.set(id, { resolve, reject, timer });
            child.stdin.write(JSON.stringify({ id, method, params }) + '\n');
        });
    }
    try {
        await rpc('initialize', { clientInfo: { name: 'mungbean_test', version: '1.0.0' } });
        child.stdin.write(JSON.stringify({ method: 'initialized' }) + '\n');
        const status = await rpc('mcpServerStatus/list', {});
        fs.writeFileSync(path.join(home, 'status.json'), JSON.stringify(status, null, 2));
        const names = status.data.map(server => server.name).sort();
        assert.deepEqual(names, expected.slice().sort());
        for (const server of status.data) {
            if (disabled) {
                assert.equal(Object.keys(server.tools).length, 0, `${server.name} must not connect while disabled`);
                assert.equal(server.serverInfo, null);
            } else {
                assert(Object.keys(server.tools).length > 0, `${server.name} must discover tools`);
            }
        }
        return status.data.map(server => ({ name: server.name, tools: Object.keys(server.tools).length }));
    } finally {
        child.stdin.end();
        for (let count = 0; count < 20 && child.exitCode === null; count++) await sleep(100);
        if (child.exitCode === null) child.kill();
        for (const request of waiting.values()) clearTimeout(request.timer);
    }
}

async function main() {
    const app = createMcpExpressApp();
    app.post('/mcp', async (req, res) => {
        if (req.headers['x-mungbean-probe'] !== 'local-test-header') {
            res.status(403).end('Missing test header');
            return;
        }
        requests.push(req.body?.method);
        const server = new McpServer({ name: 'mungbean-loopback-probe', version: '1.0.0' });
        server.registerTool('probe', { description: 'Read-only loopback integration probe', inputSchema: {} }, async () => ({
            content: [{ type: 'text', text: 'http-probe-ok' }],
        }));
        const transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined });
        res.on('close', () => { transport.close(); server.close(); });
        try {
            await server.connect(transport);
            await transport.handleRequest(req, res, req.body);
        } catch (error) {
            if (!res.headersSent) res.status(500).end(error.message);
        }
    });
    app.get('/mcp', (_req, res) => res.status(405).end());
    app.delete('/mcp', (_req, res) => res.status(405).end());
    const sse = new Map();
    app.post('/sse', (_req, res) => res.status(405).end());
    app.get('/sse', async (req, res) => {
        if (req.headers['x-mungbean-probe'] !== 'local-test-header') {
            res.status(403).end();
            return;
        }
        const server = new McpServer({ name: 'mungbean-sse-probe', version: '1.0.0' });
        server.registerTool('probe', { inputSchema: {} }, async () => ({
            content: [{ type: 'text', text: 'http-probe-ok' }],
        }));
        const transport = new SSEServerTransport('/messages', res);
        sse.set(transport.sessionId, transport);
        res.on('close', () => { sse.delete(transport.sessionId); server.close(); });
        await server.connect(transport);
    });
    app.post('/messages', async (req, res) => {
        const transport = sse.get(req.query.sessionId);
        if (!transport || req.headers['x-mungbean-probe'] !== 'local-test-header') {
            res.status(403).end();
            return;
        }
        await transport.handlePostMessage(req, res, req.body);
    });
    const listener = await new Promise(resolve => {
        const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
    });
    const endpoint = `http://127.0.0.1:${listener.address().port}/mcp`;
    console.log(`Isolated test directory: ${root}`);
    try {
        const generated = await run('cargo', [
            'test', '--locked', '--target', 'x86_64-pc-windows-msvc',
            'export_live_mcp_configurations', '--', '--ignored', '--test-threads=1',
        ], { cwd: project, env: { ...process.env, MUNGBEAN_MCP_LIVE_ROOT: root, MUNGBEAN_MCP_LIVE_URL: endpoint } });
        assert(generated.stdout.includes('1 passed'), generated.stdout + generated.stderr);
        const manifest = JSON.parse(fs.readFileSync(path.join(root, 'manifest.json')));
        fs.writeFileSync(path.join(root, 'outside-allowed.txt'), 'not allowed');
        const enabled = manifest.phases.enabled;
        for (const client of enabled) {
            for (const [name, config] of Object.entries(client.servers)) {
                await check(`${client.target}/${name}: initialize, tools/list, tools/call`, () =>
                    protocolCheck(config, manifest.marker, name === 'filesystem'));
            }
        }
        const codex = process.env.MUNGBEAN_MCP_LIVE_CODEX;
        if (codex) {
            for (const phase of ['enabled', 'disabled', 'reenabled', 'renamed', 'deleted']) {
                const expected = phase === 'deleted' ? []
                    : [phase === 'renamed' ? 'filesystem-renamed' : 'filesystem', 'probe-http'];
                for (const target of ['codex-cli', 'codex-desktop']) {
                    const config = manifest.phases[phase].find(client => client.target === target);
                    const executable = target === 'codex-desktop'
                        ? process.env.MUNGBEAN_MCP_LIVE_CODEX_DESKTOP || codex : codex;
                    await check(`${target}/${phase}: real Codex MCP discovery`, async () => {
                        const before = requests.length;
                        const status = await codexStatus(executable, path.dirname(config.path), expected, phase === 'disabled');
                        if (phase === 'disabled') assert.equal(requests.length, before, 'Disabled servers must not receive requests');
                        return status;
                    });
                }
            }
        } else {
            console.log('SKIP installed Codex client checks: set MUNGBEAN_MCP_LIVE_CODEX');
        }
        const claude = process.env.MUNGBEAN_MCP_LIVE_CLAUDE;
        if (claude) {
            for (const phase of ['enabled', 'disabled', 'reenabled', 'renamed', 'deleted']) {
                const config = manifest.phases[phase].find(client => client.target === 'claude-code');
                await check(`claude-code/${phase}: real client connection check`, async () => {
                    const result = await run(claude, ['mcp', 'list'], {
                        env: { ...process.env, CLAUDE_CONFIG_DIR: path.dirname(config.path),
                            CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', CLAUDE_CODE_SKIP_MCP_ON_STARTUP: '' },
                    });
                    fs.writeFileSync(path.join(path.dirname(config.path), 'connection-check.txt'), result.stdout + result.stderr);
                    if (['disabled', 'deleted'].includes(phase)) {
                        assert(!result.stdout.includes('probe-http'));
                        assert(!result.stdout.includes('filesystem'));
                    } else {
                        assert.equal((result.stdout.match(/Connected/g) || []).length, 3, result.stdout + result.stderr);
                    }
                    return result.stdout.trim();
                });
            }
        } else {
            console.log('SKIP installed Claude Code checks: set MUNGBEAN_MCP_LIVE_CLAUDE');
        }
        assert(requests.includes('tools/call'));
    } finally {
        listener.closeAllConnections();
        await new Promise(resolve => listener.close(resolve));
        fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify({ results, httpMethods: requests }, null, 2));
    }
    console.log(`Results: ${results.filter(result => result.passed).length}/${results.length} passed`);
    console.log(`Report: ${path.join(root, 'results.json')}`);
    if (results.some(result => !result.passed)) process.exitCode = 1;
}
// All transports, child processes and report writes finish inside main. Exit
// explicitly because dependencies can retain SSE reconnect/HTTP pool timers.
main().then(() => process.exit(process.exitCode || 0), error => {
    console.error(error);
    process.exit(1);
});
