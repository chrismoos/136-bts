# Getting started

## 1. Install dependencies

Install the packages listed under [Setup](../README.md#setup). The commands below use UHD for a USRP B200 or B210. For another radio, use its configuration and replace `uhd` with `lime`, `bladerf`, `soapy` or `network`.

## 2. Update the config

Edit `config/uhd.yaml`:

| Key | Set to |
| --- | --- |
| `radio.device` | `type=b200` opens the first B200 or B210. Add `serial=` to pick one of several |
| `radio.tx_gain_db` | Start low |
| `cell.channel` | The channel the phone will look for |
| `cell.sid` | The SID the phone expects |
| `cell.operator.soc` | The operator code programmed in the phone, if it will not camp without one. See [Configuration](configuration.md#operator-code-soc) |

## 3. Start the cell

```sh
cargo run --release -p bts136 --features uhd -- --run config/uhd.yaml
```

Open the console at <http://127.0.0.1:8090>. The status bar shows **Cell: Running**.

## 4. Register a phone

1. Set the phone to the cell's band and channel. The cell has no analog control channel, so set the phone to digital or force the channel.
2. Power the phone on. It appears on **Phones** under **Observed phones without provisioning**.
3. Select **Add subscriber for this phone**, enter a number such as `1001`, and save.

## 5. Test

- **SMS:** on the **SMS** page, send from `1002` to `1001`.
- **Call:** open the phone, enter a Caller ID, select **Call phone**. Answering plays sample audio.

Outgoing calls routed to the bundled WAV service play the configured ringback for five seconds before answering and starting audio from the beginning. This delay does not apply to SIP calls or calls between phones.
