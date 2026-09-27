# Direct WireGuard and Cloudflare TURN

This directory documents the versioned contracts implemented by
`network-contracts/`. The Rust crate is the executable source of truth for the
desktop client and `noland-network-agent`.

## Invariants

- WireGuard remains the only logical tunnel and keeps `10.77.0.1` as the host
  target.
- `direct` and `cloudflare_turn` are outer transports for the same tunnel.
- GotaTun owns the client WireGuard interface and peer endpoint.
- The host owns its TURN allocation and bridges only encrypted WireGuard UDP.
- A long-lived Cloudflare token is stored in the OS keyring, never `state.json`
  and never on the host.
- Direct connectivity is sufficient for successful provisioning. TURN failures
  are warnings unless no transport can establish the logical tunnel.
- A transport is active only after tunnel and Sunshine validation succeeds.

## Implemented contract surfaces

| Surface | Source |
|---|---|
| Client/host state | `network-contracts/src/state.rs` |
| Control envelopes | `network-contracts/src/control.rs` |
| Stable errors | `network-contracts/src/errors.rs` |
| Events | `network-contracts/src/events.rs` |
| Probe v1/v2 | `network-contracts/src/probe.rs` |
| `gaming-v1` evaluator | `network-contracts/src/evaluation.rs` |
| Generated JSON Schema | `schemas/*.schema.json` |

Regenerate the schemas from the repository root:

```sh
cargo run --manifest-path network-contracts/Cargo.toml \
  --example export_schemas -- docs/networking/connection/schemas
```

## Delivery gates

1. Shared contracts, schema v3, atomic state, and secure Cloudflare settings.
2. Host TURN allocation/bridge interoperability and throughput proof.
3. Privileged GotaTun endpoint mutation and transactional manual switching.
4. Independent direct/relay probes and provisioning evaluation.
5. Runtime automatic selection, recovery, diagnostics, and cross-platform UI.

Relay activation must remain unavailable until gates 2 and 3 pass. Persisting a
relay preference does not declare that the relay is active.

## Rollout switches

- Manual TURN selection is available when TURN is enabled and validated
  credentials are present in secure storage. The switch remains transactional
  and commits only after bridge interoperability checks pass.
- `NOLAND_ENABLE_AUTOMATIC_TRANSPORT_SELECTION=1` additionally starts the
  selected-instance `gaming-v1` evaluator every 30 seconds. It remains
  separately gated from manual switching.

Each evaluation installs short-lived in-memory probe sessions, samples direct
and relay paths concurrently, persists non-secret metrics and the decision, and
uses runtime hysteresis before requesting a transactional endpoint change.
