<script lang="ts">
import {onMount} from 'svelte';
import type {ReverseQuality} from './gen/operations_pb';
import {timestamp} from './format';
let {quality,compact=false,live=false}:{quality?:ReverseQuality;compact?:boolean;live?:boolean}=$props();
let now=$state(Date.now());
const BER_HELP='Coded bits the decoder corrected in recent CRC-valid uplink blocks, before error correction. Lower is better.';
onMount(()=>{if(!live)return;const timer=setInterval(()=>now=Date.now(),1000);return()=>clearInterval(timer);});
</script>
{#if quality}
<div class="reverse-quality" title="EVM measures error over the known sync symbols after frequency, phase and gain correction. Lower is better. This is not a full-burst transmitter conformance measurement.">
<span>Sync EVM <b>{quality.syncEvmPercent.toFixed(1)}%</b></span>
<meter aria-label="Reverse sync EVM" min="0" max="100" optimum="0" value={Math.min(100,quality.syncEvmPercent)}>{quality.syncEvmPercent.toFixed(1)}%</meter>
{#if quality.bitErrorPercent!==undefined}<span title={BER_HELP}>Uplink BER <b>{quality.bitErrorPercent.toFixed(2)}%</b></span>{/if}
{#if !compact}<span class="muted">Lower EVM is better.</span><p>Sync level <b>{quality.syncLevelDbfs.toFixed(1)} dBFS</b> · Frequency offset <b>{quality.frequencyOffsetHz.toFixed(1)} Hz</b> · Sync correlation {quality.syncCorrelation.toFixed(4)}</p><p class="muted">Last measured {live?Math.max(0,Math.floor((now-Number(quality.measuredUnixMs))/1000))+' seconds ago':timestamp(quality.measuredUnixMs)}. Level is relative to digital full scale, not calibrated RF power.</p>{#if quality.bitErrorPercent!==undefined}<p class="muted">Uplink BER is over the last {quality.checkedBits} coded bits of CRC-valid blocks. The base station sensitivity requirement is 3% or less.</p>{/if}{/if}
</div>
{:else}<span class="muted">{compact?'-':'Waiting for a decoded reverse burst.'}</span>{/if}
