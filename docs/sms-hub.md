# SMS hub

Connects SMS to an SMPP v3.4 server or client. Off by default.

- SMS to non-local numbers go to the hub.
- SMS from the hub to a subscriber's number are delivered to that phone.
- Local numbers always stay local.

## Configure

```yaml
sms_hub:
  enabled: true
  role: client
  hub_addr: 192.0.2.10:2775
  system_id: REPLACE_ME
  password: REPLACE
```

| Key | Notes |
| --- | --- |
| `role` | `client` connects to `hub_addr`. `server` listens on `listen_addr` |
| `system_id` | Up to 15 characters |
| `password` | Up to 8 characters |

The link is plain TCP. Use a private network. Put credentials in a `*.local.yaml` file, which Git ignores.

## Status

The status bar shows the hub link. Each SMS shows one of: **Queued for hub**, **Sending to hub**, **Accepted by peer**, **Delivered**, **Expired**, **Failed**.

**Accepted by peer** means the hub took it, not that the phone got it.

## Limits

- Text only, up to 254 bytes.
- Multipart messages from the hub are joined into one.
- Messages that cannot be sent expire after 24 hours. Failures retry after 30 seconds.
- A dropped link can cause a duplicate on retry.
- The hub being down does not affect calls or local SMS.
