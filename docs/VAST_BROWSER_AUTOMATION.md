# Vast.ai Browser Automation in Noland Connect

This app now supports a managed Vast.ai browser-session flow directly from the main UI.

## Product intent

Instead of forcing the user to manually paste a Vast.ai API key, Noland can now:

1. Launch a managed Chrome session for Vast.ai
2. Let the user log in manually once
3. Save a reusable Playwright `storageState`
4. Reuse that browser session to:
   - generate a Vast.ai API key
   - open the Vast.ai billing screen
   - open the Add Credit flow
   - open the Automatic Top-up flow

## Where the UI lives

### Onboarding
- `src/features/onboarding/OnboardingScreen.tsx`

Added controls:
- `Connect Vast.ai Account`
- `Generate API Key From Session`
- status display for automation availability and saved session state

### Settings
- `src/features/settings/SettingsScreen.tsx`

Added controls:
- `Connect Vast.ai`
- `Generate API Key`
- `Open Billing Session`
- `Open Add Credit`
- `Open Auto Top-up`
- `Refresh Session Status`

## Frontend bridge

### New backend wrappers
- `src/lib/backend.ts`

New commands exposed to React:
- `getVastBrowserAutomationStatus()`
- `startVastBrowserAuthSession()`
- `generateVastApiKeyFromBrowserSession()`
- `openVastBillingBrowserSession()`

### Store integration
- `src/store/appStore.ts`

New store methods:
- `refreshVastBrowserAutomationStatus`
- `connectVastBrowserSession`
- `generateVastApiKeyViaBrowserSession`
- `openVastBillingBrowserSession`

## Tauri backend bridge

### Rust commands
- `src-tauri/src/commands/mod.rs`
- `src-tauri/src/main.rs`

New commands:
- `get_vast_browser_automation_status`
- `start_vast_browser_auth_session`
- `generate_vast_api_key_from_browser_session`
- `open_vast_billing_browser_session`

These commands run local Node scripts and persist artifacts under the app data directory in:

- `vast-browser-automation/playwright/.auth/vast-ai.json`
- `vast-browser-automation/artifacts/...`

## Playwright scripts used by the app

- `scripts/vast-ai-utils.mjs`
- `scripts/vast-ai-bootstrap-session.mjs`
- `scripts/vast-ai-create-api-key.mjs`
- `scripts/vast-ai-open-billing-session.mjs`

## Development requirements

This beta path depends on Node + Playwright being present in the repo environment.

Added dev dependency:
- `playwright`

## Manual debug commands

```bash
npm run vast:auth
npm run vast:api-key
npm run vast:billing
```

## Important limitation

This implementation is designed for the desktop app development/runtime environment where the local repo scripts are available.

It is not yet packaged as a standalone production sidecar. For a fully bundled release, the next step is to move the Playwright runtime into a packaged sidecar or another app-managed automation service.
