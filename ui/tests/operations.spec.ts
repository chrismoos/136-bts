import {identityLabel} from '../src/format';
import { test, expect } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
let process: ChildProcess;
let directory: string;
let address: string;
let logs = '';
test.beforeAll(async ({browser}) => {
 directory = await mkdtemp(join(tmpdir(), '136bts-browser-'));
 const config = join(directory,'config.yaml');
 await writeFile(config, `radio:\n  kind: noop\ncell:\n  band: cellular800\n  channel: 991\n  sid: 42\n  dvcc: 1\ncore:\n  state_path: ${join(directory,'state.sqlite3')}\nweb:\n  enabled: true\n  listen: 127.0.0.1:0\n`);
 process = spawn(globalThis.process.env.BTS136_BIN || resolve('../target/release/136-bts'), ['--run',config,'--duration-seconds','180']);
 process.stderr!.on('data', data => logs = (logs + data.toString()).slice(-20000));
 process.on('error', error => logs += String(error));
 await expect.poll(() => logs.match(/listen=127\.0\.0\.1:(\d+)/)?.[1], {timeout:20000}).toBeTruthy();
 address = 'http://127.0.0.1:' + logs.match(/listen=127\.0\.0\.1:(\d+)/)![1];
 const page=await browser.newPage();
 await page.goto(address+'/#subscribers');
 await expect(page.getByText('LIVE CELL',{exact:false})).toBeVisible();
 for(const [identity,number] of [['1','1001'],['2','1002']]) {
  await page.getByRole('button',{name:'Add subscriber',exact:true}).click();
 await page.getByLabel('Match phone by',{exact:true}).selectOption('identity');
  await page.getByLabel('Phone identity (decimal)').fill(identity);
  await page.getByLabel('Phone number',{exact:true}).fill(number);
  await page.getByRole('button',{name:'Save subscriber',exact:true}).click();
  await expect(page.getByRole('button',{name:number,exact:true})).toBeVisible();
 }
 await page.close();
});
test.afterAll(async () => {
 if (process?.exitCode === null) {
   const stopped = new Promise(resolve => process.once('exit',resolve));
   process.kill('SIGINT');
   await stopped;
   expect(logs).toContain('stop_acknowledged=true');
 }
 await rm(directory,{recursive:true,force:true});
});
test('bundled console receives live protobuf data and durably queues SMS', async ({page}) => {
 const errors: string[]=[];
 page.on('pageerror', error=>errors.push(error.message));
 await page.goto(address);
 await expect(page.getByText('LIVE CELL', {exact:false})).toBeVisible();
 await expect(page.getByRole('heading',{name:'Cell and carrier'})).toBeVisible();
 await expect(page.getByText('Downlink 869.04 MHz', {exact:false})).toBeVisible();
 await page.getByRole('link',{name:'Phones',exact:true}).click();
 await expect(page.getByRole('button',{name:'1001',exact:true})).toBeVisible();
 await page.getByRole('button',{name:'1001',exact:true}).click();
 await expect(page.getByRole('heading',{name:'Phone 1001'})).toBeVisible();
 await page.getByRole('link',{name:'SMS',exact:true}).click();
 await page.getByRole('textbox',{name:'From',exact:true}).fill('1001');await page.getByLabel('To',{exact:true}).fill('1002');
 await page.getByLabel('Message',{exact:true}).fill('Hello from the carrier console');
 await page.getByRole('button',{name:'Queue SMS',exact:true}).click();
 await expect(page.getByRole('status')).toContainText('Queued for delivery');
 await expect(page.getByRole('cell',{name:'Hello from the carrier console',exact:true})).toBeVisible();
 await page.reload();
 await expect(page.getByRole('cell',{name:'Hello from the carrier console',exact:true})).toBeVisible();
 await expect(page.getByText('Page 1 of 1',{exact:false})).toBeVisible();
 await page.getByRole('link',{name:'Calls',exact:true}).click();
 await expect(page.getByRole('cell',{name:'No active calls.'})).toBeVisible();
 await page.getByRole('link',{name:'Events',exact:true}).click();
 await page.getByRole('button',{name:'Hold',exact:true}).click();
 await expect(page.getByRole('button',{name:'Resume',exact:true})).toBeVisible();
 await page.getByRole('button',{name:'Resume',exact:true}).click();
 for(const width of [1440,420]) {
  await page.setViewportSize({width,height:900});
  for(const screen of ['Overview','Phones','Subscribers','Calls','SMS','Signaling','Radio','Events']) {
   await page.getByRole('link',{name:screen,exact:true}).click();
   expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBeTruthy();
  }
 }
 expect(errors).toEqual([]);
});

test('subscriber controls add, edit and remove provisioning', async ({page}) => {
 await page.goto(address+'/#subscribers');
 await expect(page.getByText('LIVE CELL',{exact:false})).toBeVisible();
 await page.getByRole('button',{name:'Add subscriber',exact:true}).click();
 await page.getByLabel('Match phone by',{exact:true}).selectOption('identity');
 await page.getByLabel('Phone identity (decimal)').fill('3');
 await page.getByLabel('Phone number',{exact:true}).fill('1003');
 await page.getByRole('button',{name:'Save subscriber',exact:true}).click();
 await expect(page.getByRole('button',{name:'1003',exact:true})).toBeVisible();
 const row=page.getByRole('row').filter({has:page.getByRole('button',{name:'1003',exact:true})});
 await row.getByRole('button',{name:'Edit',exact:true}).click();
 await page.getByLabel('Phone number',{exact:true}).fill('1004');
 await page.getByRole('button',{name:'Save subscriber',exact:true}).click();
 await expect(page.getByRole('button',{name:'1004',exact:true})).toBeVisible();
 await page.reload();
 await expect(page.getByRole('button',{name:'1004',exact:true})).toBeVisible();
 page.once('dialog',dialog=>dialog.accept());
 await page.getByRole('row').filter({has:page.getByRole('button',{name:'1004',exact:true})}).getByRole('button',{name:'Remove',exact:true}).click();
 await expect(page.getByRole('button',{name:'1004',exact:true})).toHaveCount(0);
 await page.getByRole('link',{name:'Signaling',exact:true}).click();
 await expect(page.getByRole('heading',{name:'Forward and reverse signaling'})).toBeVisible();
 await expect(page.getByText('F-BCCH',{exact:true})).toHaveCount(0);
 await page.getByLabel('Signaling view').selectOption('2');
 await expect(page.getByText('F-BCCH',{exact:true}).first()).toBeVisible();
});

test('maximum single-relay SMS stays durable and explains its call requirement', async ({page}) => {
 await page.goto(address+'/#sms');
 await expect(page.getByText('LIVE CELL',{exact:false})).toBeVisible();
 await page.getByRole('textbox',{name:'From',exact:true}).fill('1001');await page.getByLabel('To',{exact:true}).fill('1002');
 const text='x'.repeat(248);
 await expect(page.getByLabel('Message',{exact:true})).toHaveAttribute('maxlength','248');
 await page.getByLabel('Message',{exact:true}).fill(text);
 await page.getByRole('button',{name:'Queue SMS',exact:true}).click();
 await expect(page.getByRole('status')).toContainText('Queued for delivery');
 const row=page.getByRole('row').filter({has:page.getByRole('cell',{name:text,exact:true})});
 await expect(row).toBeVisible();
 await row.getByRole('button').click();
 await page.getByText('Delivery and protocol details',{exact:true}).click();
 await expect(page.getByText('Active call',{exact:true})).toBeVisible();
 await page.reload();
 await expect(page.getByRole('cell',{name:text,exact:true})).toBeVisible();
});

test('offline phones can request an audit without claiming registration', async ({page}) => {
 await page.goto(address+'/#phones');
 await expect(page.getByText('LIVE CELL',{exact:false})).toBeVisible();
 await page.getByRole('button',{name:'1001',exact:true}).click();
 await expect(page.getByRole('button',{name:'Refresh capabilities',exact:true})).toBeDisabled();
 await expect(page.getByText('Phone must register before requesting capabilities.',{exact:true})).toBeVisible();
 await expect(page.getByRole('button',{name:'Check phone response',exact:true})).toBeEnabled();
 await page.getByRole('button',{name:'Check phone response',exact:true}).click();
 await expect(page.getByRole('status')).toContainText('Audit order queued');
 await expect(page.getByRole('button',{name:'Request registration',exact:true})).toBeEnabled();
});

test('international SMS displays text and enforces encoded byte capacity', async ({page}) => {
 await page.goto(address);
 await expect(page.getByText('LIVE CELL',{exact:false})).toBeVisible();
 await page.getByRole('link',{name:'SMS',exact:true}).click();
 await page.getByRole('textbox',{name:'From',exact:true}).fill('1001');await page.getByLabel('To',{exact:true}).fill('1002');
 const input=page.getByLabel('Message',{exact:true});
 const submit=page.getByRole('button',{name:'Queue SMS',exact:true});
 for (const text of ['café déjà vu','中'.repeat(124),'é'.repeat(248)]) {
  await input.fill(text);
  await expect(submit).toBeEnabled();
  await submit.click();
  await expect(page.getByRole('status')).toContainText('Queued for delivery');
  await expect(page.getByRole('cell',{name:text,exact:true})).toBeVisible();
 }
 await input.fill('中'.repeat(125));
 await expect(submit).toBeDisabled();
 await expect(page.getByRole('alert')).toContainText('Message exceeds 248 encoded bytes');
 await input.fill('😀');
 await expect(submit).toBeDisabled();
 await expect(page.getByRole('alert')).toContainText('Supplementary characters');
});

test('offline phone can be paged with caller ID and the call canceled', async ({page}) => {
 await page.goto(address);
 await expect(page.getByText('LIVE CELL',{exact:false})).toBeVisible();
 await page.getByRole('link',{name:'Phones',exact:true}).click();
 await page.getByRole('button',{name:'1001',exact:true}).click();
 await page.getByLabel('Caller ID',{exact:true}).fill('2001');
 const call=page.getByRole('button',{name:'Call phone',exact:true});
 await expect(call).toBeEnabled();
 await call.click();
 await expect(page.getByRole('status')).toContainText('Call paging requested');
 await page.getByRole('link',{name:'Calls',exact:true}).click();
 await expect(page.getByRole('cell',{name:/2001/})).toBeVisible();
 await expect(page.getByRole('cell',{name:'Paging phone',exact:true}).first()).toBeVisible();
 page.once('dialog', dialog=>dialog.accept());
 await page.getByRole('button',{name:'End call',exact:true}).click();
 await expect(page.getByText('No active calls.',{exact:true})).toBeVisible();
});

test('signaling defaults to phone activity and supports server-side search',async({page})=>{
 await page.goto(address+'/#signaling');
 await expect(page.getByText('LIVE CELL',{exact:false})).toBeVisible();
 await expect(page.getByLabel('Signaling view')).toHaveValue('0');
 await expect(page.getByText('F-BCCH',{exact:true})).toHaveCount(0);
 await page.getByLabel('Signaling view').selectOption('2');
 await expect(page.getByText('F-BCCH',{exact:true}).first()).toBeVisible();
 await page.getByLabel('Find',{exact:true}).fill('no such message');
 await expect(page.getByText('No matching phone activity.',{exact:true})).toBeVisible();
 await page.getByLabel('Find',{exact:true}).fill('');
 await page.getByRole('button',{name:'Pause',exact:true}).click();
 await expect(page.getByRole('button',{name:'Resume',exact:true})).toBeVisible();
});

test('subscriber directory survives a cell restart without YAML provisioning',async({page})=>{
 const {readFile}=await import('node:fs/promises');
 const config=join(directory,'config.yaml');
 expect(await readFile(config,'utf8')).not.toContain('subscribers:');
 const stopped=new Promise(resolve=>process.once('exit',resolve));
 process.kill('SIGINT');await stopped;
 expect(logs).toContain('stop_acknowledged=true');logs='';
 process=spawn(globalThis.process.env.BTS136_BIN||resolve('../target/release/136-bts'),['--run',config,'--duration-seconds','60']);
 process.stderr!.on('data',data=>logs=(logs+data.toString()).slice(-20000));
 process.on('error',error=>logs+=String(error));
 await expect.poll(()=>logs.match(/listen=127\.0\.0\.1:(\d+)/)?.[1],{timeout:20000}).toBeTruthy();
 address='http://127.0.0.1:'+logs.match(/listen=127\.0\.0\.1:(\d+)/)![1];
 await page.goto(address+'/#subscribers');
 await expect(page.getByRole('button',{name:'1001',exact:true})).toBeVisible();
 await expect(page.getByRole('button',{name:'1002',exact:true})).toBeVisible();
 await expect(page.getByRole('button',{name:'1004',exact:true})).toHaveCount(0);
 expect(await readFile(config,'utf8')).not.toContain('subscribers:');
});

test('reverse signaling shows a numeric sync EVM meter',async({page})=>{
 const {create,toBinary}=await import('@bufbuild/protobuf');
 const {SignalingPageSchema}=await import('../src/gen/operations_pb');
 const body=toBinary(SignalingPageSchema,create(SignalingPageSchema,{
  total:1,messages:[{sequence:1n,unixMs:BigInt(Date.now()),direction:'Reverse',channel:'RACH',identity:'Min34(1)',name:'Registration',decoded:'Power up',reverseQuality:{syncEvmPercent:6.25,syncLevelDbfs:-24,frequencyOffsetHz:12.5,syncCorrelation:0.99,measuredUnixMs:BigInt(Date.now())}}],
 }));
 const trailer=Buffer.from('grpc-status: 0\r\n');
 const frame=Buffer.alloc(5+body.length+5+trailer.length);
 frame.writeUInt32BE(body.length,1);frame.set(body,5);
 frame[5+body.length]=128;frame.writeUInt32BE(trailer.length,6+body.length);frame.set(trailer,10+body.length);
 await page.route('**/bts136.operations.v1.Operations/ListSignaling',route=>route.fulfill({status:200,contentType:'application/grpc-web+proto',body:frame}));
 await page.goto(address+'/#signaling');
 await expect(page.locator('.reverse-quality b')).toHaveText('6.3%');
 await expect(page.getByLabel('Reverse sync EVM')).toHaveAttribute('value','6.25');
 await page.getByText('Requested registration',{exact:true}).click();
 await expect(page.getByText('Power up',{exact:true})).toBeVisible();
});

test('MIN labels show decoded digits without packed values', () => {
 expect(identityLabel('Min34(3410151054)')).toBe('MIN 3143781765');
 expect(identityLabel('Min34(0)')).toBe('MIN (invalid encoding)');
 expect(identityLabel('Min34(17179869184)')).toBe('MIN (invalid encoding)');
 expect(identityLabel('Tmsi20(7)')).toBe('Tmsi20(7)');
 expect(identityLabel('ESN(1589959607)')).toBe('Mobile identity not learned');
});

test('ESN subscribers can be provisioned without a mobile identity', async ({page}) => {
 await page.goto(address+'/#subscribers');
 await page.getByRole('button',{name:'Add subscriber',exact:true}).click();
 await expect(page.getByLabel('Match phone by',{exact:true})).toHaveValue('esn');
 await page.getByLabel('Phone number',{exact:true}).fill('1500');
 await page.getByLabel('ESN (hex)',{exact:true}).fill('5EC4DBB7');
 await page.getByRole('button',{name:'Save subscriber',exact:true}).click();
 const row=page.getByRole('row').filter({has:page.getByRole('button',{name:'1500',exact:true})});
 await expect(row).toContainText('0x5EC4DBB7');
 await page.reload();
 await expect(row).toContainText('0x5EC4DBB7');
 await row.getByRole('button',{name:'Edit',exact:true}).click();
 await expect(page.getByLabel('ESN (hex)',{exact:true})).toHaveValue('5EC4DBB7');
 await page.getByLabel('Phone number',{exact:true}).fill('1501');
 await page.getByRole('button',{name:'Save subscriber',exact:true}).click();
 await page.getByRole('button',{name:'1501',exact:true}).click();
 await expect(page.getByRole('button',{name:'Call phone',exact:true})).toBeDisabled();
 await expect(page.getByRole('button',{name:'Request registration',exact:true})).toBeDisabled();
 await expect(page.getByText('Waiting for the phone to register and report its ESN.',{exact:false})).toBeVisible();
 await page.goto(address+'/#subscribers');
 page.once('dialog',dialog=>dialog.accept());
 await page.getByRole('row').filter({has:page.getByRole('button',{name:'1501',exact:true})}).getByRole('button',{name:'Remove',exact:true}).click();
 await expect(page.getByRole('button',{name:'1501',exact:true})).toHaveCount(0);
});

test('live phone inserts and deletes preserve rows and the reading position', async ({page,context}) => {
 await page.setViewportSize({width:1200,height:400});
 await page.goto(address+'/#phones');
 await page.getByRole('button',{name:'1001',exact:true}).click();
 const row=page.getByRole('row').filter({has:page.getByRole('button',{name:'1001',exact:true})});
 await row.evaluate(element=>{(window as any).retainedPhoneRow=element;window.scrollTo(0,element.getBoundingClientRect().top+window.scrollY-50);});
 const top=await row.evaluate(element=>element.getBoundingClientRect().top);
 expect(await page.evaluate(()=>window.scrollY)).toBeGreaterThan(0);
 const editor=await context.newPage();
 await editor.goto(address+'/#subscribers');
 await editor.getByRole('button',{name:'Add subscriber',exact:true}).click();
 await editor.getByLabel('Phone number',{exact:true}).fill('0500');
 await editor.getByLabel('ESN (hex)',{exact:true}).fill('5EC40001');
 await editor.getByRole('button',{name:'Save subscriber',exact:true}).click();
 await expect(page.getByRole('button',{name:'0500',exact:true})).toBeVisible();
 await expect.poll(()=>row.evaluate((element,expectedTop)=>Math.abs(element.getBoundingClientRect().top-expectedTop),top)).toBeLessThan(2);
 expect(await row.evaluate(element=>element===(window as any).retainedPhoneRow)).toBe(true);
 editor.once('dialog',dialog=>dialog.accept());
 await editor.getByRole('row').filter({has:editor.getByRole('button',{name:'0500',exact:true})}).getByRole('button',{name:'Remove',exact:true}).click();
 await expect(page.getByRole('button',{name:'0500',exact:true})).toHaveCount(0);
 await expect.poll(()=>row.evaluate((element,expectedTop)=>Math.abs(element.getBoundingClientRect().top-expectedTop),top)).toBeLessThan(2);
 expect(await row.evaluate(element=>element===(window as any).retainedPhoneRow)).toBe(true);
 const revision=await page.locator('main').getAttribute('data-snapshot-revision');
 await expect.poll(()=>page.locator('main').getAttribute('data-snapshot-revision')).not.toBe(revision);
 await expect.poll(()=>row.evaluate((element,expectedTop)=>Math.abs(element.getBoundingClientRect().top-expectedTop),top)).toBeLessThan(2);
 await editor.close();
});

test('phone activity refreshes without replacing open message rows', async ({page}) => {
 const {create,fromBinary,toBinary}=await import('@bufbuild/protobuf');
 const {SignalingPageSchema,SignalingQuerySchema}=await import('../src/gen/operations_pb');
 const queries: {offset:number;direction:string}[]=[];
 await page.route('**/bts136.operations.v1.Operations/ListSignaling',async route=>{
  const request=route.request().postDataBuffer()!;
  const query=fromBinary(SignalingQuerySchema,request.subarray(5));
  const relevant=query.identity==='Min34(1)';
  if(relevant) queries.push({offset:query.offset,direction:query.direction});
  const registration={sequence:10n,unixMs:1n,direction:'Reverse',channel:'RACH',identity:'Min34(1)',name:'Registration',decoded:'Keep these details open',raw:new Uint8Array([1]),bitLength:8};
  const origination={...registration,sequence:20n,name:'Origination',decoded:'Called=0/0:1002'};
  const messages=!relevant?[]:query.direction?[{...registration,sequence:30n}]:queries.length>1?[origination,registration]:[registration];
  const body=toBinary(SignalingPageSchema,create(SignalingPageSchema,{total:messages.length,messages}));
  const trailer=Buffer.from('grpc-status: 0\r\n');
  const frame=Buffer.alloc(5+body.length+5+trailer.length);
  frame.writeUInt32BE(body.length,1);frame.set(body,5);
  frame[5+body.length]=128;frame.writeUInt32BE(trailer.length,6+body.length);frame.set(trailer,10+body.length);
  await route.fulfill({status:200,contentType:'application/grpc-web+proto',body:frame});
 });
 await page.goto(address+'/#phones');
 await page.getByRole('button',{name:'1001',exact:true}).click();
 const row=page.getByRole('row').filter({hasText:'Keep these details open'});
 await row.getByText('Requested registration',{exact:true}).click();
 await row.getByText('Raw protocol data',{exact:true}).click();
 await row.evaluate(element=>(window as any).activityRow=element);
 await expect.poll(()=>queries.length,{timeout:8000}).toBeGreaterThanOrEqual(3);
 expect(queries.every(query=>query.offset===0)).toBe(true);
 await expect(page.getByText('Requested a call to 1002',{exact:true})).toBeVisible();
 await expect(page.getByText('Keep these details open',{exact:true})).toBeVisible();
 expect(await row.evaluate(element=>element===(window as any).activityRow)).toBe(true);
 expect(await row.locator('details details').evaluate(element=>(element as HTMLDetailsElement).open)).toBe(true);
 await page.getByLabel('Direction').selectOption('Reverse');
 await expect.poll(()=>queries.some(query=>query.direction==='Reverse')).toBe(true);
});

test('SMS sender accepts typed numbers and compose fields align on desktop and mobile',async({page})=>{
 await page.goto(address+'/#sms');
 const from=page.getByRole('textbox',{name:'From',exact:true});
 const to=page.getByLabel('To',{exact:true});
 const message=page.getByLabel('Message',{exact:true});
 const fromBox=(await from.boundingBox())!,toBox=(await to.boundingBox())!,messageBox=(await message.boundingBox())!;
 expect(Math.abs(fromBox.y-toBox.y)).toBeLessThan(2);
 expect(Math.abs(fromBox.width-toBox.width)).toBeLessThan(2);
 expect(Math.abs(fromBox.x-messageBox.x)).toBeLessThan(2);
 expect(Math.abs(toBox.x+toBox.width-messageBox.x-messageBox.width)).toBeLessThan(2);
 expect(messageBox.y).toBeGreaterThan(fromBox.y+fromBox.height);
 await from.fill('18005551234');await to.fill('1001');await message.fill('Typed sender works');
 await page.getByRole('button',{name:'Queue SMS',exact:true}).click();
 await expect(page.getByRole('status')).toContainText('Queued for delivery');
 await expect(page.getByRole('row').filter({has:page.getByRole('cell',{name:'Typed sender works',exact:true})})).toContainText('18005551234 → 1001');
 await page.setViewportSize({width:390,height:800});
 const mobileFrom=(await from.boundingBox())!,mobileTo=(await to.boundingBox())!,mobileMessage=(await message.boundingBox())!;
 expect(mobileTo.y).toBeGreaterThan(mobileFrom.y+mobileFrom.height);
 expect(Math.abs(mobileFrom.x-mobileTo.x)).toBeLessThan(2);
 expect(Math.abs(mobileFrom.width-mobileMessage.width)).toBeLessThan(2);
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth)).toBe(true);
});

test('unprovisioned phones show reported ESNs and prefill subscriber creation',async({page})=>{
 const {create,fromBinary,toBinary}=await import('@bufbuild/protobuf');
 const {ObservedPhonePageSchema,ObservedPhoneQuerySchema}=await import('../src/gen/operations_pb');
 const observed=[
  {identity:'Min34(3410151054)',esn:0x5ec4dbb7,lastMessage:'Registration',lastResponseUnixMs:BigInt(Date.now())},
  {identity:'Min34(7)',lastMessage:'Origination',lastResponseUnixMs:BigInt(Date.now())},
 ];
 await page.route('**/bts136.operations.v1.Operations/ListObservedPhones',async route=>{
  const query=fromBinary(ObservedPhoneQuerySchema,route.request().postDataBuffer()!.subarray(5));
  const phones=observed.filter(phone=>!query.identity||phone.identity===query.identity);
  const body=toBinary(ObservedPhonePageSchema,create(ObservedPhonePageSchema,{phones,total:phones.length}));
  const trailer=Buffer.from('grpc-status: 0\r\n');
  const frame=Buffer.alloc(5+body.length+5+trailer.length);
  frame.writeUInt32BE(body.length,1);frame.set(body,5);
  frame[5+body.length]=128;frame.writeUInt32BE(trailer.length,6+body.length);frame.set(trailer,10+body.length);
  await route.fulfill({status:200,contentType:'application/grpc-web+proto',body:frame});
 });
 await page.goto(address+'/#phones');
 const row=page.getByRole('row').filter({has:page.getByRole('cell',{name:'0x5EC4DBB7',exact:true})});
 await expect(row).toContainText('MIN 3143781765');
 await expect(page.getByRole('cell',{name:'Not reported',exact:true})).toBeVisible();
 await row.getByRole('button',{name:'MIN 3143781765',exact:true}).click();
 await expect(page.getByRole('heading',{name:'Phone MIN 3143781765',exact:true})).toBeVisible();
 await expect(page.getByText('Reported ESN: 0x5EC4DBB7.',{exact:false})).toBeVisible();
 await row.getByRole('button',{name:'Add subscriber for this phone',exact:true}).click();
 await expect(page.getByLabel('Match phone by',{exact:true})).toHaveValue('esn');
 await expect(page.getByLabel('ESN (hex)',{exact:true})).toHaveValue('5EC4DBB7');
 await expect(page.getByLabel('Phone identity (decimal)',{exact:true})).toHaveCount(0);
});
