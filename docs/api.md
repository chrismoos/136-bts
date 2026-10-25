# gRPC interface

The console runs on a gRPC API that scripts can use too. It is served on `web.listen`, as native gRPC and gRPC-Web. Service `bts136.operations.v1.Operations`, defined in `proto/operations.proto`. No authentication.

The server does not offer reflection, so pass the proto file. From the repository root, with the cell on `127.0.0.1:8090`:

```sh
# List subscribers
grpcurl -plaintext -import-path proto -proto operations.proto \
  -d '{"limit":10}' 127.0.0.1:8090 bts136.operations.v1.Operations/ListSubscribers

# Show the latest decoded signaling
grpcurl -plaintext -import-path proto -proto operations.proto \
  -d '{"limit":20}' 127.0.0.1:8090 bts136.operations.v1.Operations/ListSignaling
```

## Read

| RPC | Returns |
| --- | --- |
| `GetSnapshot` | Current cell status and active calls |
| `Watch` | Status updates, about once a second |
| `ListSubscribers` | Subscribers and their state |
| `ListObservedPhones` | Phones without a subscriber entry |
| `ListCalls` | Recent calls |
| `ListMessages` | SMS history |
| `ListSignaling` | Decoded signaling |
| `ListEvents` | Log events |

List calls take `offset` and `limit` (default 50, max 200) and return `total`.

## Control

| RPC | Does |
| --- | --- |
| `SaveSubscriber`, `DeleteSubscriber` | Manage subscribers |
| `SendSms` | Send an SMS |
| `StartCall`, `ReleaseCall` | Ring a phone with sample audio, end a call |
| `AuditSubscriber` | Ask an idle phone to respond or re-register |
| `RequestCapabilities`, `RequestStatus` | Ask a phone what it supports |
| `SetMeasurements` | Start or stop measurement reports on a call |
| `SetPhysicalControl` | Set a call's power level or timing advance |
| `SendCallInformation` | Send caller name, number or message-waiting counts during a call |
| `SendUserAlert`, `SendMessageWaiting` | Alert an idle phone |

A command that takes longer than 5 seconds returns an error, but may still have happened. Check before retrying.

## Examples

Send an SMS. Reuse the same `requestId` when retrying so it is sent once:

```json
{"requestId":"a1b2c3","senderNumber":"1002","recipientNumber":"1001","text":"hello"}
```

Ring a phone:

```json
{"subscriberId":{"kind":2,"value":"3410151054"},"callingNumber":"2001"}
```

Identity `kind`: 0 TMSI20, 1 TMSI24, 2 MIN34, 3 IMSI50.
