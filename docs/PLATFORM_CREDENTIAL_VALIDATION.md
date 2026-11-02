# Platform credential validation

The desktop (`noland/no_land`), Android (`no_land_android`), and iOS-port
(`no_land_mobile_port`) clients enforce the same policy. The website and feedback
API do not accept these platform credentials.

## Command-path review

- Platform credentials become Sunshine web credentials, via `sunshine --creds`
  over SSH and nested `sudo -u … bash -lc …` commands in `sunshine.rs` and
  `post_wireguard_setup.rs`.
- The same values authenticate HTTP Basic requests; a username must not contain
  the Basic-auth colon separator.
- The platform password also unlocks SSH keys. It must remain byte-for-byte
  unchanged; trimming or Unicode normalization would change the secret.
- Bootstrap command templates use Rust raw strings to preserve shell escapes
  through both shell layers. Input validation is not a substitute for quoting.

## Conservative supported-input policy

- Username: at least 3 ASCII characters; starts with a letter or number;
  remaining characters are letters, numbers, `.`, `_`, or `-`.
- Password: at least 6 ASCII characters; letters, numbers, and
  `._~!@#%^*+=,:?/-` are allowed. A leading `-` is rejected to avoid command-option
  ambiguity.
- Both reject whitespace, control characters (including newlines/NUL), Unicode,
  quotes, backslashes, and other unsupported shell-sensitive characters.
- Reject unsupported values rather than silently trimming, lowercasing,
  normalizing, or stripping characters. This is a deliberately conservative
  compatibility policy, not a claim that every rejected character is unsafe in
  every individual downstream API.

The UI validates during input in onboarding and settings. Both backend write
commands enforce the same policy before side effects. Provisioning and auth
repair revalidate stored credentials; existing unsupported credentials must be
replaced in Settings before retrying. They are not rewritten automatically.

Keep `src/lib/platformCredentials.ts` and
`src-tauri/src/services/platform_credentials.rs` aligned across the three repos.

## Tests

- `node --test scripts/platform-credentials.test.mjs scripts/sunshine-auth.test.mjs`
- `npx tsc --noEmit`
- `cargo test --locked --manifest-path src-tauri/Cargo.toml platform_credentials`

Tests cover ASCII allowlists, invisible/Unicode characters, command injection
payloads, unchanged secrets, and execution of both actual bootstrap templates
through nested shells with stubbed sudo/Sunshine (no remote host required).
