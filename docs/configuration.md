# Configuration

One YAML file, passed with `--run` or `--check-startup`. Only `radio` and `cell` are required. Unknown keys stop startup with an error naming the key. Restart the cell after changes. Templates are in `config/`.

## cell

| Key | Default | Notes |
| --- | --- | --- |
| `band` | required | `cellular800` or `pcs1900` |
| `channel` | required | Cellular 1–799 or 991–1023. PCS 2–1998 |
| `sid` | required | 0–32767 |
| `dvcc` | required | 1–255 |
| `network_name` | none | Up to 15 ASCII characters. Not every phone displays it |
| `operator` | none | Operator code. See [Operator code](#operator-code-soc). Some phones need it to camp |
| `access_power_level` | 7 | Phone power for access, 0–10 |
| `traffic_power_level` | 7 | Phone power on calls, 0–10 |

Frequencies follow from band and channel. Cellular channel 991 is 869.040 MHz down, 824.040 MHz up.

### Operator code (SOC)

Some phones only camp on a cell whose operator code matches the one stored in the phone. If a phone finds the cell but will not stay on it, set the SOC the phone is programmed with:

```yaml
cell:
  operator:
    soc: 21
    bsmc: 0
```

| Key | Default | Notes |
| --- | --- | --- |
| `soc` | none | System operator code, 12 bits. 0 and 2048 mean unknown |
| `bsmc` | 0 | Base station manufacturer code. 0 means unknown |

Without `operator`, the cell identifies itself by SID only.

### Neighbors

Optional. Most single-cell setups leave these out.

```yaml
cell:
  neighbors:
    - channel: 2
      dvcc: 1
```

Up to 24 neighbors in the same band, for idle reselection. Neighbors do not provide handoff. Set each neighbor's values to match the real cell. Other neighbor keys: `protocol_version_code`, `reselect_offset_db`, `sufficient_signal_code`, `minimum_signal_code`, `delay_code`, `frequent_measurements`, `synchronized`, `cell_type_code`, `access_power_level`.

## radio

### USRP

Build with `--features uhd`.

```yaml
radio:
  kind: uhd
  device: "type=b200"
  channel: 0
  antenna: TX/RX
  rx_antenna: RX2
  tx_gain_db: 20.0
  rx_gain_db: 20.0
  tx_bandwidth_hz: 200000
  rx_bandwidth_hz: 200000
  master_clock_rate: 24883200
  clock_source: internal
  time_source: internal
```

`type=b200` opens the first B200 or B210 found. Add `serial=` to pick one of several, for example `type=b200,serial=ABC123`. Add `fpga=/path/to/image.bin` to load a specific FPGA image. Keep the bandwidth and master clock values as shown. TX gain is not calibrated power.

### Remote radio daemon

Build with `--features network`.

```yaml
radio:
  kind: network
  addr: 127.0.0.1:50710
  transport: tcp
```

The cell sends its [sample rate](#sample-rate) to the daemon when it connects.

### LimeSDR

Build with `--features lime`.

```yaml
radio:
  kind: lime
  device: ""
  channel: 0
  tx_antenna: BAND1
  rx_antenna: LNAW
  tx_gain_db: 40
  rx_gain_db: 30
```

An empty `device` opens the first LimeSDR found.

### bladeRF

Build with `--features bladerf`.

```yaml
radio:
  kind: blade_rf
  device: ""
  channel: 0
  tx_gain_db: 30
  rx_gain_db: 30
  tx_sample_rate_hz: 777600
```

An empty `device` opens the first bladeRF found. A bladeRF 2.0 cannot run below 520834 samples/s, so it needs a higher [sample rate](#sample-rate). A bladeRF 1 works with the default.

### SoapySDR

Build with `--features soapy`.

```yaml
radio:
  kind: soapy
  device: "driver=lime"
  channel: 0
  antenna: BAND1
  tx_gain_db: 40
  rx_gain_db: 30
```

`device` takes SoapySDR device arguments. The radio must support timestamped transmit and receive.

### No radio

`kind: noop` runs the cell with no hardware, for testing.

### Sample rate

`tx_sample_rate_hz` in `radio` sets the TX and RX sample rate. When it is omitted, the radio runs at 388800 samples/s. When it is set, it must be a whole multiple of the 24300 symbols/s rate, from 194400 to 777600. Any other value stops startup with an error naming `radio.tx_sample_rate_hz`. Use 777600 for a radio that cannot run as low as 388800.

The radio must report exactly the requested rate, or startup stops with a sample rate error. `rx_sample_rate_hz` is not accepted.

## registration

| Key | Default | Notes |
| --- | --- | --- |
| `allow_unknown` | `true` | Let phones without a subscriber entry register |
| `home` | `true` | Allow home registration |
| `roaming` | `true` | Allow roaming registration |
| `period_superframes` | 940 | Periodic registration, about 10 minutes. 94 is about 1 minute. Multiples of 94 only. `null` turns it off |
| `area` | `null` | Location area number, 0–1023 |

## calls

| Key | Default | Notes |
| --- | --- | --- |
| `ringback` | `us` | Caller ringback style. `us`: 440+480 Hz, 2 seconds on and 4 off. `eu`: 425 Hz, 1 second on and 4 off |
| `answer_timeout_seconds` | 60 | Ring time before giving up, 1–65 |
| `radio_loss_timeout_seconds` | 5 | End a call after this long without hearing the phone |

Local calls play ringback to the caller while the called phone is ringing. It stops when the call is answered, released or times out. The `eu` preset uses a continental European pattern. Outbound SIP uses the same style when the remote peer reports ringing without early media.

```yaml
calls:
  ringback: eu
sip:
  generate_ringback: false
```

`sip.generate_ringback: false` suppresses the generated tone on outbound SIP calls. Local calls still play ringback, and received SIP early media still plays. Omitted settings select `us` and enable generated SIP ringback. Restart the cell after changing either setting.

## sms

| Key | Default | Notes |
| --- | --- | --- |
| `display_time` | `invoke` | `invoke` shows SMS immediately. `default` lets the phone decide, and some phones then store it silently |

## web

| Key | Default | Notes |
| --- | --- | --- |
| `enabled` | `false` | Turn on the console and gRPC |
| `listen` | `127.0.0.1:8080` | No login, so keep it on a trusted network |
| `allowed_origins` | none | Add the browser origin when behind a reverse proxy |

## core, logging, time

| Key | Default | Notes |
| --- | --- | --- |
| `core.state_path` | `136-bts.sqlite3` | Database file |
| `logging.filter` | `info` | `RUST_LOG` overrides it |
| `logging.format` | `text` | Or `json` |
| `time.source` | `local_system` | The cell broadcasts the host clock and timezone |

All shipped radio examples use `info`, which omits debug and trace events. `RUST_LOG` replaces `logging.filter` for the process in both text and JSON modes. For runtime diagnostics:

```sh
RUST_LOG=info,bts136::runtime=debug target/release/136-bts --run config/handset.yaml
```

Use `RUST_LOG=warn` for warnings and errors only. Unset `RUST_LOG` to use the YAML filter again. Invalid filter syntax fails startup. SIP packet and SDP logging also require the switches described in [SIP calling](sip.md#logging).

## Channel measurements (MACA)

Optional. Asks phones to report signal strength on other channels.

```yaml
maca:
  mode: short_term
  current_channels: [992, 993]
```

`mode` is `short_term`, `long_term` or `both`. Up to 15 channels in total. `other_hyperbands` adds channels in other bands. Results appear on each phone's page.

## SIP and SMS hub

See [SIP calling](sip.md) and [SMS hub](sms-hub.md).

## Real-time scheduling

```yaml
realtime:
  enabled: true
  tx_priority: 80
  rx_priority: 75
```

Real-time scheduling is enabled by default. The serving/TX thread and buffered RX reader request real-time scheduling. Background service threads retain their existing scheduling. Set `enabled: false` to disable these requests.

On Linux, both threads request `SCHED_FIFO`. `tx_priority` defaults to 80 and `rx_priority` defaults to 75. Both values must be in 1–99. The process requires `CAP_SYS_NICE` or a sufficient `RLIMIT_RTPRIO`. Effective policy and priority are checked after each request.

On macOS, both threads request a Mach time constraint with a 2 ms period and a 1 ms computation budget. The Linux priority values do not apply on macOS.

A failed scheduling request logs a warning and operation continues with the existing scheduling. Unsupported operating systems also log a warning. The calling thread's previous scheduling is restored when serving ends.
