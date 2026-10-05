# Spend Tracking and Monthly Budget

No Land keeps a local estimate of what rented instances cost so users can see
their burn rate and cap monthly spending without leaving the app. The
provider's invoice remains authoritative.

## Runtime architecture

- `src-tauri/src/models/spend.rs` — pure ledger model (`SpendState`). All
  accrual, month splitting, budget levels and auto-stop selection live here
  and are unit tested.
- `src-tauri/src/services/spend_tracker.rs` — background loop started from
  `main.rs`. Every 60 seconds it lists the account's instances, folds them into
  `state.spend`, persists, and emits `spend:updated`.
- `src-tauri/src/commands/spend.rs` — `get_spend_summary` and
  `update_budget_settings`.
- `src/features/spend/` — dashboard `SpendPanel`, Settings → Budget form and
  the `useSpendSummary` hook. `src/lib/spendNotifications.ts` turns
  `spend:alert` events into OS notifications (main window only).

## Accrual rules

- Each instance has a cursor holding the time and billing state of its last
  observation. The interval since then is billed at the rate that applied at
  the previous observation: the full hourly price while running (Vast
  `running`/`loading`/`creating`), the storage price while stopped.
- Gaps while the app was closed are billed the same way, because providers
  keep charging while No Land is not running.
- Intervals that span a month boundary (UTC) are split so each month gets its
  own share.
- A failed provider listing skips the tick. An instance missing from a
  successful listing is treated as destroyed: its cursor is removed and its
  history stays in the ledger.
- Ledger entries are kept per instance per month for 12 months.

## Budget

`state.spend.settings`:

| Field | Default | Meaning |
| --- | --- | --- |
| `monthlyBudgetUsd` | `0` | Monthly cap in USD; `0` disables the budget. |
| `warnAtPercent` | `80` | Warning threshold, clamped to 1–100. |
| `autoStopAtBudget` | `false` | Stop running instances when the cap is reached. |

Alerts (`spend:alert`, kinds `warning`, `exceeded`, `auto_stopped`,
`auto_stop_failed`) fire at most once per month per kind. Raising the budget
or the warning threshold re-arms them.

### Auto-stop safety

- Stops go through `InstanceLifecycleService::pause_instance`, so the normal
  shared-storage backup runs first and the per-instance lifecycle lock is
  respected.
- Stopped instances still bill for storage; destroying them is left to the
  user.
- An instance is stopped at most once per month. If the user starts it again
  it is treated as a deliberate override and left running.
- A failed stop produces an `auto_stop_failed` alert, which is shown even when
  budget notifications are turned off.
