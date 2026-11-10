# Lichii runtime validation — 2026-11-09

Connected to the active cloud PC using the local app state's SSH configuration.
No credentials are included in this record.

## Failures reproduced and repaired

- NVIDIA EGL libraries required `libwayland-server.so.0`, missing from the image.
  Added Ubuntu's `libwayland-server0` to the image package list and installed it
  inside the existing cloud runtime (not on the parent desktop).
- Driver discovery incorrectly included `nvidia-settings` GTK2/GTK3 UI libraries,
  which pulled obsolete parent GTK/PCRE dependencies into driver validation.
  Excluded those UI libraries and included CUDA/NVCUVID driver SONAMEs required
  by the remaining NVIDIA driver closure.
- Ubuntu Base's absolute `/etc/localtime` symlink could not be used as a file
  overlay target after Bubblewrap mounted the image read-only. Replaced that
  runtime-image symlink with a regular timezone file. Provisioning now
  materializes `hosts`, `resolv.conf` and `localtime` overlay targets.
- Diagnostic failures now include command stderr/exit status instead of only
  the generic “diagnostics did not pass” message.

No driver replacement, parent libc update, desktop restart, namespace bypass or
PRoot fallback was used.

## Observed results

All seven checks passed: glibc 2.39; actual glibc-2.38 symbol; X11; NVIDIA OpenGL;
NVIDIA Vulkan; PulseAudio connection; visible GPU surface at approximately 60 FPS.
Hardware was an NVIDIA GeForce RTX 4070 Ti SUPER with driver 580.105.08.

Registered `/home/user/Downloads/lichii-linux-x86_64-pgo.AppImage` and launched it
using the runtime supervisor, as desktop UID 1000. Confirmed:

- Visible window: `Lichii | build-125 | Clang 23.1.6` (1960×1232).
- Live process executable: `/app/usr/bin/lichii` inside the Bubblewrap runtime.
- Observed application PID: `78528`; supervisor PID: `77708`.
- App ID: `66af7feb-a19a-4331-babf-458c6e10ab40`.
- Private process record on the cloud PC:
  `/home/user/.local/state/noland-runtime/lichii-process.json`.
- Launch log on the cloud PC:
  `/home/user/.local/state/noland-runtime/launch-87925d7c-6f72-4465-97e9-2ab244815287.log`.

PIDs are observations, not durable identifiers; the process record also stores
the `/proc` start identity and observation timestamp. Lichii was left running.

**No game was loaded.** This validates application startup and the runtime's
GPU/display/audio connection, not game compatibility, audible playback,
controllers, save migration, or emulator performance. The prior startup failure
did not recur in this launch, but that is not a general compatibility guarantee.

## Controller investigation — 2026-11-09

The Mac detected an `8BitDo Ultimate wireless` Bluetooth gamepad. The cloud PC
also exposed `Sunshine X-Box One (virtual) pad` through `/dev/input/event12`;
udev classified it with `ID_INPUT_JOYSTICK=1`. The desktop user belonged to the
`input` group. However, the running Lichii process had no `/dev/input`,
`/run/udev` or input sysfs tree in its mount namespace, blocking discovery.

The runtime now shares the input-device directory and read-only udev/sysfs
metadata. Directory binds preserve visibility of newly added devices. Local
tests passed (23 total); the four engine tests also passed on the cloud PC.
The full cloud test run hit an unrelated fixture assumption: the extraction
cancellation test expects no active image, but `Store::activation()` falls back
to the cloud PC's installed system image. No system-image receipt was removed
or changed to work around that test.

Built and installed the updated companion on the cloud PC, preserving the old
binary and controller configuration under
`/var/cache/noland-runtime/controller-build-8jCVO5Mr/backup/`. Closed Lichii via
its normal confirmation dialog and relaunched the same registered application.
The new process (observed PID `162163`) had all three mounts and held an open
file descriptor to `/dev/input/event12`, the Sunshine virtual Xbox controller.
Installed binary SHA256:
`3e0285bc4e8c51ec62929afc55bf3f92daebaf45542a1882449ebcee4dbd551a`.

Player 1's saved A/B bindings still use `engine:keyboard`, the left stick uses
keyboard buttons, and the right stick uses `engine:mouse`. These mappings were
not overwritten. Selecting the virtual Xbox controller in Lichii's Controls
settings and confirming button/stick response remains necessary; opening its
device node alone does not prove gameplay input works. The game was not
automatically reloaded.
