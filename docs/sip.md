# SIP calling

Connects phone calls to a PBX or SIP provider. Off by default.

- Calls to non-local numbers go to the SIP peer.
- SIP calls to a subscriber's number ring that phone.
- Audio is G.711 (PCMU or PCMA). Keypad tones are sent as RFC 4733 events.
- Up to two calls at a time. The carrier has three full-rate channels and the control channel uses one. A call between two local phones uses both of the others, so no other call can start until it ends.

## Build

```sh
cargo build --release -p bts136 --features uhd,sip-backend
```

Needs the `libre` development package.

## Provider with authentication, registration and STUN

`config/sip.example.yaml` contains every supported SIP setting, including an authenticated provider account, registration and STUN-assisted RTP. The example is disabled. Copy its `sip` block into the cell configuration, replace the placeholders below, then set `sip.enabled: true`. Restart the cell after configuration changes.

| Example value | Replace with |
| --- | --- |
| `192.0.2.10` in `listen_addr` and `rtp_listen_addr` | An address assigned to the host running 136BTS |
| `sip.example.com` | The provider's SIP domain or outbound server. The registrar may use a different server |
| `15555550100` | The assigned number or SIP account identity, as required by the provider |
| `provider-account` | The digest authentication username, which may differ from the assigned number |
| `replace-with-provider-password` | The provider password |
| `stun.example.com:3478` | A reachable STUN server and port |

Keep the configuration containing credentials outside version control, for example in the ignored `config/handset.yaml`. The example file is a fragment and cannot be passed to `--run` by itself. It must be combined with the cell and radio settings.

### SIP and audio

| Key | Default when omitted | Behavior |
| --- | --- | --- |
| `enabled` | `false` | Starts the SIP gateway when enabled. Requires the `sip-backend` build feature |
| `generate_ringback` | `true` | Plays the `calls.ringback` tone on outbound SIP ringing without early media. Set `false` to suppress this generated tone. Received early media still plays |
| `listen_addr` | `127.0.0.1:5060` | Local SIP address and port. Must be an assigned, concrete address with a nonzero port |
| `transport` | `udp` | `udp`, `tcp` or `tls`. Must match the provider |
| `request_uri_template` | `sip:{called}@127.0.0.1:5061` | Outgoing destination. `{called}` is replaced with the dialed number |
| `from_domain` | `136-bts.local` | Domain in the outgoing SIP From identity |
| `caller_id_override` | `null` | Fixed outgoing caller number. When omitted, uses the calling phone's number |
| `rtp_listen_addr` | `127.0.0.1` | Local RTP bind address. Must be an assigned address, not `0.0.0.0` |
| `rtp_advertise_addr` | `null` | Address advertised for RTP when STUN is disabled. When omitted, uses `rtp_listen_addr`. With STUN enabled, uses the discovered address and port instead |
| `rtp_port_range` | `[16000, 16031]` | Local RTP port allocation range. Allow the selected UDP ports through the host firewall |
| `jitter_buffer_ms` | `60` | Audio buffering delay. Must be below 180 ms |
| `max_call_seconds` | `300` | Maximum SIP session lifetime, including call setup. Must be positive |

### Authentication and registration

The example sets both blocks. Setting `auth: null` disables outgoing digest credentials. Setting `registration: null` disables REGISTER while still allowing calls to the configured peer.

| Key | Default when omitted | Behavior |
| --- | --- | --- |
| `auth.username` | Required when `auth` is set | Digest authentication username for calls and registration |
| `auth.password` | Required when `auth` is set | Digest authentication password |
| `registration.registrar_uri` | Required when `registration` is set | Registrar SIP URI |
| `registration.from_uri` | Required when `registration` is set | Registration identity, used for both From and To. This may differ from the digest username |
| `registration.contact_user` | Required when `registration` is set | User part of the registered Contact address |
| `registration.expires_secs` | `300` | Requested registration lifetime in seconds. Must be positive |

Registration without `auth` sends REGISTER without digest credentials. An authentication challenge then cannot be answered. Credentials authenticate 136BTS to the provider. Incoming SIP calls are not authenticated. Allow only the trusted peer to reach the SIP port.

### NAT and STUN

| Key | Default when omitted | Behavior |
| --- | --- | --- |
| `nat.mode` | `disabled` | `stun_latch` enables STUN discovery and RTP mapping packets |
| `nat.stun_server` | `null` | STUN host and port. Required with `stun_latch` |
| `nat.stun_timeout_ms` | `1000` | STUN discovery timeout in milliseconds. Must be positive with `stun_latch` |
| `nat.rtp_latch_packets` | `5` | Number of RTP mapping packets sent to the peer with `stun_latch`. Zero disables these packets |
| `nat.rtp_latch_interval_ms` | `20` | Interval between RTP mapping packets |

STUN discovers the public endpoint of each RTP socket during call setup. Discovery failure fails call setup. STUN does not configure the router or discover the SIP listener's public endpoint. SIP signaling must remain reachable through the deployment's firewall and NAT configuration.

For a public host or static RTP forwarding, set `nat.mode: disabled` and `nat.stun_server: null`. Set `rtp_advertise_addr` to the externally reachable address when it differs from `rtp_listen_addr`.

### Local PBX without registration

Use the PBX address in `request_uri_template` and its domain in `from_domain`. Set `registration: null` when the PBX does not require REGISTER, `auth: null` when it does not require digest authentication, and `nat.mode: disabled` when RTP is directly reachable. Leave `rtp_advertise_addr: null` when the local RTP address is also the address the PBX should use.

### Logging

| Key | Default when omitted | Behavior |
| --- | --- | --- |
| `logging.sip_trace` | `false` | Logs SIP signaling when enabled |
| `logging.sip_sdp` | `false` | Logs negotiated audio descriptions when enabled |

## Check

1. The status bar shows **SIP: Up**.
2. Call out and in. Confirm audio both ways and keypad tones.
3. Turn on `sip.logging.sip_trace` to debug.
