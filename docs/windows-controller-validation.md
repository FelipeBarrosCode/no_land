# Windows controller review

Source review and host-side virtual-controller tests: November 15, 2026.
**Not a Windows build or physical-controller certification.**

## Input route and dependencies

- The desktop frontend does **not** use `navigator.getGamepads()` or browser
  gamepad events. WebView2's secure-context/user-activation rules are therefore
  not the controller forwarding path, and there is no browser fallback that
  overrides a native controller selection.
- `nl_connection_started()` in
  `src-tauri/native/noland-moonlight/src/noland_moonlight.c` starts the SDL
  controller manager. It forwards arrivals and state through
  `LiSendControllerArrivalEvent()` / `LiSendMultiControllerEvent()` to Sunshine.
  Rumble, trigger rumble, motion, LED and DualSense feedback return to SDL.
- `scripts/bootstrap-native-deps.mjs` builds SDL **2.30.10** statically on Windows.
  `src-tauri/build.rs` links `SDL2-static`, DirectInput/system libraries and the
  streaming core. This route does not require shipping `SDL2.dll` or installing
  Microsoft's modern **GameInput** runtime.
- SDL's Windows backends include Raw Input, XInput, DirectInput,
  Windows.Gaming.Input and HIDAPI when available in the Windows SDK. These are
  distinct from the modern GameInput API. XInput/HID system DLL loading is
  handled by SDL; static SDL linkage does not certify the final package's CRT
  dependencies or a clean Windows installation.

## Evidenced fixes

### Worker-owned Windows messages were not serviced

The manager initializes SDL joystick/controller subsystems on its own worker,
then calls only `SDL_GameControllerUpdate()`. In the pinned SDL source:

- `src/joystick/windows/SDL_windowsjoystick.c` creates a message-only window on
  the initializing thread by default (`SDL_JOYSTICK_THREAD` defaults to false).
- `src/joystick/windows/SDL_rawinputjoystick.c` registers that window for
  `WM_INPUT` / `WM_INPUT_DEVICE_CHANGE`. Xbox input and arrival/removal depend
  on their dispatch.
- `src/hidapi/SDL_hidapi.c` creates another thread-owned discovery window and
  explicitly leaves its message dispatch to the caller's event pump.
- `SDL_GameControllerUpdate()` calls `SDL_JoystickUpdate()`; neither dispatches
  those Windows messages. The stream window's SDL event consumer runs on a
  different thread and cannot service this queue.

`nl_sync_controllers()` now drains the **current worker's Windows message
queue** with `PeekMessageW` / `TranslateMessage` / `DispatchMessageW` before
polling. The `_WIN32` compiler guard applies to both x64 and ARM64. It does not
consume the SDL events used by the stream window. Existing background-input
and Sony enhanced-input hints remain set before joystick initialization.

### Batched reconnect changed player slots

The old polling order opened arrivals before closing detached controllers.
Removing slot zero and connecting its replacement before one poll assigned the
replacement to slot two when another controller occupied slot one. This was
reproduced with real SDL virtual controllers. Detached slots are now released
first; the replacement remains player one, with an explicit neutral removal
packet before its arrival.

Startup inventory logs now include SDL version/platform and joystick
VID/PID/GUID, and explicitly flag unmapped joysticks. Existing mapped hotplug
logs include type, capabilities, slot/mask and send failures.

## Tests and limitations

- `node --test scripts/ci/windows-controller.test.mjs`: portable source-contract
  checks, including the platform guard, pump ordering, slot reuse, static SDL
  configuration, and absence of a duplicate Web Gamepad sender.
- `noland_controller_manager_test`: real SDL virtual devices, stubbed network
  sends. Covers Xbox/Sony type classification, buttons/d-pad, stick inversion
  and extrema, trigger scaling, unchanged/duplicate polls, two controllers,
  batched replacement, neutral removals and unmapped-device rejection.
  Physical devices are not opened: the test skips if any remain enumerated.
  Passed on macOS ARM64 with the existing SDL 2.30.10 dependency, both with
  `-Wall -Wextra -Werror` / `-DNDEBUG` and as the CMake Release/CTest target.
- On Windows that same native test also creates a hidden message-only window
  and posts a message that changes a virtual controller, verifying dispatch
  occurs before forwarding. **This Windows-only test was not run on macOS.**

With native dependencies already available, enable
`BUILD_NOLAND_MOONLIGHT_TESTS`, build target `noland_controller_manager_test`,
then run `ctest --test-dir <native-build-dir> -R noland_controller_manager_test
--output-on-failure`. The existing no-build script lane discovers the JS
suite automatically; it does not compile/run the Windows native test.

Still requires Windows x64/ARM64 hardware/package testing:

1. Xbox wired USB, Bluetooth and Xbox Wireless Adapter; DualShock 4 and
   DualSense wired USB/Bluetooth. Check basic buttons/sticks/triggers before
   optional rumble/sensors/adaptive triggers.
2. Connect before streaming, hotplug while streaming, unplug with a held
   button, reconnect, reconnect the stream, and use two identical controllers.
3. Focus the stream, release mouse capture, alt-tab and restore. The native
   manager deliberately allows background controller input; mouse/keyboard
   capture is separate.
4. Confirm a virtual Sunshine pad appears remotely and the target application's
   selected input device receives it. Successful local SDL/transport sends do
   not prove remote permissions, virtual-device creation, runtime device
   visibility or in-game bindings.

## Reproduction details needed from the affected user

Request the app version, Windows version/architecture, exact controller model,
USB/Bluetooth/adapter connection, whether Windows `joy.cpl` receives input,
whether it works in another native app, connect-before/after-stream behavior,
and whether Steam Input, DS4Windows or HidHide is involved. Do not instruct the
user to install arbitrary DLLs, disable security or change device drivers.

For native debugging, capture `[noland-controller]` logs from a diagnostic
build/debugger, including startup inventory and attach/send failures.
**SDL native logs are not currently bridged into the Rust file logger**, so a
normal exported app diagnostic report may omit them. Unsupported hotplugged
joysticks may also be absent from the startup-only inventory: reconnect the
stream with the device already attached when collecting it.

SDL ignores joysticks without a game-controller mapping. A mapping must match
the exact GUID/transport; use a verified `SDL_GAMECONTROLLERCONFIG` (or SDL's
mapping-file hint) rather than guessing another controller's layout. The app
does not currently offer a controller picker or mapping editor. Native instance
IDs prevent reopening the same SDL device as multiple players, but a separate
virtual device from a remapper can legitimately appear as another controller;
identical VID/PID values alone are not sufficient to merge devices.

## Startup/focus follow-up findings

These findings are confined to controller/native SDL behavior, not permission
storage, MCP authorization or macOS TCC:

- Windows is included in the real manager's supported-platform guard, not its
  no-op fallback. The manager starts after the connection enters `STREAMING`,
  stops on termination, and native send functions reject non-streaming input.
  Portable source-contract tests cover those boundaries.
- `nl_controller_manager_start()` currently acknowledges **thread creation**,
  not successful asynchronous SDL subsystem initialization. The caller in
  `nl_connection_started()` ignores its return value. A later initialization
  failure is only an SDL log; a successful video connection is not evidence
  that controller capture initialized. No blocking-start handshake or new
  permission prompt was introduced in this fix.
- `SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS=1` is pre-existing controller behavior.
  Controller forwarding does not use the mouse/keyboard focus/capture state
  from `noland_desktop_input_sdl.c`. Thus alt-tab/releasing mouse capture does
  **not** pause controllers. This is a product input-policy consideration,
  not browser/OS permission persistence. Changing that policy would require
  explicit controller forwarding gates and neutral/resync packets; disabling
  the SDL hint alone is not a safe substitute in this mixed Tauri/SDL window
  setup. This review preserves the existing policy.
