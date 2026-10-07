import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { spawn, spawnSync } from 'node:child_process';
import {smokeWeb} from './web-smoke.mjs';

// 在独立副本启动实际发行程序；模拟模型只检查工具注册，不执行系统诊断。
export async function smokeRelease(source, workDir) {
  if (fs.existsSync(workDir)) throw new Error(`冒烟目录已存在，拒绝覆盖: ${workDir}`);
  const root = path.join(workDir, 'cst-pilot');
  fs.cpSync(source, root, { recursive: true });
  fs.writeFileSync(path.join(root,'agent/home/telemetry.json'),JSON.stringify({enabled:false}));
  const fixture=path.join(root,'web-smoke-fixture.txt');fs.writeFileSync(fixture,'CST_WEB_FIXTURE_OK');
  const seen = new Set();
  let requests = 0;
  const server = http.createServer(async (req, res) => {
    try {
      let raw = '';
      for await (const chunk of req) raw += chunk;
      const body = JSON.parse(raw);
      requests++;
      for (const tool of body.tools ?? []) seen.add(tool.function.name);
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      const web=raw.includes('CST_WEB_SMOKE'),tool=web&&body.messages.at(-1)?.role!=='tool';
      const delta=tool?{role:'assistant',tool_calls:[{index:0,id:'pack-read',type:'function',function:{name:'read',arguments:JSON.stringify({path:fixture})}}]}:{role:'assistant',content:web?'CST_WEB_SMOKE_OK':'CST_SMOKE_OK'};
      for (const choice of [{ delta, finish_reason: null }, { delta: {}, finish_reason: tool?'tool_calls':'stop' }]) {
        res.write(`data: ${JSON.stringify({ id: 'smoke', object: 'chat.completion.chunk', created: 1, model: 'smoke', choices: [{ index: 0, ...choice }] })}\n\n`);
      }
      res.end('data: [DONE]\n\n');
    } catch {
      res.writeHead(500);
      res.end('Invalid smoke request');
    }
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  try {
    fs.writeFileSync(path.join(root, 'agent/home/models.json'), JSON.stringify({ providers: { 'cst-build-smoke': {
      baseUrl: `http://127.0.0.1:${server.address().port}/v1`, api: 'openai-completions', apiKey: 'smoke-placeholder-not-a-secret',
      models: [{ id: 'smoke', reasoning: false, input: ['text'], contextWindow: 200000, maxTokens: 1000 }],
    } } }));
    // 不继承开发机的 API key、代理、预加载选项或 pi 配置。
    const env = {};
    for (const key of ['SystemRoot', 'WINDIR', 'ComSpec', 'PATHEXT', 'OS', 'SystemDrive', 'ProgramFiles', 'ProgramFiles(x86)', 'ProgramData']) {
      if (process.env[key]) env[key] = process.env[key];
    }
    for (const [key, relative] of Object.entries({ USERPROFILE: 'profile', LOCALAPPDATA: 'profile/Local', APPDATA: 'profile/Roaming', TEMP: 'profile/Temp', TMP: 'profile/Temp' })) {
      env[key] = path.join(workDir, relative);
      fs.mkdirSync(env[key], { recursive: true });
    }
    env.PATH = path.join(env.SystemRoot, 'System32');
    const command = `""${path.join(root, 'pi.cmd')}" --no-session --offline --provider cst-build-smoke --model smoke --thinking off --print CST_SMOKE"`;
    const runOnce = () => new Promise((resolve, reject) => {
      const child = spawn(env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', command], { cwd: root, env, windowsHide: true, windowsVerbatimArguments: true, stdio: ['ignore', 'pipe', 'pipe'] });
      let stdout = '', stderr = '';
      const timer = setTimeout(() => {
        spawnSync(path.join(env.SystemRoot, 'System32/taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true });
        reject(new Error('冒烟超过 90 秒'));
      }, 90000);
      child.stdout.on('data', (chunk) => { stdout += chunk; });
      child.stderr.on('data', (chunk) => { stderr += chunk; });
      child.on('error', (error) => { clearTimeout(timer); reject(error); });
      child.on('close', (code) => { clearTimeout(timer); code === 0 ? resolve(stdout) : reject(new Error(`冒烟退出码 ${code}: ${stderr}`)); });
    });
    const output = await runOnce();
    const warmOutput = await runOnce();
    if (!output.includes('CST_SMOKE_OK') || !warmOutput.includes('CST_SMOKE_OK') || requests !== 2) throw new Error('首次或再次启动的模拟模型回合未完成');
    for (const name of ['read', 'ls', 'disk', 'driver', 'eventlog', 'startup', 'sys', 'runbook', 'web_search', 'fetch_content']) {
      if (!seen.has(name)) throw new Error(`冒烟缺少工具: ${name}`);
    }
    for (const name of ['write', 'edit', 'bash', 'powershell']) {
      if (seen.has(name)) throw new Error(`冒烟发现不应向模型开放的工具: ${name}`);
    }
    await smokeWeb(root,workDir,env);
    fs.writeFileSync(path.join(workDir, 'smoke-result.json'), JSON.stringify({ passed: true, requests, tools: [...seen] }, null, 2));
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
}
