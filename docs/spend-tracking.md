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
- Confirmed local pause/destroy actions close the billing interval immediately,
  so exiting before the next poll does not bill a known stopped VM as running.
- Ledger entries are kept per instance per month for 12 months.

## Budget

`state.spend.settings`:

| Field | Default | Meaning |
| --- | --- | --- |
| `monthlyBudgetUsd` | `0` | Monthly cap in USD; `0` disables the budget. |
| `warnAtPercent` | `80` | Warning threshold, clamped to 1–100. |
| `autoStopAtBudget` | `false` | Stop running instances when the cap is reached. |

Threshold alerts (`warning`, `exceeded`) are queued once per month. Raising the
budget or warning threshold re-arms them. Each alert has a UUID and remains in a
bounded persisted queue until the main window handles and acknowledges it.
The frontend subscribes before replaying queued alerts and deduplicates IDs.
Delivery failures leave an alert available for retry; disabled notifications or
denied OS permission count as handled. A crash between delivery and acknowledgment
can replay an alert once (at-least-once delivery, not exactly-once).

### Auto-stop safety

- Budget stops go through `InstanceLifecycleService::pause_instance_for_budget`,
  which shares the normal pause implementation, so the
  shared-storage backup runs first and the per-instance lifecycle lock is
  respected.
- The pre-stop seal must reach `SEALED`/`COMPLETED`; a queued response is not a
  successful backup. Failure, cancellation, rollback, or timeout prevents stopping.
- Stopped instances still bill for storage; destroying them is left to the
  user.
- Only successful stops are recorded as a monthly override. Failed attempts retry
  after a five-minute cooldown; in-flight reservations prevent duplicate jobs.
  At most two budget stops run concurrently. Reservations are memory-only, so an
  app exit cannot persist an unperformed stop as completed.
- A failed stop produces an `auto_stop_failed` alert, which is shown even when
  budget notifications are turned off.

## Security boundaries

- Budget IPC commands require the main window and reject invalid settings rather
  than silently disabling a budget. Events containing spending data target main.
- Incoming webview events are only wake-up hints: notification contents are
  fetched from the main-only native queue, so a forged event cannot supply its
  own notification message. Concurrent notification permission requests are
  serialized and alert IDs are deduplicated before enqueueing.
- Listing responses are accepted only if their credential still matches the app.
  Stop jobs retain the original credential in memory (never serialized/logged)
  and recheck the credential, exact budget policy, month, and target before backup
  and again before the stop request. Changing policy while backup runs cancels
  the queued stop; requests already sent to the provider cannot be recalled.
- Changing the Vast API key starts a new local authorization scope and disables
  the budget until explicitly enabled again. Previous ledger estimates are kept
  in `priorCredentialLedger` (12-calendar-month retention), but are not charged
  against the new scope's budget. This conservative behavior also applies to key
  rotations for the same account, because keys alone do not identify an account.
- Failure notifications contain generic messages, not raw provider/SSH errors or
  backup credentials. Existing application credentials remain in the OS vault.
- This is a local estimation/control feature, not an authoritative billing cap or
  a complete app security boundary. No Land must be running to enforce the budget.
