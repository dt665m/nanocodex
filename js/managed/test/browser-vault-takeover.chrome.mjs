// Human-only takeover protocol in real Chrome, using synthetic content and fake input.
import assert from 'node:assert/strict';
import {createNamecheapFixture,namecheapSynthetic} from './fixtures/namecheap-login.mjs';
import https from 'node:https';
import { readFileSync, mkdtempSync, rmSync, readdirSync, mkdirSync, writeFileSync } from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { registerHooks } from 'node:module';
// Cloudflare allocation/storage and the unused public browser factory are local
// adapters; the private runtime, HTTPS route, CDP and DOM all execute unchanged.
const browserAdapter = `export const createBrowserSession=(b,o)=>b.create(o);
export const deleteBrowserSession=(b,id)=>b.delete?.(id);
export const connectBrowser=()=>{throw Error('Unused public browser adapter')};
export class DurableBrowserSessionStore {
  constructor(storage){this.storage=storage}
  get(k){return this.storage.get(k)} set(k,v){return this.storage.put(k,v)} delete(k){return this.storage.delete(k)}
  async acquireLock(){return {release:async()=>{}}}
}`;
registerHooks({resolve(specifier, context, nextResolve) {
  if (specifier === 'agents/browser') return {url:'data:text/javascript,'+encodeURIComponent(browserAdapter),shortCircuit:true};
  if (specifier === 'agents/browser/ai') return {url:'data:text/javascript,export const createBrowserRuntime=()=>{throw Error("Provide local browser allocation")};',shortCircuit:true};
  return nextResolve(specifier.startsWith('./browser-') && !specifier.endsWith('.ts') ? specifier + '.ts' : specifier, context);
}});
const { createBrowserLoginRuntime } = await import('../src/browser-login-runtime.ts');
const { createManagedBrowserRuntime } = await import('../src/browser-runtime.ts');
const { browserTakeover, decodeVaultIntake } = await import('../../account/src/vaultIntake.ts');
const { PrivateBrowserCdp, fillBrowserVault } = await import('../src/browser-vault.ts');
const { default: WebSocket } = await import('ws');
const { privateVaultTakeover, releasePrivateVaultTakeover } = await import('../src/browser-vault-takeover.ts');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const temp = mkdtempSync(join(tmpdir(), 'private-touch-'));
const namecheapFixture=createNamecheapFixture();
let browser, server, chrome, privateCdp, loginRuntime, vaultRuntime, handleControl, loginBrowser, runtimeChrome;
try {
  execFileSync('openssl', ['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(temp,'key'),'-out',join(temp,'cert'),'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
  server = https.createServer({key:readFileSync(join(temp,'key')),cert:readFileSync(join(temp,'cert'))}, (req,res) => {
    if (namecheapFixture.handler(req,res)) return;
    if (req.url.startsWith('/v1/agents/')) { handleControl(req,res); return; }
    res.setHeader('Content-Type','text/html');
    if (req.url === '/otp') {
      res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><style>input{display:block;height:40px;margin:12px}</style><span id="code-label">Verification code</span> <span id="delivery-label">from your device</span><input id="otp" aria-labelledby="code-label delivery-label" aria-label="Fallback label" autocomplete="section-login one-time-code" inputmode="numeric"><input id="account" aria-label="Account" autocomplete="username webauthn"><input id="unsupported" aria-label="Other" autocomplete="arbitrary-private-marker" inputmode="none"><iframe title="Embedded unsupported input" srcdoc="<input autocomplete=one-time-code>"></iframe>');
      return;
    }
    res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><style>body{margin:20px;font:16px sans-serif;min-height:2800px}input{display:block;height:48px;width:90%;margin:16px 0;font:inherit}</style><h1>Private browser fixture</h1><input type="email" placeholder="Email"><input type="password" placeholder="Password"><textarea aria-label="Notes"></textarea><input type="hidden" value="never exposed"><input disabled placeholder="Disabled"><input readonly placeholder="Read only"><div contenteditable>Custom fallback</div><p>Swipe this page</p><script>window.counts={input:0,change:0};document.addEventListener("input",()=>counts.input++);document.addEventListener("change",()=>counts.change++);Object.defineProperty(document.querySelector("input[type=email]"),"value",{get(){return Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value").get.call(this)},set(){throw Error("framework setter must be bypassed")}})</script>');
  });
  await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
  const origin = `https://127.0.0.1:${server.address().port}`;
  chrome = spawn(process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    ['--headless','--ignore-certificate-errors','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${join(temp,'profile')}`,'about:blank'],{stdio:'ignore'});
  for (let i=0; i<100; i++) {
    try { readFileSync(join(temp,'profile','DevToolsActivePort')); break; }
    catch { await new Promise(resolve => setTimeout(resolve,100)); }
  }
  const [port, endpoint] = readFileSync(join(temp,'profile','DevToolsActivePort'),'utf8').trim().split('\n');
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  const context = await browser.newContext({ignoreHTTPSErrors:true});
  const page = await context.newPage();
  await page.goto(origin);
  const session = await context.newCDPSession(page);
  const socket = new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);
  await new Promise((resolve,reject) => { socket.once('open',resolve); socket.once('error',reject); });
  socket.accept = () => {};
  const cdp = privateCdp = new PrivateBrowserCdp(socket);
  const { targetInfo } = await session.send('Target.getTargetInfo');
  const identity={vault_id:'a'.repeat(22),expected_origin:origin,target_id:targetInfo.targetId}, touch={};
  const act = action => privateVaultTakeover(cdp,identity,action,touch);
  let frame=await act({action:'observe',viewport:{width:390,height:740,mobile:true}});
  assert.equal(frame.width,390); assert.equal(frame.height,740);
  assert.equal(frame.native_form,undefined,'legacy clients receive no new frame keys');
  frame=await act({action:'observe',native_fields:true});
  assert.deepEqual(frame.native_form.fields.map(f=>f.label),['Email','Password','Notes']);
  const batch = (view, values) => ({action:'fill_fields',document_id:view.native_form.document_id,fields:view.native_form.fields.map((f,i)=>({ref:f.ref,value:values[i]}))});
  const values=['synthetic@example.test','synthetic-password-78235','Unicode 🙂 notes'];
  const firstBatch=batch(frame,values);
  const started=performance.now(); frame=await act(firstBatch); const batchMs=Math.round(performance.now()-started);
  assert.deepEqual(await page.locator('input:not([type=hidden]):not([disabled]):not([readonly]),textarea').evaluateAll(es=>es.map(e=>e.value)),values);
  assert.deepEqual(await page.evaluate(()=>counts),{input:3,change:3});
  assert.ok(!JSON.stringify(frame.native_form).includes(values[1]));
  await assert.rejects(act(firstBatch),'replay must fail after rotation');
  frame=await act({action:'observe',native_fields:true});
  const removedBatch=batch(frame,['must-not-fill','must-not-fill','must-not-fill']);
  await page.locator('input[type=password]').evaluate(e=>e.replaceWith(e.cloneNode()));
  await assert.rejects(act(removedBatch));
  assert.equal(await page.locator('input[type=email]').inputValue(),values[0],'all refs validated before first mutation');
  frame=await act({action:'observe',native_fields:true});
  const coveredBatch=batch(frame,['must-not-fill','must-not-fill','must-not-fill']);
  await page.locator('input[type=password]').evaluate(e=>{const r=e.getBoundingClientRect(),overlay=document.createElement('div');overlay.id='fixture-overlay';Object.assign(overlay.style,{position:'fixed',left:r.left+'px',top:r.top+'px',width:r.width+'px',height:r.height+'px',zIndex:9999});document.body.append(overlay);});
  await assert.rejects(act(coveredBatch));
  assert.equal(await page.locator('input[type=email]').inputValue(),values[0],'occluded field rejects batch before mutation');
  await page.locator('#fixture-overlay').evaluate(e=>e.remove());
  frame=await act({action:'observe',native_fields:true});
  const staleBatch=batch(frame,['must-not-fill','must-not-fill','must-not-fill']);
  await page.reload(); await assert.rejects(act(staleBatch));
  assert.equal(await page.locator('input[type=email]').inputValue(),'');
  frame=await act({action:'observe',native_fields:true});
  const forged=batch(frame,values); forged.fields[0].ref=crypto.randomUUID();
  await assert.rejects(act(forged)); assert.equal(await page.locator('input[type=password]').inputValue(),'');
  frame=await act({action:'observe',native_fields:true});
  const duplicate=batch(frame,values); duplicate.fields[1].ref=duplicate.fields[0].ref;
  await assert.rejects(act(duplicate));
  // If CDP loses the response after applying values, the batch is consumed and
  // only an explicit observation recovers. Never replay possibly completed input.
  frame=await act({action:'observe',native_fields:true});
  const lostBatch=batch(frame,values);
  const lostResponse={attachTarget: target=>cdp.attachTarget(target),send:async(method,params,sid)=>{
    const result=await cdp.send(method,params,sid);
    if(method==='Runtime.callFunctionOn' && params.arguments?.[0]?.value===lostBatch.document_id) throw Error('Synthetic lost batch response');
    return result;
  }};
  await assert.rejects(privateVaultTakeover(lostResponse,identity,lostBatch,touch));
  assert.deepEqual(await page.locator('input:not([type=hidden]):not([disabled]):not([readonly]),textarea').evaluateAll(es=>es.map(e=>e.value)),values);
  await assert.rejects(act(lostBatch));
  frame=await act({action:'observe',native_fields:true});
  await act(batch(frame,['','','']));
  frame=await act({action:'observe',native_fields:false});
  assert.equal(frame.native_form,undefined,'client may explicitly return to the legacy viewport');
  const email=frame.inputs.find(input=>input.type==='email');
  assert.ok(email); assert.equal(frame.keyboard,undefined);
  const x=email.x+email.width/2,y=email.y+email.height/2;
  await act({action:'touch',phase:'start',x,y});
  frame=await act({action:'touch',phase:'end'});
  assert.deepEqual(frame.keyboard,{type:'email',multiline:false});
  await act({action:'edit',delete_backward:0,text:'fake🙂'});
  await act({action:'edit',delete_backward:1,text:'!'});
  assert.equal(await page.locator('input[type=email]').inputValue(),'fake!');
  await act({action:'key',key:'Tab'});
  frame=await act({action:'observe',native_fields:true});
  assert.deepEqual(frame.keyboard,{type:'password',multiline:false});
  await act({action:'touch',phase:'start',x:0.85,y:0.8});
  await act({action:'touch',phase:'move',x:0.85,y:0.55});
  await act({action:'touch',phase:'move',x:0.85,y:0.3});
  await act({action:'touch',phase:'end'});
  assert.ok(await page.evaluate(()=>scrollY)>100,'touch swipes must scroll the actual page');
  await act({action:'touch',phase:'start',x:0.5,y:0.5});
  await act({action:'observe',native_fields:true}); // Explicit recovery cancels the finger, never repeats input.
  assert.equal(touch.active,false);
  await assert.rejects(act({action:'touch',phase:'move',x:0.5,y:0.4}));
  // Simulate a failed edit response: no finger is active, but runtime marks
  // the lease uncertain and requires an explicit refresh before further input.
  const failedEditCdp = {attachTarget: target => cdp.attachTarget(target), send: async (method, params, sid) => {
    if (method === 'Input.insertText') throw new Error('Synthetic disconnected edit response');
    return cdp.send(method, params, sid);
  }};
  await assert.rejects(privateVaultTakeover(failedEditCdp,identity,{action:'edit',delete_backward:0,text:'synthetic'},touch));
  touch.uncertain = true;
  await act({action:'observe',native_fields:true});
  assert.equal(touch.uncertain,false);
  await releasePrivateVaultTakeover(cdp,identity.target_id);
  assert.notEqual((await session.send('Page.getLayoutMetrics')).cssLayoutViewport.clientWidth,390);
  // Public private-login runtime and shipped account decoder over HTTPS.
  // Only allocation/storage are local adapters; CDP, DOM and transport are real.
  // A synthetic JS login with no form must return action_required, not an
  // ambiguous failure. Methodless JS forms retain their submit handler.
  for(const mode of ['formless','methodless']) {
    await page.setContent((mode==='methodless'?'<form>':'')+'<input id="user" type="email"><input id="pass" type="password"><button type="button" id="login">Sign in</button>'+(mode==='methodless'?'</form>':'')+'<script>window.signedIn=false;document.querySelector("button").onclick=()=>window.signedIn=true;</script>');
    const fill=await fillBrowserVault({cdp,sessionId:'synthetic-vault',request:{...identity,username_selector:'#user',password_selector:'#pass',submit:true},resolve:async()=>({username:'fake@example.test',password:'synthetic-password'}),quarantine:async()=>{}});
    assert.deepEqual(fill,{status:'filled',submission:'action_required'});
    assert.equal(await page.evaluate(()=>signedIn),false);
    assert.equal(await page.locator('#pass').inputValue(),'synthetic-password');
  }
  const durable=new Map();
  const storage={get:async k=>structuredClone(durable.get(k)),put:async(k,v)=>durable.set(k,structuredClone(v)),delete:async k=>durable.delete(k),transaction:async f=>f(storage)};
  // Runtime owns a separate browser, as in production. Connecting Playwright
  // before Target.createTarget races its debugger-paused auto-attachment with
  // the runtime's Page.navigate. Attach the observer after creation/navigation.
  runtimeChrome=spawn(process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    ['--headless','--ignore-certificate-errors','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${join(temp,'runtime-profile')}`,'about:blank'],{stdio:'ignore'});
  for(let i=0;i<100;i++){try{readFileSync(join(temp,'runtime-profile','DevToolsActivePort'));break;}catch{await new Promise(resolve=>setTimeout(resolve,100));}}
  const [runtimePort,runtimeEndpoint]=readFileSync(join(temp,'runtime-profile','DevToolsActivePort'),'utf8').trim().split('\n');
  let allocations=0;
  const binding={create:async()=>{allocations++;return {sessionId:'native-fields-fixture'};},delete:async()=>{},fetch:async()=>{
    const socket=new WebSocket(`ws://127.0.0.1:${runtimePort}${runtimeEndpoint}`);
    await new Promise((resolve,reject)=>{socket.once('open',resolve);socket.once('error',reject);});socket.accept=()=>{};return {webSocket:socket};
  }};
  loginRuntime=createBrowserLoginRuntime({storage,browser:binding,agentId:'fixture-agent',authorize:ctx=>{if(ctx.sessionId!=='owner')throw Error('forbidden');}});
  const ctx={sessionId:'owner',callId:'fixture',signal:new AbortController().signal};
  const tool=(name,args)=>loginRuntime.tools.find(t=>t.name===name).handler(args,ctx);
  const operation=crypto.randomUUID();
  const login=await tool('request_browser_login',{operation_id:operation,url:origin,allowed_origins:[origin]});
  let activeId=operation;
  const human=action=>loginRuntime.submit({challenge_id:activeId,...action},ctx.signal);
  await assert.rejects(human({action:'observe',native_fields:true}));
  await human({action:'approve'});
  loginBrowser=await chromium.connectOverCDP(`http://127.0.0.1:${runtimePort}`);
  let loginPage;
  for(let i=0;i<100;i++){loginPage=loginBrowser.contexts().flatMap(c=>c.pages()).find(p=>p.url().startsWith(origin));if(loginPage)break;await new Promise(resolve=>setTimeout(resolve,50));}
  assert.ok(loginPage,'private login target navigated: '+JSON.stringify(loginBrowser.contexts().map(c=>c.pages().map(p=>p.url()))));
  await loginPage.locator('input[type=email]').waitFor();
  let intake={operation:'browser_login',kind:'login',agent_id:'fixture-agent',challenge_id:operation,request_id:operation,allowed_origins:[origin]};
  handleControl=(req,res)=>{let body='';req.on('data',c=>body+=c);req.on('end',async()=>{
    res.setHeader('content-type','application/json');res.setHeader('cache-control','no-store');
    try{res.end(JSON.stringify(await loginRuntime.submit(JSON.parse(body),ctx.signal)));}catch{res.statusCode=409;res.end('{}');}
  });};
  const requestPrivate=(url,init)=>new Promise((resolve,reject)=>{
    const req=https.request(new URL(url,origin),{method:init.method,headers:init.headers,rejectUnauthorized:false},res=>{
      let body='';res.on('data',c=>body+=c);res.on('end',()=>resolve(new Response(body,{status:res.statusCode,headers:res.headers})));
    });req.on('error',reject);req.end(init.body);
  });
  const webFrame=await browserTakeover(intake,{action:'observe'},requestPrivate);
  assert.equal(webFrame.status,'active');assert.equal(webFrame.native_form,undefined);
  const nativeFrame=await human({action:'observe',native_fields:true});
  assert.equal(nativeFrame.native_form.fields.length,3);
  // A legacy web observer explicitly switches the lease back to the old response schema.
  const legacyAgain=await human({action:'observe'}); assert.equal(legacyAgain.native_form,undefined);
  const decoded=await browserTakeover(intake,{action:'observe'},requestPrivate);
  assert.equal(decoded.status,'active');assert.equal(decoded.native_form,undefined);
  const beforeFill=await human({action:'observe',native_fields:true});
  await loginPage.locator('input[type=email]').evaluate(e=>e.multiple=true);
  const privateValues=['  synthetic@example.test \r\n, \tsecond@example.test  ','synthetic\r\n-password-78235','Unicode 🙂 notes\r\nwith newlines\rand more'];
  await browserTakeover(intake,batch(beforeFill,privateValues),requestPrivate);
  const normalized=await loginPage.locator('input:not([type=hidden]):not([disabled]):not([readonly]),textarea').evaluateAll(es=>es.map(e=>e.value));
  assert.deepEqual(normalized,['synthetic@example.test,second@example.test','synthetic-password-78235','Unicode 🙂 notes\nwith newlines\nand more']);
  await loginPage.evaluate(vals=>{const p=document.createElement('p');p.textContent=vals.join(' ');document.body.append(p);},normalized);
  // Native OTP sheet: the user types once into a labeled native field, with the
  // code keyboard/autofill purpose preserved, then sends a private batch over HTTPS.
  assert.equal((await browserTakeover(intake,{action:'finish'},requestPrivate)).status,'finished');
  const firstSnapshot=JSON.stringify(await tool('browser_login_snapshot',{request_id:operation}));
  for(const value of [...privateValues,...normalized])assert.ok(!firstSnapshot.includes(value));
  await loginPage.evaluate(()=>sessionStorage.setItem('synthetic-session-marker','retained'));
  await tool('browser_login_action',{request_id:operation,operation_id:crypto.randomUUID(),action:'navigate',url:origin+'/otp'});
  await loginPage.locator('#otp').waitFor();
  const beforeReentry=structuredClone(durable.get('browser-login:fixture-agent'));
  const reentryOperation=crypto.randomUUID();
  const followup=await tool('request_browser_login_input',{request_id:operation,operation_id:reentryOperation});
  assert.equal(followup.status,'input_required');assert.equal(followup.approved,true);
  assert.notEqual(followup.request_id,operation);assert.equal(followup.request_id,followup.challenge_id);
  assert.deepEqual(await tool('request_browser_login_input',{operation_id:reentryOperation,request_id:operation}),followup,'reordered retry replays the same fresh panel');
  assert.equal(allocations,1,'reentry reuses the original browser');
  const afterReentry=durable.get('browser-login:fixture-agent');
  assert.equal(afterReentry.sessionId,beforeReentry.sessionId);assert.equal(afterReentry.targetId,beforeReentry.targetId);
  assert.equal(await loginPage.evaluate(()=>sessionStorage.getItem('synthetic-session-marker')),'retained');
  for(const action of [{action:'observe'},{action:'approve'},{action:'cancel'},batch(beforeFill,privateValues)])
    await assert.rejects(loginRuntime.submit({challenge_id:operation,...action},ctx.signal),'old panel cannot control the new epoch');
  assert.deepEqual(await loginRuntime.submit({challenge_id:operation,action:'finish'},ctx.signal),{type:'browser_login_receipt',status:'finished',request_id:operation});
  assert.equal(durable.get('browser-login:fixture-agent').phase,'human','old finish receipt must not release the new epoch');
  await assert.rejects(tool('browser_login_snapshot',{request_id:followup.request_id}),'model stays blocked during native input');
  activeId=followup.request_id;
  intake=decodeVaultIntake({name:'request_browser_login_input',status:'completed',output:JSON.stringify(followup)});
  assert.ok(intake,'account client recognizes followup native intake');
  const described=await requestPrivate('/v1/agents/fixture-agent/browser-vault/takeover',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({challenge_id:activeId,action:'describe'})});
  assert.equal((await described.json()).approved,true,'authenticated describe skips repeated origin review');
  const oldNative=await human({action:'observe',native_fields:true});
  assert.ok(oldNative.native_form.fields.every(f=>Object.keys(f).sort().join(',')==='label,multiline,ref,type'),'old native clients receive the original descriptor keys');
  const hinted=await human({action:'observe',native_fields:true,native_field_hints:true});
  const descriptors=hinted.native_form.fields.map(({ref,...f})=>f);
  assert.deepEqual(descriptors,[
    {label:'Verification code from your device',type:'text',multiline:false,autocomplete:'one-time-code',inputmode:'numeric'},
    {label:'Account',type:'text',multiline:false,autocomplete:'username'},
    {label:'Other',type:'text',multiline:false},
  ]);
  assert.ok(!JSON.stringify(hinted.native_form).includes('webauthn'),'autocomplete is never represented as passkey capability');
  const otpValues=['783492','synthetic-otp-user','synthetic-other'];
  await browserTakeover(intake,batch(hinted,otpValues),requestPrivate);
  assert.deepEqual(await loginPage.locator('input').evaluateAll(es=>es.map(e=>e.value)),otpValues);
  const changedPurpose=await human({action:'observe',native_fields:true,native_field_hints:true});
  await loginPage.locator('#otp').evaluate(e=>e.autocomplete='cc-csc');
  await assert.rejects(browserTakeover(intake,batch(changedPurpose,['must-not-fill','must-not-fill','must-not-fill']),requestPrivate));
  assert.deepEqual(await loginPage.locator('input').evaluateAll(es=>es.map(e=>e.value)),otpValues,'changed purpose rejects the entire batch before mutation');
  await human({action:'observe',native_fields:true,native_field_hints:true});
  await loginPage.evaluate(vals=>{const p=document.createElement('p');p.textContent=vals.join(' ');document.body.append(p);},[...normalized,...otpValues]);
  assert.equal((await browserTakeover(intake,{action:'finish'},requestPrivate)).status,'finished');
  const snapshot=JSON.stringify(await tool('browser_login_snapshot',{request_id:activeId}));
  for(const value of [...privateValues,...normalized,...otpValues]){assert.ok(!snapshot.includes(value));assert.ok(!JSON.stringify([...durable]).includes(value));}
  await tool('browser_login_close',{});
  // Public Namecheap login structure, then a synthetic server-verified OTP.
  // No live Namecheap credentials, post-password markup or WebAuthn are used.
  await loginBrowser.close();loginBrowser=undefined;
  const namecheapOperation=crypto.randomUUID();
  const namecheapRequest=await tool('request_browser_login',{operation_id:namecheapOperation,url:origin+'/namecheap/login',allowed_origins:[origin]});
  activeId=namecheapOperation;
  intake=decodeVaultIntake({name:'request_browser_login',status:'completed',output:JSON.stringify(namecheapRequest)});
  assert.ok(intake);
  await human({action:'approve'});
  loginBrowser=await chromium.connectOverCDP(`http://127.0.0.1:${runtimePort}`);
  loginPage=loginBrowser.contexts().flatMap(c=>c.pages()).find(p=>p.url()===origin+'/namecheap/login');
  assert.ok(loginPage);
  await loginPage.locator('input[name=LoginPassword]:visible').waitFor();
  const namecheapFrame=await human({action:'observe',native_fields:true,native_field_hints:true,viewport:{width:390,height:740,mobile:true}});
  assert.deepEqual(namecheapFrame.native_form.fields.map(({ref,...field})=>field),[
    {label:'Username',type:'text',multiline:false},
    {label:'Password',type:'password',multiline:false},
  ],'hidden duplicates and offscreen newsletter excluded; placeholder-only labels survive');
  const namecheapValues=[namecheapSynthetic.username,namecheapSynthetic.password];
  await browserTakeover(intake,batch(namecheapFrame,namecheapValues),requestPrivate);
  assert.deepEqual(await loginPage.locator('input[name=LoginUserName],input[name=LoginPassword]').evaluateAll(es=>es.map(e=>e.value)),['','',...namecheapValues]);
  assert.deepEqual(namecheapFixture.counts,{passwordPosts:0,otpPosts:0,authenticatedVisits:0},'native fill never submits');
  const namecheapClickReceipts=[];
  const clickNamecheap=async label=>{
    assert.equal((await browserTakeover(intake,{action:'finish'},requestPrivate)).status,'finished');
    const snapshot=await tool('browser_login_snapshot',{request_id:activeId});
    const submit=snapshot.elements.find(el=>el.role==='button'&&el.text===label);
    assert.ok(submit,'agent can identify the form submit from the redacted snapshot');
    for(const value of Object.values(namecheapSynthetic))assert.ok(!JSON.stringify(snapshot).includes(value));
    const action={request_id:activeId,operation_id:crypto.randomUUID(),action:'click',snapshot_id:snapshot.snapshot_id,ref:submit.ref};
    const receipt=await tool('browser_login_action',action);
    assert.ok(['action_requested','outcome_unknown'].includes(receipt.status));
    assert.deepEqual(await tool('browser_login_action',action),receipt,'stable operation replay cannot submit twice');
    namecheapClickReceipts.push(receipt.status);
  };
  await clickNamecheap('Submit form');
  await loginPage.waitForURL(origin+'/namecheap/otp');
  assert.equal(namecheapFixture.counts.passwordPosts,1);
  assert.ok(JSON.stringify(await tool('browser_login_snapshot',{request_id:activeId})).includes('Synthetic second-factor code'));
  const namecheapState=structuredClone(durable.get('browser-login:fixture-agent'));
  const namecheapAllocations=allocations;
  const otpRequest=await tool('request_browser_login_input',{request_id:activeId,operation_id:crypto.randomUUID()});
  assert.equal(otpRequest.approved,true);
  assert.notEqual(otpRequest.request_id,activeId);
  const otpState=durable.get('browser-login:fixture-agent');
  assert.equal(otpState.targetId,namecheapState.targetId);assert.equal(otpState.sessionId,namecheapState.sessionId);
  assert.equal(allocations,namecheapAllocations);
  activeId=otpRequest.request_id;
  intake=decodeVaultIntake({name:'request_browser_login_input',status:'completed',output:JSON.stringify(otpRequest)});
  assert.ok(intake);
  const namecheapOtp=await human({action:'observe',native_fields:true,native_field_hints:true});
  assert.deepEqual(namecheapOtp.native_form.fields.map(({ref,...field})=>field),[{label:'Verification code',type:'text',multiline:false,autocomplete:'one-time-code',inputmode:'numeric'}]);
  await browserTakeover(intake,batch(namecheapOtp,[namecheapSynthetic.otp]),requestPrivate);
  assert.equal(namecheapFixture.counts.otpPosts,0,'OTP native fill never submits');
  await clickNamecheap('Verify');
  await loginPage.waitForURL(origin+'/namecheap/account');
  const namecheapSnapshot=JSON.stringify(await tool('browser_login_snapshot',{request_id:activeId}));
  assert.ok(namecheapSnapshot.includes('Synthetic Namecheap-shaped account verified'));
  assert.deepEqual(namecheapFixture.counts,{passwordPosts:1,otpPosts:1,authenticatedVisits:1});
  for(const value of Object.values(namecheapSynthetic)){
    assert.ok(!namecheapSnapshot.includes(value));assert.ok(!JSON.stringify([...durable]).includes(value));
  }
  const namecheapOutput=new URL('../../../output/private-native-fields/',import.meta.url);mkdirSync(namecheapOutput,{recursive:true});
  writeFileSync(new URL('namecheap-journey.json',namecheapOutput),JSON.stringify({
    public_source:'https://www.namecheap.com/myaccount/login/',observed:'2026-10-03',
    scope:'Synthetic structural compatibility only; no authenticated Namecheap or WebAuthn test',
    descriptors:namecheapFrame.native_form.fields.map(({ref,...field})=>field),
    otp_descriptors:namecheapOtp.native_form.fields.map(({ref,...field})=>field),
    merchant_counts:namecheapFixture.counts,click_receipts:namecheapClickReceipts,same_session:true,new_browser_allocations:allocations-namecheapAllocations,
    checks:['duplicate hidden header credentials left empty','offscreen newsletter excluded','placeholder-only labels and generic autocomplete=on','native HTTPS fill does not submit','native fill then Finish then redacted snapshot agent click reaches synthetic OTP','OTP reentry keeps target and pending session cookie','native code hints survive HTTPS','native OTP fill then Finish then redacted snapshot agent click reaches server-authenticated synthetic account','input submit has fixed label without exposing its value','stable action UUID replay submits each form once','no synthetic inputs in snapshot or durable metadata'],
    limitations:['OTP markup is synthetic, not inspected after a live Namecheap login','CAPTCHA/trusted-device challenges untested','no iOS system credential autofill or passkey assertion tested'],
  },null,2));
  await tool('browser_login_close',{});
  console.log('PASS: Namecheap-shaped native fill/Finish -> agent snapshot submit -> retained-session native OTP/Finish -> agent verify -> server-confirmed fixture account; no live Namecheap authentication or passkey claim');
  // Vault Finish: actual runtime and CDP, with HTTP response loss simulated only
  // after the real finish completed. Retry cannot release a newer lease.
  const vaultData=new Map();
  const vaultStorage={get:async k=>structuredClone(vaultData.get(k)),put:async(k,v)=>vaultData.set(k,structuredClone(v)),delete:async k=>vaultData.delete(k),transaction:async f=>f(vaultStorage)};
  const vaultBinding={fetch:async()=>{
    const ws=new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);
    await new Promise((resolve,reject)=>{ws.once('open',resolve);ws.once('error',reject);});ws.accept=()=>{};return {webSocket:ws};
  }};
  const makeVaultRuntime=()=>createManagedBrowserRuntime({ctx:{storage:vaultStorage},env:{MANAGED_BROWSER_PROVIDER:'cloudflare',BROWSER:vaultBinding,LOADER:{}},sessionId:'vault-fixture',privateOnly:true,
    resolveVaultLogin:async()=>({username:'synthetic-vault-user',password:'synthetic-vault-password'}),authorizeVaultAccess:()=>{},
    createRuntime:()=>({connector:{sessionInfo:async()=>({sessionId:'vault-session'}),closeSession:async()=>{}},tools:{},runtime:{expirePaused:async()=>{}}})});
  vaultRuntime=await makeVaultRuntime();
  const vaultTool=name=>vaultRuntime.tools.find(t=>t.name===name).handler(identity,ctx);
  const lease=await vaultTool('browser_vault_request_takeover');
  const vaultIntake={operation:'browser_takeover',kind:'login',agent_id:'vault-fixture',challenge_id:lease.challenge_id};
  handleControl=(req,res)=>{let body='';req.on('data',c=>body+=c);req.on('end',async()=>{
    res.setHeader('content-type','application/json');res.setHeader('cache-control','no-store');
    try{res.end(JSON.stringify(await vaultRuntime.submitVaultTakeover(JSON.parse(body),ctx.signal)));}catch{res.statusCode=409;res.end('{}');}
  });};
  const lostFinish=async(url,init)=>{const response=await requestPrivate(url,init);assert.equal(response.status,200);await response.body.cancel();throw Error('Synthetic lost Finish response');};
  await assert.rejects(browserTakeover(vaultIntake,{action:'finish'},lostFinish));
  assert.equal((await browserTakeover(vaultIntake,{action:'finish'},requestPrivate)).status,'finished');
  const newerLease=await vaultTool('browser_vault_request_takeover');
  assert.notEqual(newerLease.challenge_id,lease.challenge_id);
  assert.equal((await browserTakeover(vaultIntake,{action:'finish'},requestPrivate)).status,'finished');
  assert.equal(vaultData.get('browser-vault-takeover:private:cloudflare:vault-fixture').id,newerLease.challenge_id,'old finish does not release newer lease');
  await assert.rejects(vaultRuntime.submitVaultTakeover({challenge_id:lease.challenge_id,action:'observe'},ctx.signal));
  await assert.rejects(vaultRuntime.submitVaultTakeover({challenge_id:crypto.randomUUID(),action:'finish'},ctx.signal));
  assert.equal((await browserTakeover({...vaultIntake,challenge_id:newerLease.challenge_id},{action:'finish'},requestPrivate)).status,'finished');
  assert.equal((await browserTakeover(vaultIntake,{action:'finish'},requestPrivate)).status,'finished','first receipt survives second completion');
  await vaultRuntime.close();vaultRuntime=await makeVaultRuntime();
  assert.equal((await browserTakeover({...vaultIntake,challenge_id:newerLease.challenge_id},{action:'finish'},requestPrivate)).status,'finished','durable finish survives runtime recreation');
  assert.equal((await browserTakeover(vaultIntake,{action:'finish'},requestPrivate)).status,'finished','both epochs remain retryable after recreation');
  const output=new URL('../../../output/private-native-fields/',import.meta.url);mkdirSync(output,{recursive:true});
  writeFileSync(new URL('takeover-journey.json',output),JSON.stringify({batch_ms:batchMs,checks:['vault Finish response loss retries exact durable receipt','old vault Finish cannot release newer lease','both vault Finish epochs remain retryable after later completion and runtime recreation','same-session fresh-ID OTP reentry without allocation/navigation','reordered operation replay returns identical fresh panel','old panel input and cancel rejected; old finish does not release new epoch','authenticated describe reports prior approval','original and OTP secrets remain redacted after reentry','OTP autocomplete and numeric keyboard hints survive private HTTPS fill','aria-labelledby labels identify OTP fields','hint opt-in preserves old native descriptor schema','unrecognized hint values and iframe fields are excluded','webauthn suffix is not passkey capability','changed input purpose rejects batch before mutation','OTP values redacted from model snapshot and durable storage','legacy clients receive no native_form until explicit opt-in','explicit opt-out and legacy observation retain viewport','document-bound labels and types without values','single native setter plus input/change per field','Unicode batch','replayed batch rejected','lost batch response is consumed; explicit observation recovers','HTTPS account decoder accepts new optional metadata','browser-normalized CR/LF and multiple-email whitespace variants redacted','formless and methodless custom JS login returns filled/action_required','private-login batch values redacted from model snapshot and durable storage','replaced or occluded element rejects entire batch before mutation','same-origin reload rejects stale document','forged and duplicate refs rejected','viewport touch/keyboard fallback retained']},null,2));
  console.log('PASS: vault Finish loss/retry over HTTPS, newer-lease protection, durable receipt after runtime recreation');
  console.log('PASS: same-session OTP reentry, fresh challenge, replay, stale-panel fencing, redaction continuity');
  console.log('PASS: native OTP hints, accessible labels, private HTTPS fill, purpose-change rejection, legacy descriptor compatibility');
  console.log('PASS: HTTPS decoder compatibility, raw/normalized private-login snapshot redaction, synthetic JS custom-login action_required');
  console.log('PASS: native batched form fill, stale/replaced/forged refs fail closed; batch '+batchMs+'ms');
  console.log('PASS: mobile viewport, native touch focus, keyboard traits, Unicode edit/delete, real touch scrolling, cancel recovery, viewport cleanup');
} finally {
  await vaultRuntime?.close();
  await loginRuntime?.close();
  privateCdp?.close();
  await loginBrowser?.close();
  if(runtimeChrome && runtimeChrome.exitCode===null){const ended=new Promise(resolve=>runtimeChrome.once("exit",resolve));runtimeChrome.kill();await ended;}
  await browser?.close();
  if (chrome && chrome.exitCode === null) {
    const closed = new Promise(resolve => chrome.once('exit',resolve));
    chrome.kill(); await closed;
  }
  if(server) await new Promise(resolve=>server.close(resolve));
  rmSync(temp,{recursive:true,force:true,maxRetries:5,retryDelay:100});
}
