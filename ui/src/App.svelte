<script lang="ts">
import {smsTextInfo, maxSmsOctets} from './sms';
import { onMount, untrack } from 'svelte';
import {preserveViewport} from './liveUpdates';
import { create } from '@bufbuild/protobuf';
import { CallParty, EventLevel, SnapshotSchema, SubscriberSort, type Call, type Event, type ObservedPhone, type Snapshot, type Subscriber, type Sms, type Metric } from './gen/operations_pb';
import { Paged } from './paging.svelte';
import Pager from './Pager.svelte';
import { client } from './client';
import Metrics from './Metrics.svelte';
import AirMessages from './AirMessages.svelte';
import ReverseQuality from './ReverseQuality.svelte';
import {timestamp, identityLabel, esnLabel, errorText, cellPhaseLabel, callLabel, legLabel, smsPhaseLabel, eventLevelLabel, frequency} from './format';
import {CallPhase, CellPhase, SmsPhase, TrunkState} from './gen/operations_pb';
let snapshot: Snapshot = $state(create(SnapshotSchema));
let screen = $state('Overview');
let connected = $state(false);
let lastReceived = $state(0);
let now = $state(Date.now());
let error = $state('');
let connectionError = $state('');
let selectedNumber = $state('');
let callingNumber = $state('2001');
let selectedMessage = $state<Sms>();
let phoneFilter = $state('');
let sortKey = $state<'number'|'identity'|'state'>('number');
let descending = $state(false);
let eventLevel = $state('All');
let eventComponent = $state('');
let eventSearch = $state('');
let smsFilter = $state('All');
let sender = $state('');
let recipient = $state('');
let text = $state('');
let requestId = $state('');
let pending = $state(false);
let commandMessage = $state('');
const screens = ['Overview', 'Phones', 'Subscribers', 'Calls', 'SMS', 'Signaling', 'Radio', 'Events'];
let editing = $state(false);
let provisionMode = $state('esn');
let esnValue = $state('');
let editingWireId: Subscriber['subscriberId'] = $state();
let editingEsn = $state<number | undefined>();
let editingOriginalNumber = $state('');
const parsedEsn = () => /^(?:0x)?[0-9a-fA-F]{8}$/.test(esnValue.trim()) ? Number.parseInt(esnValue.trim().replace(/^0x/,''),16) : undefined;
let identityKind = $state(2);
let identityValue = $state('');
let directoryNumber = $state('');
let existingIdentity = $state(false);
function editSubscriber(item?: Subscriber) {
 navigate('Subscribers'); editing=true; existingIdentity=!!item;
 editingWireId=item?.subscriberId; editingEsn=item?.provisionedEsn; editingOriginalNumber=item?.number??'';
 provisionMode=!item || item.provisionedEsn!==undefined || item.esn!==undefined ? 'esn' : 'identity';
 esnValue=(item?.provisionedEsn??item?.esn)?.toString(16).toUpperCase().padStart(8,'0')??'';
 identityKind=item?.subscriberId?.kind??2; identityValue=item?.subscriberId?String(item.subscriberId.value):'';
 directoryNumber=item?.number??'';
}
async function saveSubscriber(event: SubmitEvent) {
 event.preventDefault(); if(stale || pending || provisioningError) return; pending=true; error='';
 try { const result = await client.saveSubscriber({subscriberId:provisionMode==='esn'?editingWireId:{kind:identityKind,value:BigInt(identityValue)},number:directoryNumber,esn:provisionMode==='esn'?parsedEsn():undefined}); commandMessage=result.message; editing=false; selectedIdentity=''; selectedNumber=directoryNumber; } catch(caught) { error=errorText(caught); } finally {pending=false;}
}
async function removeSubscriber(item: Subscriber) {
 if(stale || pending || !confirm(`Remove subscriber ${item.number}? SMS history is retained.`)) return; pending=true; error='';
 try {const result=await client.deleteSubscriber(item.provisionedEsn!==undefined?{esn:item.provisionedEsn}:item.subscriberId!); commandMessage=result.message; selectedNumber=''; selectedIdentity='';} catch(caught) {error=errorText(caught);} finally {pending=false;}
}
const value = (rows: Metric[], name: string) => rows.find(row => row.name === name)?.value ?? '-';
const hz = (text: string) => text === '-' ? text : frequency(text);
const date = (ms: bigint) => new Date(Number(ms)).toLocaleString();
const esn = (phone: Subscriber) => {const value=phone.provisionedEsn??phone.esn; return value===undefined?'-':'0x'+value.toString(16).toUpperCase().padStart(8,'0');};
let stale = $derived(!connected || now - lastReceived > 5000);
const trunkFault = (state: TrunkState) => state===TrunkState.BACKOFF || state===TrunkState.STOPPED;
const trunkLabel = (state: TrunkState) => ({[TrunkState.UNSPECIFIED]:'-',[TrunkState.DISABLED]:'Disabled',[TrunkState.CONNECTING]:'Connecting',[TrunkState.UP]:'Up',[TrunkState.BACKOFF]:'Retrying',[TrunkState.STOPPED]:'Stopped'}[state]);
let stopUnconfirmed = $derived(snapshot.phase === CellPhase.STOP_UNCONFIRMED);
let ended = $derived(stopUnconfirmed || snapshot.phase === CellPhase.STOPPED || snapshot.phase === CellPhase.FAILED);
let cellDetail = $derived(snapshot.failure ? `${cellPhaseLabel(snapshot.phase)}: ${snapshot.failure}` : cellPhaseLabel(snapshot.phase));
let banner = $derived(stopUnconfirmed ? `TX STOP NOT CONFIRMED. The radio did not acknowledge stop. Verify RF is off. ${snapshot.failure}`
 : ended ? `CELL STOPPED: ${cellDetail}`
 : stale ? 'CONNECTION LOST. Readings may be stale. Controls are disabled.'
 : `LIVE CELL: ${cellDetail}`);
const subscriberQuery = (offset:number, limit:number) => ({search:phoneFilter, sort:{number:SubscriberSort.NUMBER,identity:SubscriberSort.IDENTITY,state:SubscriberSort.REGISTRATION}[sortKey], descending, offset, limit});
const phonesPage = new Paged<Subscriber>(async (offset,limit) => {const page=await client.listSubscribers(subscriberQuery(offset,limit)); return {rows:page.subscribers,total:page.total};});
const subscribersPage = new Paged<Subscriber>(async (offset,limit) => {const page=await client.listSubscribers(subscriberQuery(offset,limit)); return {rows:page.subscribers,total:page.total};});
const observedPage = new Paged<ObservedPhone>(async (offset,limit) => {const page=await client.listObservedPhones({offset,limit}); return {rows:page.phones,total:page.total};});
let selectedIdentity = $state('');
const selectedPage = new Paged<Subscriber>(async () => {
 if(!selectedIdentity && !selectedNumber) return {rows:[],total:0};
 const page=await client.listSubscribers({identity:selectedIdentity,number:selectedIdentity?'':selectedNumber,limit:1}); return {rows:page.subscribers,total:page.total};
},1);
const selectedObserved = new Paged<ObservedPhone>(async () => {
 if(!selectedIdentity) return {rows:[],total:0};
 const page=await client.listObservedPhones({identity:selectedIdentity,limit:1}); return {rows:page.phones,total:page.total};
},1);
let phone = $derived(selectedIdentity||selectedNumber ? selectedPage.rows[0] : undefined);
let phoneKey = $derived(phone ? `${phone.identity}|${phone.number}` : '');
const RECENT_ROWS = 5;
const phoneCalls = new Paged<Call>(async (offset,limit) => {if(!phone) return {rows:[],total:0}; const page=await client.listCalls({identity:phone.identity,offset,limit}); return {rows:page.calls,total:page.total};},RECENT_ROWS);
const phoneSms = new Paged<Sms>(async (offset,limit) => {if(!phone) return {rows:[],total:0}; const page=await client.listMessages({number:phone.number,offset,limit}); return {rows:page.messages,total:page.total};},RECENT_ROWS);
let provisioningError=$derived(
 provisionMode==='esn' && (!parsedEsn()) ? 'Enter eight hexadecimal ESN digits, such as 5EC4DBB7.' :
 provisionMode==='identity' && (!/^\d+$/.test(identityValue)||BigInt(identityValue)===0n) ? 'Enter the positive decimal identity received from the phone.' :
 provisionMode==='identity' && BigInt(identityValue)>=(1n<<BigInt([20,24,34,50][identityKind]??0)) ? 'Identity is too large for the selected type.' :
 !/^\d+$/.test(directoryNumber) ? 'Enter a phone number using digits.' : '');
const routeIdentity = (item:Subscriber) => item.provisionedEsn!==undefined ? `ESN(${item.provisionedEsn})` : item.identity;
function openPhone(identity:string){
 selectedNumber='';selectedIdentity=identity;screen='Phones';
 location.hash='phones/'+encodeURIComponent(identity);
}
function provisionObserved(identity:string, esn?:number){editSubscriber();if(esn!==undefined){provisionMode='esn';esnValue=esn.toString(16).toUpperCase().padStart(8,'0');return;}provisionMode='identity';const match=identity.match(/^(Tmsi20|Tmsi24|Min34|Imsi50)\((\d+)\)$/);if(match){identityKind=['Tmsi20','Tmsi24','Min34','Imsi50'].indexOf(match[1]);identityValue=match[2];}}
const phoneLink=(identity:string)=>openPhone(identity);

const smsPhases: Record<string, SmsPhase> = {All:SmsPhase.UNSPECIFIED, Queued:SmsPhase.QUEUED, Delivered:SmsPhase.DELIVERED, Failed:SmsPhase.FAILED};
const smsPage = new Paged<Sms>(async (offset,limit,beforeId) => {const page=await client.listMessages({phase:smsPhases[smsFilter],offset,limit,beforeId:beforeId===undefined?undefined:beforeId}); return {rows:page.messages,total:page.total};},undefined,row=>row.id);
let message = $derived(selectedMessage && (smsPage.rows.find(row => row.id === selectedMessage!.id) ?? selectedMessage));
const eventLevels: Record<string, EventLevel> = {All:EventLevel.UNSPECIFIED, INFO:EventLevel.INFO, WARN:EventLevel.WARN, ERROR:EventLevel.ERROR};
let eventComponents = $state<string[]>([]);
const eventsPage = new Paged<Event>(async (offset,limit,beforeSequence) => {const page=await client.listEvents({level:eventLevels[eventLevel],component:eventComponent,search:eventSearch,offset,limit,beforeSequence}); eventComponents=page.components; return {rows:page.events,total:page.total};},undefined,row=>row.sequence);
const callsPage = new Paged<Call>(async (offset,limit) => {const page=await client.listCalls({offset,limit}); return {rows:page.calls,total:page.total};});
const activePages = () => screen==='Phones' ? [phonesPage, observedPage, selectedPage, selectedObserved, phoneCalls, phoneSms] : screen==='Subscribers' ? [subscribersPage] : screen==='Calls' ? [callsPage] : screen==='SMS' ? [smsPage] : screen==='Events' ? [eventsPage] : [];
$effect(() => { snapshot.revision; screen; untrack(() => { for (const page of activePages()) void page.refresh(); }); });
$effect(() => { phoneFilter; sortKey; descending; untrack(() => { phonesPage.reset(); subscribersPage.reset(); }); });
$effect(() => { selectedIdentity; selectedNumber; untrack(() => { selectedPage.reset(); selectedObserved.reset(); }); });
$effect(() => { phoneKey; untrack(() => { phoneCalls.reset(); phoneSms.reset(); }); });
$effect(() => { smsFilter; untrack(() => smsPage.reset()); });
$effect(() => { eventLevel; eventComponent; eventSearch; untrack(() => eventsPage.reset()); });
function navigate(name: string) { screen = name; location.hash = name.toLowerCase(); }
function sort(key: typeof sortKey) { if (sortKey === key) descending = !descending; else {sortKey = key; descending = false;} }
function compose(number: string) { recipient = number; navigate('SMS'); }
let smsInfo = $derived(smsTextInfo(text));
function changedMessage() { requestId = ''; commandMessage = ''; error = ''; }
async function sendSms(event: SubmitEvent) {
 event.preventDefault(); if (pending || stale || smsInfo.error) return;
 pending = true; commandMessage = ''; error = '';
 requestId ||= Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join('');
 try { const result = await client.sendSms({requestId, senderNumber: sender, recipientNumber: recipient, text}); commandMessage = `Message ${result.smsId}: ${result.message}`; text = ''; requestId = ''; }
 catch (caught) { error = errorText(caught); }
 finally { pending = false; }
}
async function callPhone(phone:Subscriber) {
 if (!phone.subscriberId || pending || stale) return;
 pending=true;error='';commandMessage='';
 try {const result=await client.startCall({subscriberId:phone.subscriberId,callingNumber});commandMessage=`Call ${result.callId}: ${result.message}`;} catch(caught) {error=errorText(caught);} finally {pending=false;}
}
async function auditPhone(phone:Subscriber,forceRegistration:boolean) {
 if (!phone.subscriberId) return;
 pending=true;error='';commandMessage='';
 try {const result=await client.auditSubscriber({subscriberId:phone.subscriberId,forceRegistration});commandMessage=result.message;} catch(caught) {error=errorText(caught);} finally {pending=false;}
}
function capabilityBlockReason(phone:Subscriber):string {
 if(stale) return 'Waiting for a live cell connection.';
 if(pending) return 'Another command is in progress.';
 if(!phone.registered) return 'Phone must register before requesting capabilities.';
 if(snapshot.calls.some(call=>call.legs.some(leg=>leg.identity===phone.identity))) return 'Finish the active call first.';
 return '';
}
async function requestCapabilities(phone:Subscriber) {
 if (!phone.subscriberId) return;
 pending=true; error=''; commandMessage='';
 try {const result=await client.requestCapabilities(phone.subscriberId);commandMessage=result.message;} catch(caught) {error=errorText(caught);} finally {pending=false;}
}
async function requestStatus(callId:bigint, party:string) {
 pending=true; error=''; commandMessage='';
 try {const result=await client.requestStatus({callId,party:party==='Caller'?CallParty.CALLER:CallParty.CALLEE}); commandMessage=result.message;} catch(caught) {error=errorText(caught);} finally {pending=false;}
}
async function setMeasurements(callId:bigint, party:string, enabled:boolean) {
 if(stale||pending) return; pending=true; error='';
 try {const result=await client.setMeasurements({callId,party:party==='Caller'?CallParty.CALLER:CallParty.CALLEE,enabled}); commandMessage=result.message;} catch(caught) {error=errorText(caught);} finally {pending=false;}
}
async function release(id: bigint) {
 if (stale || !confirm(`Release call ${id}? The call will be cleared.`)) return;
 try { const result = await client.releaseCall({callId:id}); commandMessage = result.message; }
 catch (caught) { error = errorText(caught); }
}
onMount(() => {
 const abort = new AbortController();
 const readHash = () => { const [route,id]=location.hash.slice(1).split('/'); const name = screens.find(name => name.toLowerCase() === route); if(name) screen = name; if(route==='phones'&&id){selectedIdentity=decodeURIComponent(id);selectedNumber='';} };
 readHash(); window.addEventListener('hashchange',readHash);
 const timer = setInterval(() => now = Date.now(),1000);
 void (async () => {
  while (!abort.signal.aborted) {
   try {
    for await (const next of client.watch({}, {signal:abort.signal})) {
      preserveViewport(() => {
       snapshot=next;
       connected=true;lastReceived=Date.now();connectionError='';
      });
    }
   } catch (caught) { if(!abort.signal.aborted) connectionError = errorText(caught); }
   connected = false;
   if (!abort.signal.aborted) await new Promise(resolve => setTimeout(resolve,1000));
  }
 })();
 return () => { abort.abort(); clearInterval(timer); window.removeEventListener('hashchange',readHash); };
});
</script>
<div class="frame">
<header class="masthead"><div class="brand"><svg class="slot-mark" viewBox="0 0 62 22" aria-hidden="true"><rect x="0" y="0" width="8" height="22" rx="1.5" class="on"/><rect x="10.8" y="0" width="8" height="22" rx="1.5"/><rect x="21.6" y="0" width="8" height="22" rx="1.5"/><rect x="32.4" y="0" width="8" height="22" rx="1.5" class="on"/><rect x="43.2" y="0" width="8" height="22" rx="1.5"/><rect x="54" y="0" width="8" height="22" rx="1.5"/></svg><div><div class="wordmark">136BTS</div><div class="sub">{value(snapshot.carrier,'Band')} · Channel {value(snapshot.carrier,'Channel')} · SID {value(snapshot.carrier,'SID')}</div></div></div><div class="who">{new Date(now).toLocaleString()}</div></header>
<div class="advisory" class:stale={stale||ended}>{banner}</div>
<div class="health-strip"><span>Console: <b>{stale?'Disconnected':'Connected'}</b></span><span>Cell: <b>{cellPhaseLabel(snapshot.phase)}</b></span><span>Last uplink: <b>{timestamp(snapshot.lastUplinkUnixMs)}</b></span>{#if snapshot.sip}<span class:error={trunkFault(snapshot.sip.state)} title={snapshot.sip.lastError}>SIP: <b>{trunkLabel(snapshot.sip.state)}</b></span>{/if}{#if snapshot.smsHub && snapshot.smsHub.state!==TrunkState.DISABLED}<span class:error={trunkFault(snapshot.smsHub.state)} title={snapshot.smsHub.lastError}>SMS hub: <b>{trunkLabel(snapshot.smsHub.state)}</b></span>{/if}</div>
<nav class="tabs" aria-label="Main navigation">{#each screens as name}<a class:on={screen===name} href={'#'+name.toLowerCase()} onclick={()=>{screen=name;if(name==='Phones'){selectedIdentity='';selectedNumber='';}}}>{name}</a>{/each}</nav>
<div class="body"><main class="main" data-snapshot-revision={String(snapshot.revision)}>
<h2 class="screen">{screen}</h2>
{#if connectionError}<p class="error" role="status">Console connection: {connectionError}</p>{/if}
{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if commandMessage}<p class="notice" role="status">{commandMessage}</p>{/if}
{#if screen==='Overview'}
<div class="summary-cards"><section><strong>{snapshot.registeredPhones}</strong><span>Registered phones</span><a href="#phones">View phones</a></section><section><strong>{snapshot.calls.length}</strong><span>Active calls</span><a href="#calls">View calls</a></section><section><strong>{snapshot.queuedSms??'-'}</strong><span>Queued SMS</span><a href="#sms">View SMS</a></section></div>
<div class="cols"><section class="panel"><h3>Cell and carrier</h3><div class="inner"><p>Channel {value(snapshot.carrier,'Channel')} · SID {value(snapshot.carrier,'SID')}</p><p>Downlink {hz(value(snapshot.carrier,'Forward TX'))} · Uplink {hz(value(snapshot.carrier,'Reverse RX'))}</p><a href="#radio">Radio details and stream health</a></div></section></div>
<AirMessages compact onphone={phoneLink}/>
{/if}
{#if screen==='Phones'}
<section class="panel"><h3>Phones</h3><div class="inner"><label>Find phone <input bind:value={phoneFilter} placeholder="Number, identity or ESN"></label> <a href="#subscribers">Manage subscribers</a><p class="muted">Registered means accepted during this run. Last response shows when the phone actually sent a decoded message.</p></div><div class="scroll"><table class="data"><thead><tr><th>Phone</th><th>Registration</th><th>Last registration</th><th>Last response</th><th>Reverse signal</th><th>Actions</th></tr></thead><tbody>
{#each phonesPage.rows as item (routeIdentity(item))}<tr data-live-row><td><button class="table-action" onclick={()=>openPhone(routeIdentity(item))}>{item.number}</button><small class="mono">ESN: {esn(item)} · {identityLabel(item.identity)}</small></td><td>{!item.subscriberId?'Waiting for first registration':item.registered?'Registered':'Not registered'}</td><td>{timestamp(item.lastRegistrationUnixMs)}</td><td>{timestamp(item.lastResponseUnixMs)}</td><td><ReverseQuality quality={item.reverseQuality} compact/></td><td><button disabled={!item.subscriberId} title={!item.subscriberId?"Wait for the first registration to learn the mobile identity":""} onclick={()=>compose(item.number)}>Send SMS</button> <button onclick={()=>openPhone(routeIdentity(item))}>Details</button></td></tr>{:else}<tr><td colspan="6">No subscribers yet. Add one in Subscribers or provision an observed phone below.</td></tr>{/each}</tbody></table></div><Pager page={phonesPage}/>{#if phonesPage.error}<p class="error inner">{phonesPage.error}</p>{/if}</section>
{#if observedPage.total}<section class="panel"><h3>Observed phones without provisioning</h3><div class="scroll"><table class="data"><thead><tr><th>Phone identity</th><th>Reported ESN</th><th>Last response</th><th>Message</th><th>Action</th></tr></thead><tbody>{#each observedPage.rows as item (item.identity)}<tr data-live-row><td><button onclick={()=>openPhone(item.identity)}>{identityLabel(item.identity)}</button></td><td>{esnLabel(item.esn)}</td><td>{timestamp(item.lastResponseUnixMs)}</td><td>{item.lastMessage}</td><td><button onclick={()=>provisionObserved(item.identity,item.esn)}>Add subscriber for this phone</button></td></tr>{/each}</tbody></table></div><Pager page={observedPage}/></section>{/if}
{#if phone}
<section class="panel"><h3>Phone {phone.number}</h3><div class="inner"><p>Registration: <b>{phone.registered?'Accepted':'Not registered'}</b> · Last response: {timestamp(phone.lastResponseUnixMs)}</p><div class="toolbar"><label>Caller ID <input aria-label="Caller ID" bind:value={callingNumber} size="16"/></label><button disabled={stale||pending||!phone.subscriberId||snapshot.calls.length>0||!/^\d+$/.test(callingNumber)} onclick={()=>callPhone(phone)}>Call phone</button><button disabled={!phone.subscriberId} onclick={()=>compose(phone.number)}>Send SMS</button><button disabled={stale||pending} onclick={()=>editSubscriber(phone)}>Edit subscriber</button></div><p class="muted">Calls play sample audio after the phone answers.</p><ReverseQuality quality={phone.reverseQuality} live/><div class="toolbar"><button disabled={stale||pending||!phone.subscriberId||snapshot.calls.some(c=>c.legs.some(l=>l.identity===phone.identity))} onclick={()=>auditPhone(phone,false)}>Check phone response</button><button disabled={stale||pending||!phone.subscriberId||snapshot.calls.some(c=>c.legs.some(l=>l.identity===phone.identity))} onclick={()=>auditPhone(phone,true)}>Request registration</button><button disabled={!!capabilityBlockReason(phone)} title={capabilityBlockReason(phone)} onclick={()=>requestCapabilities(phone)}>Refresh capabilities</button></div>{#if capabilityBlockReason(phone)}<p class="muted">{capabilityBlockReason(phone)}</p>{/if}
<details><summary>Technical details and capabilities</summary><p>Identity {identityLabel(phone.identity)} · Protocol {phone.protocolVersion??'Not reported'} · ESN: {esn(phone)}</p><Metrics title="Reported phone details" rows={phone.measurements}/></details></div></section>
<section class="panel"><h3>Recent calls and SMS for {phone.number}</h3><div class="inner">{#each [...snapshot.calls.filter(c=>c.legs.some(l=>l.identity===phone.identity)),...phoneCalls.rows].slice(0,RECENT_ROWS) as call}<p>Call {String(call.id)} · {call.legs.map(l=>l.number).join(' → ')} · {callLabel(call)}</p>{:else}<p>No recent calls for this phone.</p>{/each}{#each phoneSms.rows as sms}<p>SMS {String(sms.id)} · {sms.sender} → {sms.recipient} · {smsPhaseLabel(sms.phase)} · {sms.text}</p>{:else}<p>No recent SMS for this phone.</p>{/each}</div></section>
{#if phone.subscriberId}<AirMessages identity={phone.identity} onphone={phoneLink}/>{:else}<p class="muted">Waiting for the phone to register and report its ESN. Calls and SMS become available after its mobile identity is learned.</p>{/if}
{:else if selectedIdentity}<section class="panel"><h3>Phone {identityLabel(selectedIdentity)}</h3><div class="inner"><p>This phone has no subscriber record. Reported ESN: {esnLabel(selectedObserved.rows[0]?.esn)}.</p><button onclick={()=>provisionObserved(selectedIdentity,selectedObserved.rows[0]?.esn)}>Add subscriber for this phone</button></div></section><AirMessages identity={selectedIdentity} onphone={phoneLink}/>{/if}
{/if}
{#if screen==='Subscribers'}
<section class="panel"><h3>Subscriber directory</h3><div class="inner"><p>Add, edit or remove subscribers here. Changes are saved in SQLite and survive restarts. No YAML edits are needed.</p><button disabled={stale||pending} onclick={()=>editSubscriber()}>Add subscriber</button> <label>Find <input bind:value={phoneFilter} placeholder="Number or identity"></label></div><div class="scroll"><table class="data"><thead><tr><th>Phone number</th><th>Phone identity</th><th>Actions</th></tr></thead><tbody>{#each subscribersPage.rows as item (routeIdentity(item))}<tr data-live-row><td><button onclick={()=>openPhone(routeIdentity(item))}>{item.number}</button></td><td class="mono">ESN: {esn(item)}<small>{identityLabel(item.identity)}</small></td><td><button disabled={stale||pending} onclick={()=>editSubscriber(item)}>Edit</button> <button disabled={stale||pending} onclick={()=>removeSubscriber(item)}>Remove</button></td></tr>{:else}<tr><td colspan="3">No subscribers. Add a subscriber or use an observed phone on the Phones page.</td></tr>{/each}</tbody></table></div><Pager page={subscribersPage}/>{#if subscribersPage.error}<p class="error inner">{subscribersPage.error}</p>{/if}</section>
{#if editing}<section class="panel"><h3>{existingIdentity?'Edit subscriber':'Add subscriber'}</h3><div class="inner"><form class="subscriber-form" onsubmit={saveSubscriber}><label>Phone number <input aria-label="Phone number" type="tel" bind:value={directoryNumber} required pattern="[0-9]+" placeholder="1500"><small>Local number used for calls and SMS.</small></label><label>Match phone by <select aria-label="Match phone by" bind:value={provisionMode} disabled={editingEsn!==undefined}><option value="esn">ESN (recommended)</option><option value="identity">Mobile identity (advanced)</option></select></label>
{#if provisionMode==='esn'}<label>ESN (hex) <input aria-label="ESN (hex)" bind:value={esnValue} readonly={editingEsn!==undefined} required placeholder="5EC4DBB7"><small>The handset serial number. Its MIN or IMSI is learned when it registers.</small></label>{:else}<label>Identity type <select bind:value={identityKind} disabled={existingIdentity}><option value={2}>MIN34</option><option value={3}>IMSI50</option><option value={0}>TMSI20</option><option value={1}>TMSI24</option></select></label><label>Phone identity (decimal) <input aria-label="Phone identity (decimal)" bind:value={identityValue} readonly={existingIdentity} required pattern="[0-9]+" placeholder="3410151054"><small>Use the packed decimal identity from Phones or Signaling. This is separate from the local phone number.</small></label>{/if}{#if provisioningError}<p class="muted">{provisioningError}</p>{/if}<div class="toolbar"><button disabled={stale||pending||!!provisioningError}>Save subscriber</button><button type="button" onclick={()=>editing=false}>Cancel</button></div></form><p class="muted">An existing ESN or mobile identity cannot be changed. A conflicting learned identity is rejected rather than reassigned automatically. Finish active calls or SMS procedures before editing. Removal preserves message history.</p></div></section>{/if}
{/if}
{#if screen==='Calls'||screen==='Overview'}
<section class="panel"><h3>Calls in progress</h3><div class="scroll"><table class="data"><thead><tr><th>Call</th><th>Participants</th><th>Status</th><th>Action</th></tr></thead><tbody>{#each snapshot.calls as call (call.id)}<tr data-live-row><td>{String(call.id)}</td><td>{#each call.legs as leg,i}{#if i>0} → {/if}{#if leg.identity}<button class="table-action" onclick={()=>openPhone(leg.identity)}>{leg.number}</button>{:else}{leg.number||'Sample audio'}{/if}{/each}</td><td>{callLabel(call)}</td><td><button disabled={stale} onclick={()=>release(call.id)}>End call</button></td></tr>{:else}<tr><td colspan="4">No active calls.</td></tr>{/each}</tbody></table></div></section>
{#if screen==='Calls'}{#each snapshot.calls as call}<details class="technical"><summary>Call {String(call.id)} technical details</summary><Metrics title="Call timers" rows={call.measurements}/>{#each call.legs as leg}<section class="panel"><h3>{leg.number||'Sample audio'}</h3><div class="inner"><p>{leg.identity||'Local audio source'} · {legLabel(leg)} · Channel {leg.channel||'-'} · Slots {leg.slots||'-'}</p>{#if leg.identity}<div class="toolbar"><button disabled={stale||pending||call.phase!==CallPhase.CONNECTED} onclick={()=>setMeasurements(call.id,leg.party,true)}>Start measurement reports</button><button disabled={stale||pending||call.phase!==CallPhase.CONNECTED} onclick={()=>setMeasurements(call.id,leg.party,false)}>Stop measurement reports</button><button disabled={stale||pending||call.phase!==CallPhase.CONNECTED} onclick={()=>requestStatus(call.id,leg.party)}>Refresh phone status</button></div>{/if}<Metrics title="Measurements" rows={leg.measurements}/></div></section>{/each}</details>{/each}
<section class="panel"><h3>Recent calls this run</h3><div class="scroll"><table class="data"><thead><tr><th>Call</th><th>Participants</th><th>Outcome</th><th>Duration</th></tr></thead><tbody>{#each callsPage.rows as call (call.id)}<tr><td>{String(call.id)}</td><td>{call.legs.map(l=>l.number).join(' → ')}</td><td>{callLabel(call)}</td><td>{value(call.measurements,'Duration')} ms</td></tr>{:else}<tr><td colspan="4">No completed calls this run.</td></tr>{/each}</tbody></table></div><Pager page={callsPage}/></section>{/if}
{/if}
{#if screen==='SMS'}
<section class="panel"><h3>Send SMS</h3><div class="inner"><form class="sms-compose" onsubmit={sendSms}>
<div class="sms-addresses"><label>From <input aria-label="From" type="tel" inputmode="numeric" pattern="[0-9]+" required bind:value={sender} oninput={changedMessage}><small>Enter the sender number to display.</small></label>
<label>To <input aria-label="To" type="tel" inputmode="numeric" pattern="[0-9]+" required bind:value={recipient} oninput={changedMessage}><small>Enter a local subscriber number.</small></label></div>
<label class="sms-message">Message <textarea aria-label="Message" bind:value={text} oninput={changedMessage} maxlength="248" required rows="3"></textarea></label>
<button disabled={stale||pending||!/^\d+$/.test(sender)||!recipient||!text||!!smsInfo.error}>{pending?'Submitting…':'Queue SMS'}</button><p class="muted">Offline recipients remain queued. {smsInfo.octets}/{maxSmsOctets} encoded bytes.</p>{#if smsInfo.error}<p class="error" role="alert">{smsInfo.error}</p>{/if}</form></div></section>
<section class="panel"><h3>SMS history</h3><div class="inner"><label>Status <select bind:value={smsFilter}><option>All</option><option>Queued</option><option>Delivered</option><option>Failed</option></select></label><p class="muted">Delivered means the phone accepted the relay, not that a person read it.</p></div><div class="scroll"><table class="data"><thead><tr><th>Message</th><th>From → To</th><th>Status</th><th>Created</th><th>Text</th></tr></thead><tbody>{#each smsPage.rows as item (item.id)}<tr><td><button onclick={()=>selectedMessage=item}>{String(item.id)}</button></td><td>{item.sender} → {item.recipient}</td><td>{smsPhaseLabel(item.phase)}</td><td>{date(item.createdAtUnixMs)}</td><td class="wrap">{item.text??'(binary or receipt)'}</td></tr>{:else}<tr><td colspan="5">No messages.</td></tr>{/each}</tbody></table></div><Pager page={smsPage}/>{#if smsPage.error}<p class="error inner" role="status">SMS history unavailable: {smsPage.error}</p>{/if}</section>
{#if message}<section class="panel"><h3>SMS {String(message.id)} details</h3><div class="inner"><p>{message.sender} → {message.recipient} · {smsPhaseLabel(message.phase)}</p><p>{message.text}</p><details><summary>Delivery and protocol details</summary><p>Relay accepted: {message.relayAccepted?'Yes':'No'} · Delivery acknowledgment: {message.deliveryAcknowledged?'Yes':'No'} · Read acknowledgment: {message.manualAcknowledged?'Yes':'No'}</p><p>Message reference: {message.messageReference}</p><p class="mono wrap">PDU: {Array.from(message.pdu,b=>b.toString(16).padStart(2,'0')).join(' ')}</p><Metrics title="Delivery procedure" rows={message.measurements}/></details></div></section>{/if}
{/if}
{#if screen==='Signaling'}<AirMessages onphone={phoneLink}/>{/if}
{#if screen==='Radio'}<div class="cols"><Metrics title="Cell and carrier" rows={snapshot.carrier}/></div>{/if}
{#if screen==='Events'}
<section class="panel"><h3>Event log</h3><div class="inner toolbar"><label>Severity <select bind:value={eventLevel}><option>All</option><option>INFO</option><option>WARN</option><option>ERROR</option></select></label><label>Component <select bind:value={eventComponent}><option value="">All</option>{#each eventComponents as component}<option>{component}</option>{/each}</select></label><label>Find <input bind:value={eventSearch}></label><button onclick={()=>eventsPage.toggleHold()}>{eventsPage.held?'Resume':'Hold'}</button></div><div class="scroll"><table class="data"><thead><tr><th>Time</th><th>Severity</th><th>Event</th></tr></thead><tbody>{#each eventsPage.rows as event (event.sequence)}<tr><td>{date(event.unixMs)}</td><td>{eventLevelLabel(event.level)}</td><td class="event-message"><details><summary>{event.message}</summary><p>{event.component}</p><pre class="wrap">{event.fields}</pre></details></td></tr>{:else}<tr><td colspan="3">No matching events.</td></tr>{/each}</tbody></table></div><Pager page={eventsPage}/><div class="inner muted">The cell keeps the latest 256 events.</div></section>
{/if}
</main></div><footer class="footer"><img class="footer-icon" src="./favicon.svg" alt="">136BTS · channel {value(snapshot.carrier,'Channel')} · {stale?'DISCONNECTED':'CONNECTED'} · last update {timestamp(snapshot.unixMs)}</footer>
</div>
