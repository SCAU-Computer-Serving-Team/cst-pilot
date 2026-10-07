import fs from 'node:fs';
import path from 'node:path';
import {spawn,spawnSync} from 'node:child_process';

// 只在冒烟副本中注入引导扩展。通过官方exe运行真实Web API、Pi和存储。
export async function smokeWeb(root,workDir,env){
 const home=path.join(root,'agent/home'),ready=path.join(workDir,'web-ready.json');
 const settings=JSON.parse(fs.readFileSync(path.join(home,'settings.json'),'utf8'));
 fs.writeFileSync(path.join(home,'settings.json'),JSON.stringify({...settings,defaultProvider:'cst-build-smoke',defaultModel:'smoke',retry:{enabled:false,maxRetries:0}}));
 const extension=path.join(home,'extensions/pack-web-probe.ts');
 fs.writeFileSync(extension,`import fs from 'node:fs';import {join} from 'node:path';import {fileURLToPath} from 'node:url';import {createWebApi} from './web/server/api.ts';import {createWebServer,listenWebServer} from './web/server/http.ts';import {WebSessionPool} from './web/server/session/sessions.ts';import {getWebContainer} from './web/server/container.ts';
export default function(pi){const c=getWebContainer();if(c.loadingWebSession>0)return;pi.on('session_start',async(e,ctx)=>{if(ctx.mode!=='rpc'||c.server)return;const agentDir=process.env.PI_CODING_AGENT_DIR;const pool=new WebSessionPool({cwd:ctx.cwd,agentDir,sessionDir:ctx.sessionManager.getSessionDir(),withLoader:async f=>{c.loadingWebSession++;try{return await f();}finally{c.loadingWebSession--;}}});const server=createWebServer(fileURLToPath(new URL('./web/static/',import.meta.url)),0,async()=>({sessions:await pool.list(),stage:'smoke'}));await listenWebServer(server,0);const port=server.address().port;const api=createWebApi(pool,agentDir,port,()=>ctx.shutdown());server.closeAllConnections();await new Promise(r=>server.close(r));const live=createWebServer(fileURLToPath(new URL('./web/static/',import.meta.url)),port,async()=>({sessions:await pool.list(),stage:'smoke'}),api);await listenWebServer(live,port);c.server=live;c.pool=pool;c.closeApi=api.close;fs.writeFileSync(${JSON.stringify(ready)},JSON.stringify({port}));});}`);
 const command=`""${path.join(root,'pi.cmd')}" --offline --mode rpc --provider cst-build-smoke --model smoke --thinking off"`;
 const child=spawn(env.ComSpec||'cmd.exe',['/d','/s','/c',command],{cwd:root,env,windowsHide:true,windowsVerbatimArguments:true,stdio:['pipe','pipe','pipe']});let stdout='',stderr='';child.stdout.on('data',c=>stdout+=c);child.stderr.on('data',c=>stderr+=c);let spawnError;child.on('error',e=>spawnError=e);
 const deadline=Date.now()+60000;const wait=async check=>{while(Date.now()<deadline){if(spawnError)throw spawnError;if(child.exitCode!==null)throw Error(`Web冒烟进程提前退出: ${child.exitCode} ${stderr}`);if(await check())return;await new Promise(r=>setTimeout(r,100));}throw Error('Web发行冒烟超时');};
 try{await wait(()=>fs.existsSync(ready));const {port}=JSON.parse(fs.readFileSync(ready,'utf8')),origin=`http://127.0.0.1:${port}`;
 const api=async(url,method='GET',body)=>{const response=await fetch(origin+url,{method,headers:method==='GET'?{}:{Origin:origin,'Content-Type':'application/json','X-CST-Web-Request':'1','Idempotency-Key':crypto.randomUUID()},body:method==='GET'?undefined:JSON.stringify(body??{})});if(!response.ok)throw Error(`Web冒烟HTTP ${response.status}: ${await response.text()}`);return response.json();};
 const page=await fetch(origin+'/');if(!page.ok||!(await page.text()).includes('/assets/'))throw Error('发行Web页面/静态引用缺失');
 const models=await api('/api/models');if(!models.models.some(m=>m.provider==='cst-build-smoke'&&m.id==='smoke'))throw Error('发行Web未读取隔离模型');
 const {id}=await api('/api/sessions','POST',{provider:'cst-build-smoke',modelId:'smoke'});
 await api(`/api/sessions/${id}/messages`,'POST',{id:crypto.randomUUID(),text:'CST_WEB_SMOKE',delivery:'queue'});let state;
 await wait(async()=>{state=await api(`/api/sessions/${id}`);return !state.running&&state.messages.some(m=>m.role==='assistant'&&m.content?.some(p=>p.text?.includes('CST_WEB_SMOKE_OK')));});
 if(!state.messages.some(m=>m.role==='toolResult'&&m.toolName==='read'&&m.content?.some(p=>p.text?.includes('CST_WEB_FIXTURE_OK'))))throw Error('发行Web未完成真实read工具链路');
 const result={passed:true,staticPage:true,models:true,read:true,session:id};fs.writeFileSync(path.join(workDir,'web-smoke-result.json'),JSON.stringify(result,null,2));
 await api('/api/lifecycle/exit','POST',{confirm:'stop'});
 // RPC宿主在下一条命令后消费SDK的shutdown请求；实际TUI宿主空闲时直接退出。
 await new Promise(r=>setTimeout(r,200));child.stdin.write('{"type":"get_state","id":"smoke-exit-wake"}\n');
 await new Promise((resolve,reject)=>{if(child.exitCode!==null)return resolve();const timer=setTimeout(()=>reject(Error('宿主退出超时')),10000);child.once('close',()=>{clearTimeout(timer);resolve();});});
 }finally{fs.writeFileSync(path.join(workDir,'web-smoke-stdout.log'),stdout);fs.writeFileSync(path.join(workDir,'web-smoke-stderr.log'),stderr);if(child.exitCode===null){spawnSync(path.join(env.SystemRoot,'System32/taskkill.exe'),['/PID',String(child.pid),'/T','/F'],{windowsHide:true});}fs.rmSync(extension,{force:true});}
}
