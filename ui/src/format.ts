import { ConnectError } from '@connectrpc/connect';
import { BearerPhase, CallPhase, CellPhase, EventLevel, SmsPhase } from './gen/operations_pb';
import type { Call, CallLeg, WireMessage } from './gen/operations_pb';

export const errorText = (caught: unknown) =>
  caught instanceof ConnectError ? caught.rawMessage : caught instanceof Error ? caught.message : String(caught);
export function frequency(value: string | number): string {
 const hz = Number(value);
 if (!Number.isFinite(hz)) return `${value} Hz`;
 const [scale, unit, digits] = hz >= 1e9 ? [1e9, 'GHz', 6] : hz >= 1e6 ? [1e6, 'MHz', 6] : hz >= 1e3 ? [1e3, 'kHz', 3] : [1, 'Hz', 0];
 return `${(hz / scale).toLocaleString('en-US', { maximumFractionDigits: digits })} ${unit}`;
}
export const timestamp = (value?: bigint) => value ? new Date(Number(value)).toLocaleString() : 'Not recorded';
export function summary(message: WireMessage): string {
 const called = message.decoded.match(/Called=[^:]*:([^\s]+)/)?.[1];
 switch(message.name) {
  case 'Origination': return called ? `Requested a call to ${called}` : 'Requested a call';
  case 'Page': return 'Looking for phone';
  case 'Page response': case 'Page Response': return 'Phone responded to paging';
  case 'Registration': return 'Requested registration';
  case 'Registration response': return 'Registration result';
  case 'Registration accept': return 'Registration accepted';
  case 'Registration reject': return 'Registration rejected';
  case 'Alert with information': return 'Ring phone and send caller information';
  case 'Connect': return 'Phone answered';
  case 'Release': return 'End call';
  case 'R-DATA': return message.decoded.includes('SMS payload:') ? (message.direction==='Reverse' ? 'SMS submitted by phone' : 'SMS sent to phone') : 'Relay data';
  case 'R-ACCEPT': return 'SMS relay accepted';
  case 'R-REJECT': return 'SMS relay rejected';
  case 'Audit Order': case 'Audit order': return 'Check phone response';
  case 'Audit confirmation': return 'Phone responded to check';
  default: return message.name;
 }
}

export function identityLabel(identity: string): string {
 if (/^ESN\(\d+\)$/.test(identity)) return 'Mobile identity not learned';
 const match = /^Min34\((\d+)\)$/.exec(identity);
 if (!match) return identity;
 const value = BigInt(match[1]);
 const groupBits = 10n, digitBits = 4n, localBits = 24n, minBits = 34n;
 const groupMask = (1n << groupBits) - 1n;
 const group = (value: bigint) => value < 1000n ? String(value).padStart(3,'0').split('').map(d => (Number(d)+1)%10).join('') : undefined;
 const area = group(value >> localBits), exchange = group((value >> (groupBits+digitBits)) & groupMask), tail = group(value & groupMask);
 const digit = Number((value >> groupBits) & ((1n << digitBits)-1n));
 if (value >= (1n << minBits) || area === undefined || exchange === undefined || tail === undefined || digit < 1 || digit > 10) return 'MIN (invalid encoding)';
 return `MIN ${area}${exchange}${digit%10}${tail}`;
}

export const esnLabel=(value?:number)=>value===undefined?'Not reported':'0x'+value.toString(16).toUpperCase().padStart(8,'0');

export const cellPhaseLabel = (phase: CellPhase) => ({
  [CellPhase.UNSPECIFIED]: 'Connecting', [CellPhase.STARTING]: 'Starting', [CellPhase.RUNNING]: 'Running',
  [CellPhase.STOPPED]: 'Stopped', [CellPhase.STOP_UNCONFIRMED]: 'Stop unconfirmed', [CellPhase.FAILED]: 'Failed',
}[phase]);
const callPhaseLabels: Record<CallPhase, string> = {
  [CallPhase.UNSPECIFIED]: 'Unknown', [CallPhase.PAGING]: 'Paging phone', [CallPhase.ESTABLISHING]: 'Setting up',
  [CallPhase.ALERTING]: 'Ringing', [CallPhase.WAITING_ANSWER]: 'Waiting for answer', [CallPhase.CONNECTING]: 'Connecting',
  [CallPhase.CONNECTED]: 'In call', [CallPhase.RELEASING]: 'Ending call', [CallPhase.ENDED]: 'Ended',
};
export const callLabel = (call: Call) =>
  call.endReason ? `${callPhaseLabels[call.phase]}: ${call.endReason}` : callPhaseLabels[call.phase];
export const legLabel = (leg: CallLeg) => leg.source || ({
  [BearerPhase.UNSPECIFIED]: '-', [BearerPhase.PAGING]: 'Paging', [BearerPhase.RESERVED]: 'Reserved',
  [BearerPhase.ASSIGNING]: 'Assigning channel', [BearerPhase.ESTABLISHED]: 'On channel', [BearerPhase.STOPPING]: 'Leaving channel',
  [BearerPhase.STOPPED]: 'Off channel',
}[leg.phase]);
export const smsPhaseLabel = (phase: SmsPhase) => ({
  [SmsPhase.UNSPECIFIED]: 'Unknown', [SmsPhase.QUEUED]: 'Queued', [SmsPhase.DELIVERED]: 'Delivered', [SmsPhase.FAILED]: 'Failed',
}[phase]);
export const eventLevelLabel = (level: EventLevel) => ({
  [EventLevel.UNSPECIFIED]: '-', [EventLevel.TRACE]: 'TRACE', [EventLevel.DEBUG]: 'DEBUG', [EventLevel.INFO]: 'INFO',
  [EventLevel.WARN]: 'WARN', [EventLevel.ERROR]: 'ERROR',
}[level]);
