<script lang="ts">
import type { Metric } from './gen/operations_pb';
import { frequency } from './format';
const help:Record<string,string>={
 'TX underflows (nonfatal)':'The radio ran out of transmit samples. The cell continues, but the downlink may have been interrupted.',
 'Missed TX slots':'Scheduled slots skipped because samples could not be submitted in time. Total for this run.',
 'Missed TX slots, last minute':'Scheduled slots skipped in the last minute. Any nonzero value can break handset exchanges.',
 'TX underflows, last minute':'Radio TX underflows in the last minute. Repeated underflows can break handset exchanges.',
 'Late packets (nonfatal)':'Transmit packets that arrived after their scheduled radio time.',
 'TX queue depth':'Transmit blocks currently pending on the host.',
 'RSSI':'Received signal power when reported by the radio or handset.',
 'BER':'Bit error rate when reported by the handset.',
 'Uplink BER':'Coded bits the decoder corrected in recent CRC-valid uplink frames, before error correction. Speech counts only its protected class-1 bits.',
};
const label=(name:string)=>({'TX underflows (nonfatal)':'TX underruns','Late packets (nonfatal)':'Late TX packets','Forward TX':'Downlink frequency','Reverse RX':'Uplink frequency','Last access physical slot':'Last access slot'}[name]??name);
let { title, rows }: { title: string; rows: Metric[] } = $props();
</script>
<section class="panel"><h3>{title}</h3><div class="inner"><table class="kvgrid"><tbody>
{#each rows as row}<tr><th>{label(row.name)}{#if help[row.name]} <span title={help[row.name]} aria-label={help[row.name]}>ⓘ</span>{/if}</th><td class:metric-unavailable={row.value === undefined} title={row.unavailableReason}>{row.value === undefined ? 'Unavailable' : row.unit === 'Hz' ? frequency(row.value) : `${row.value} ${row.unit}`}</td></tr>{/each}
</tbody></table></div></section>
