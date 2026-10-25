<script lang="ts">
import {onMount, untrack} from 'svelte';
import {SignalingScope, type WireMessage} from './gen/operations_pb';
import {client} from './client';
import {Paged, PAGE_SIZE} from './paging.svelte';
import Pager from './Pager.svelte';
import ReverseQuality from './ReverseQuality.svelte';
import {summary, identityLabel} from './format';
let {identity='', compact=false, onphone=()=>{}}:{identity?:string;compact?:boolean;onphone?:(identity:string)=>void}=$props();
let direction=$state(''),search=$state(''),scope=$state(SignalingScope.PHONE_ACTIVITY),number=$state(''),dropped=$state(0n);
const COMPACT_ROWS=20;
const signaling=new Paged<WireMessage>(async (offset,limit,beforeSequence)=>{
 const page=await client.listSignaling({scope,identity,number,direction,search,offset,limit,beforeSequence});
 dropped=page.droppedMessages;
 return {rows:page.messages,total:page.total};
},untrack(()=>compact)?COMPACT_ROWS:PAGE_SIZE,row=>row.sequence);
const queryKey=$derived(JSON.stringify([identity,number,direction,scope,search]));
$effect(()=>{queryKey;untrack(()=>signaling.reset());});
onMount(()=>{const timer=setInterval(()=>void signaling.refresh(),1000);return()=>clearInterval(timer);});
</script>
<section class="panel"><h3>{compact?'Recent phone activity':identity?'Phone signaling':'Forward and reverse signaling'}</h3>
<div class="inner">
{#if !compact}<div class="toolbar"><label>View <select aria-label="Signaling view" bind:value={scope}><option value={SignalingScope.PHONE_ACTIVITY}>Phone activity</option><option value={SignalingScope.ALL_PHONE_SIGNALING}>All phone signaling</option><option value={SignalingScope.SYSTEM_BROADCASTS}>System broadcasts</option></select></label>
{#if !identity}<label>Phone <input aria-label="Signaling phone" type="tel" bind:value={number} placeholder="Number" size="10"></label>{/if}
<label>Direction <select bind:value={direction}><option value="">Both directions</option><option value="Forward">To phone</option><option value="Reverse">From phone</option></select></label>
<label>Find <input type="search" bind:value={search} placeholder="Number, message or field"></label><button onclick={()=>signaling.toggleHold()}>{signaling.held?'Resume':'Pause'}</button></div>
<p class="muted">{scope===SignalingScope.PHONE_ACTIVITY?'Registration, paging, calls and SMS. Routine radio maintenance and system broadcasts are hidden.':scope===SignalingScope.SYSTEM_BROADCASTS?'Cell overhead broadcasts. Repeated identical broadcasts are grouped.':'Includes acknowledgments, measurements and radio-control exchanges.'} Current run only.</p>{/if}
{#if signaling.error}<p class="error" role="alert">Signaling could not be loaded: {signaling.error}</p>{/if}{#if dropped>0n}<p class="error">{String(dropped)} signaling records could not be captured.</p>{/if}
</div><div class="scroll"><table class="data activity"><colgroup><col class="time"><col class="direction"><col class="phone"><col class="signal"><col></colgroup><thead><tr><th>Time</th><th>Direction</th><th>Phone</th><th>Reverse signal</th><th>Activity</th></tr></thead><tbody>
{#each signaling.rows as row (row.sequence)}<tr data-live-row><td>{new Date(Number(row.unixMs)).toLocaleTimeString()}</td><td>{row.direction==='Forward'?'To phone':'From phone'}<small>{row.channel}</small></td><td>{#if row.identity}<button class="table-action" onclick={()=>onphone(row.identity)}>{row.number||identityLabel(row.identity)}</button>{:else}{row.broadcast?'System':'Unidentified'}{/if}</td><td><ReverseQuality quality={row.reverseQuality} compact/></td><td><details><summary>{summary(row)}{row.repetitions>1n?' × '+row.repetitions:''}</summary><p><b>{row.name}</b> · {row.status}</p><p class="mono">{identityLabel(row.identity)}</p><pre class="wrap">{row.decoded}</pre><details><summary>Raw protocol data</summary><p class="muted">Forward entries follow host TX completion, which does not confirm reception. Traffic bits are re-encoded from decoded messages.</p><p class="mono wrap">{row.bitLength} bits: {Array.from(row.raw,byte=>byte.toString(16).padStart(2,'0')).join(' ')}</p></details></details></td></tr>
{:else}<tr><td colspan="5">No matching phone activity.</td></tr>{/each}
</tbody></table></div>{#if !compact}<Pager page={signaling}/>{/if}</section>
