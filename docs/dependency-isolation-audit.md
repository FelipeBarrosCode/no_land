# Dependency isolation audit

Source review: 2026-10-13. This is a packaging/runtime review, **not a clean-machine certification**. Windows DLL loading and Linux package behavior still require their native CI runners.

## Fixed in this change

### Windows OpenSSH (high priority)

Previously CI selected each tool independently from PATH, the bootstrap cache copied executables without DLLs, and packaging put DLLs in a subdirectory that Windows would not search when starting the flat executables. Installer verification exercised only `ssh -V`.

- `scripts/windows-openssh.mjs` selects one distribution directory. Mixed overrides, incomplete distributions, wrong architectures, and missing non-OS DLL imports (including transitive and delay imports) fail the build.
- Bootstrap and sidecar preparation copy the distribution's executables, DLLs, helper executables, and available license/notice files together under `binaries/ssh-runtime/<target>/`.
- Runtime SSH/SCP/keygen lookup uses that directory, not legacy flat copies. Explicit `NOLAND_*_BIN` overrides remain an intentional escape hatch.
- Windows installer verification runs SSH, SCP usage, Ed25519 generation, and public-key derivation without build-host PATH or runtime overrides. This checks loaded symbols too, not only dependency filenames.

Builds use a complete Windows `System32/OpenSSH` distribution or `NOLAND_OPENSSH_ROOT`. They do not choose Git's tools from PATH. A missing or incompatible cached distribution fails closed; replace that specific cache from a complete matching distribution rather than adding arbitrary OpenSSL DLLs.

### Windows x64 microphone plugins (high priority)

`tauri.windows.x64.conf.json` included capture/conversion plugins but omitted the appsrc, Opus, RTP, and UDP plugins used by `mic-sidecar/src/pipeline.rs`. Added those resources and a regression test. Installer verification now starts/stops the real microphone pipeline using a synthetic sine source, packaged plugin paths, a fresh registry, and no host GStreamer environment. No microphone hardware or remote host is needed.

Windows ARM64 intentionally uses the unsupported microphone stub, not the x64 GStreamer DLLs.

## Remaining findings / intentional external dependencies

| Area | Finding | Follow-up |
| --- | --- | --- |
| Linux SSH/SCP/keygen | Wrappers execute `/usr/bin` OpenSSH. `.deb` declares `openssh-client`; Arch declares `openssh`. This is not a bundled OpenSSH distribution. | Keep distro dependencies for native packages. Do not advertise the AppImage as dependency-free: it uses the same wrappers. |
| Linux streaming/microphone | Uses system GStreamer, GTK/WebKit, GLib, graphics/audio libraries. Current code explicitly removes older managed Linux GStreamer artifacts. | Keep desktop/media stacks ABI-consistent. Validate minimal supported distro images, including runtime-loaded plugins, not just CI's already-installed dependencies. |
| Linux optional utilities | `xrandr` display detection, `systemd-inhibit` sleep prevention, `ping` MTU probing, and `pkexec` legacy tunnel cleanup are external commands and are not explicitly listed in Debian dependencies. Some can be absent on minimal or non-systemd desktops. | Decide required vs optional support before adding distro-specific packages; test fallback/error behavior. `libxrandr2` is not the `xrandr` executable. |
| Windows GStreamer DLL closure | Runtime bin DLLs are packaged and scoped through child PATH; required plugins are explicitly selected. The previous installer smoke test did not initialize this pipeline. | New synthetic pipeline test checks actual initialization. A complete PE closure audit of every shipped plugin and testing on a clean Windows VM remain advisable; unused plugins and optional `LoadLibrary` paths are not certified by this test. |
| Windows C/C++ runtime | Static SDL2/Opus/Moonlight linkage is configured, but static library linkage alone does not prove static CRT linkage. | Inspect release PE imports on Windows and bundle any required redistributable DLLs or install the appropriate runtime. Do not treat the CI runner's installed runtimes as evidence of completeness. |
| Windows WebView2 | Normal architecture overlays use the offline installer, not a fixed-version runtime inside the app. Store overlay specifies an embedded bootstrapper; final behavior depends on configuration merge order. | Verify the effective Store configuration and first-run behavior without internet. WebView2 is a declared platform prerequisite, not an OpenSSL dependency. |
| macOS SSH | Packaged wrappers call `/usr/bin/ssh`, `/usr/bin/scp`, `/usr/bin/ssh-keygen`. | Intentional OS dependency. No bundled OpenSSH/OpenSSL requirement for these wrappers. |
| macOS native libraries | Streaming links crypto/Opus/SDL2 dynamically; bundle-fix scripts stage/rewrite dependencies and GStreamer, and verification checks absolute linkage/signatures. | Existing checks are useful, but cover more runtime-loaded paths and clean-machine launches. OS frameworks remain external by design. |
| Local OS integrations | PowerShell/netsh/task utilities and Media Foundation on Windows; Apple frameworks/system tools; Linux desktop portals, keyring services and privilege brokers. | These must remain OS-owned. Missing/disabled services need explicit diagnostics rather than bundling replacements. |
| Remote provisioning | Sunshine, drivers, PipeWire, Python, rclone and other Linux packages are installed/used on the remote VM. `MicReceiverProvisioner` also has a source-build path installing Rust/build dependencies. | Remote setup is intentionally network/package-manager dependent, not an offline self-contained desktop runtime. Distinguish immutable packaged agents from legacy source-build paths. |
| Frontend / HTTP | React/xterm/Tauri JS dependencies are compiled into the frontend; no external font/CDN runtime import found in the reviewed source. Rust HTTP uses rustls with default reqwest features disabled. | No separate Node/npm or OpenSSL installation is required for these paths. Cloud APIs and external links remain network features. |

## Coverage and limits

Reviewed frontend dependencies/external imports, Rust manifests, native CMake/link configuration, local process launches in desktop services and the network helper, microphone runtime lookup, bootstrap/staging scripts, Tauri resource overlays, installer hooks, bundle verification, and remote provisioning boundaries.

PE import checks verify filenames and architecture, not all exported symbols. Native smoke tests cover symbols on executed paths. Neither replaces testing the installed release on clean supported Windows x64/ARM64, macOS, and Linux images. Build-time tools (Rust, Node, CMake, clang, pkg-config, download utilities) are not end-user runtime dependencies.

Run portable regression checks with `node --test scripts/windows-openssh.test.mjs`. Native installer tests run through `scripts/verify-bundled-sidecars.mjs` in the existing release workflow.
