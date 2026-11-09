# No Land Runtime — on the cloud PC desktop

## Using it

1. Provision the cloud PC normally. Existing PCs get the same setup when
   **Provisioning** is resumed; no need to destroy or recreate the instance.
2. Inside the streamed desktop, double-click **No Land Runtime**.
3. Drop a Linux x86-64 executable or AppImage into the small launcher, or click
   **Choose an app…**. Select a file on the **cloud PC**, not your Mac/Windows PC.
4. Confirm that you trust the application. The launcher validates its format,
   checks runtime/graphics/audio support, registers it and requests its launch.
   Registered apps also have a **Run** button in the launcher.

No client Launch Library setup panel, external runtime release URLs, signing
keys, or manual activation are required. The normal **Launch PC** and Steam
routes are unchanged. Windows `.exe`, ARM and 32-bit applications are not
supported by this runtime route.

## Automatic provisioning

Both the new-instance and existing-instance provisioning flows:

- Transfer the exact bundled, Cargo-locked launcher sources over the existing
  authenticated SSH connection, verify their SHA256 and build on the cloud PC.
- Install parent-side Python/GTK launcher prerequisites, Bubblewrap and a
  fixed-function X11 credential helper. Applications never run as root.
- Download the official Ubuntu Base 24.04.3 x86-64 archive over HTTPS and verify
  its pinned SHA256 (from Canonical's published `SHA256SUMS`).
- Prepare a separate Ubuntu 24.04 userspace under `/opt/noland/runtime/images/`,
  installing modern application libraries **inside that image**, not on the
  parent OS. Ubuntu's normal package authentication remains enabled.
- Validate the newer-glibc probe and atomically activate the finished image.
- Create a **No Land Runtime** menu entry and desktop icon for the desktop user.

There is **no separately signed No Land runtime release requirement**. The
official-base digest and standard package verification protect downloads without
requiring users or CI to generate a signing key. The optional `runtime-artifacts`
workflow is for standalone image experiments, not a prerequisite for this flow.

Provisioning uses an exclusive install lock, staged image directories and
resumable downloads. Re-running it reuses the installed image and unchanged
companion. It does not overwrite apps or saves. The first install needs at least
4 GiB of free space and adds download/build time to provisioning.

Failures are reported in provisioning events and logs; the ordinary streamed
desktop remains available. **Resume Provisioning** to retry. A desktop icon
alone does not mean the runtime is fully installed: the launcher shows an error
when its image is unavailable. There is no silent native/PRoot fallback.

## Isolation and compatibility

The runtime provides newer Linux libraries, a read-only base image and separate
per-app HOME/config/cache/save directories. Each application runs using the real
desktop UID with an unprivileged Bubblewrap backend. Its parent application
directory is mounted read-only, while its own app-data remains writable.

**This is not a security sandbox.** Display/X11, audio, network and selected GPU
devices are shared with the parent desktop. When available, `/dev/input` and
read-only `/run/udev` metadata and `/sys` are also shared so apps can discover streamed
controllers, including controllers connected after launch. This exposes other
parent input devices too; parent device permissions still apply. Only run trusted
applications. Streaming must first create an accessible controller on the cloud
PC; this runtime does not forward a local controller by itself.
No parent libc replacement, NVIDIA driver installation, Xorg/Sunshine restart,
replacement audio server, or new public network service is introduced.

Namespace support, actual NVIDIA graphics injection and the existing No Land
X11/audio session are required. Diagnostics check glibc, X11, hardware OpenGL,
Vulkan, PulseAudio and a small visible GPU-rendered surface before a launch.
Unsupported hosts produce an error instead of being labelled gaming-ready.

One live cloud session and Lichii startup were verified on 2026-11-09 after
repairing image/driver dependencies and a timezone bind target. See
[the validation record](runtime-lichii-validation.md). No game was loaded;
audible audio, controller/input behaviour and emulator performance still require
testing. Session checks are not general application-compatibility guarantees.

## Files and logs

```text
/usr/local/bin/noland-runtime                  bundled-source engine/desktop launcher
/etc/noland-runtime/image.json                 root-owned active system-image receipt
/opt/noland/runtime/images/<id>/               provisioned Ubuntu userspace
/var/cache/noland-runtime/                     checksum-verified base download
~/.local/share/applications/noland-runtime.desktop
~/Desktop/noland-runtime.desktop               localized desktop directory when configured
~/.local/share/noland-runtime/apps/             registered application metadata
~/.local/share/noland-runtime/app-data/<id>/     per-app HOME and saves
~/.local/share/noland-runtime/cache/             extracted AppImages
~/.local/state/noland-runtime/                  diagnostics and launch logs
/run/user/<uid>/noland-runtime/<session>/        ephemeral display credentials/driver bindings
```

When `/run/user/<uid>` is unavailable, private session resources use the user's
state directory. A supervisor owns them until the app exits; closing the launcher
does not kill the application. A hard crash may leave stale session directories.

Back up app metadata, source applications and `app-data`, not runtime images,
caches, machine-specific drivers or credentials. Automatic per-runtime-app
shared-storage restore is not finished; keep manual backups.

## CLI (cloud PC terminal, as the desktop user)

```sh
noland-runtime gui
noland-runtime status --json
noland-runtime doctor --json
noland-runtime register --file '/home/user/Downloads/Trusted App.AppImage' --make-executable
noland-runtime launch --app-id <registered-id>
noland-runtime create-shortcut --app-id <registered-id>
```

`--make-executable` grants only the owner's execute bit after Linux ELF and
architecture validation. The GUI requires confirmation before using it. Filenames
are always literal command arguments, never shell code.

## Validation

Offline tests cover file-URI handling, argument safety, unsupported app formats,
permission changes, runtime registration and provisioning wiring. Linux CI opens
the actual GTK drop window under Xvfb before an image is installed. Full cloud
provisioning (package downloads/chroot permissions), GPU launch, audible audio,
stream capture and persistence must also be tested on a live supported instance.
