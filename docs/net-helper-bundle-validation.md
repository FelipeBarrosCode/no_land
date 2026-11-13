# Packaged network-helper validation

The desktop release checks require `noland-net-helper` in the **extracted or
installed distributable**, not merely in `src-tauri/binaries`, the build tree,
the developer's `PATH`, or an environment override.

## Windows and Linux

Both x86-64 and ARM64 CI lanes run the offline header/layout regression tests
before building. The signed-build workflow then runs
`scripts/verify-bundled-sidecars.mjs --target <triple> --require-distributables`
before publishing artifacts:

- Windows: install NSIS packages, or extract MSI packages when produced. Require
  an actual PE64 helper of the target architecture beside the installed app.
- Linux: extract AppImage, Debian and RPM payloads. Require a target-architecture
  ELF64 helper in a production lookup location with executable permissions.
  Portable AppImages must not rely on an unrelated host-installed helper.
- Arch: the package builder verifies the final pacman file list includes the
  helper as well as the application. Its payload comes from the already-verified
  Debian package without stripping the binaries.
- Run the **packaged helper** with `check --config <temporary fixture>` for every
  inspected Windows/AppImage/Debian/RPM payload. This validates configuration
  without opening a tunnel, installing a driver, changing privileges or modifying
  the user's app profile. Fixture paths contain spaces.

The no-build tests include missing helpers, staging-only copies, misplaced copies,
wrong CPU architectures, foreign executable formats, empty/truncated binaries,
placeholder scripts, unsafe symlinks and missing Unix executable permissions.
Effective Tauri config overlays must preserve the helper in `bundle.externalBin`.

## Signing and notarization

- **macOS:** the helper is signed with Developer ID Application, a trusted
  timestamp and hardened runtime, with the same TeamIdentifier as its containing
  app. The bundle verifier checks the helper signature explicitly, verifies all
  nested app code, validates the app's stapled ticket and requires Gatekeeper's
  `Notarized Developer ID` assessment. These checks apply to the built app and
  the app mounted from the final DMG. The updater archive is rebuilt from the
  final notarized app, after dependency repair and signing.
- **Windows:** the helper is explicitly in the pre-bundle Authenticode signing
  inputs. The installed helper, not just the installer, must pass signature and
  trusted timestamp verification. The input-receipt tests detect subsequent helper
  modification.
- **Linux:** Apple notarization and Authenticode do not apply. The helper is
  covered by the signed release package, OpenPGP checksum manifest and verified
  provenance of the exact final artifact bytes.

A command-line helper inside a macOS app is covered as nested code in that
notarized app; it does not receive a separately stapled standalone executable
ticket. Signing/trust failures are fatal, not warnings or optional CI steps.

## Local versus native evidence

Header/layout and signing-policy tests run locally using synthetic files and
injected read-only tool responses; they do **not** claim to create valid production
signatures. The full Windows/Linux/macOS artifact checks require their native CI
hosts, real bundles and existing managed release credentials. No workflow was
dispatched, signing credential read, release published, or production bundle
re-signed during this local change. All changes remain uncommitted/unpushed.

Local results: **91 focused packaging/signing-policy tests** and **386 full script
tests passed**. Workflow YAML parsing and Arch script syntax validation passed.
The shared restore-policy guards also passed in desktop, Android and iOS after
updating stale source-inspection tests for the split production modules. Native
Windows/Linux payload execution and macOS production signing/notarization remain
required CI checks, not locally claimed results.
