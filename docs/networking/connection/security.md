# Security and secret ownership

| Material | Owner | Storage | Lifetime |
|---|---|---|---|
| Cloudflare TURN Key ID/API token | Desktop | OS keyring entry `com.noland.connect.cloudflare-turn/default` | Until removed |
| TURN username/password | Desktop and host process memory | Never persisted | At most 48 hours |
| Per-instance control secret | Desktop and host | OS keyring / host root-only file | Instance lifetime |
| Probe token | Desktop and host process memory | Never persisted | Probe session |
| TURN allocation tuple | Host runtime | Non-secret host metadata only | Allocation lifetime |

`state.json` stores `secure-store://cloudflare-turn/default`, enablement, and
non-secret expiry/validation metadata. It must never contain any credential
value. API errors are reduced to status and generic provider detail before they
reach logs.

The public UDP probe listener silently drops malformed, expired, unknown, and
unauthenticated packets. Its acknowledgement is never larger than the request.
Privileged management RPC is not exposed on a public interface.
