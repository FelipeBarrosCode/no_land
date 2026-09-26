# Transport transition contract

The state machine is:

```text
idle -> validating_target -> applying_endpoint -> validating_tunnel -> committing -> completed
                                  |                    |                 |
                                  +--------------------+-----------------+-> rolling_back -> failed
```

A transition commits only after all of these checks succeed:

1. The helper reports the requested peer endpoint.
2. GotaTun reports its expected launch/config identity.
3. Fresh WireGuard traffic or a handshake is observed.
4. `10.77.0.1` is reachable.
5. Host control RPC is reachable.
6. The Sunshine control endpoint is reachable.

Only one transition may mutate GotaTun at a time. Manual requests supersede
queued automatic requests. Allocation replacement takes the allocation lock
before the transition lock; code must never acquire them in reverse order.

The desktop increments `clientRevision` only after its atomic state commit.
The host independently increments `hostRevision`. `transitionId` and
`allocationGeneration` are never reused.
