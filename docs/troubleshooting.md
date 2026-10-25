# Troubleshooting

| Problem | Check |
| --- | --- |
| Phone does not find the cell | Phone set to digital, or to the right channel and SID. The cell has no analog control channel. Try more TX gain |
| Phone finds the cell but will not camp | Set `cell.operator.soc` to the operator code programmed in the phone. See [Configuration](configuration.md#operator-code-soc) |
| Phone finds the cell but does not register | Look for its registration on **Signaling**. If nothing arrives, check RX gain and the RX antenna. With `allow_unknown: false`, unknown phones are rejected |
| Calls or SMS to a phone fail | Subscriber shows **Waiting for first registration**, the ESN is wrong, or the phone has not re-registered since a restart |
| SMS stays **Queued** | The phone is off or out of range. It is delivered when the phone registers |
| Ringing phone will not decline | Phones cannot decline a ringing call. Wait for the answer timeout or use **End call** |
| Missed TX slot or underflow warnings | The host or USB link is too slow. Lower logging to `info` and reduce host load |
| Console says **CONNECTION LOST** | The cell stopped, or the browser cannot reach `web.listen`. Behind a proxy, set `web.allowed_origins` |
| Startup fails | The error names the bad key. Common causes: channel outside the band, `period_superframes` not a multiple of 94, sample rate keys under `radio` |
| Phone ignores the network name or clock | Depends on the phone's own settings |
