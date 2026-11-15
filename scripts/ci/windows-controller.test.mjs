import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';

const source = path => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
const manager = source('src-tauri/native/noland-moonlight/src/noland_controller_manager.c');
const sync = manager.split('static void nl_sync_controllers(')[1].split('static int SDLCALL nl_controller_thread_main(')[0];

test('Windows controller polling dispatches its own Raw Input and HID message queue before reading state', () => {
  const pump = sync.match(/#if defined\(_WIN32\)([\s\S]*?)#endif/u)?.[1];
  assert.ok(pump, 'the Windows compiler platform guard covers x64 and ARM64');
  assert.match(pump, /PeekMessageW\(&message, NULL, 0, 0, PM_REMOVE\)/u);
  assert.match(pump, /TranslateMessage\(&message\)/u);
  assert.match(pump, /DispatchMessageW\(&message\)/u);
  assert.ok(sync.indexOf('DispatchMessageW(') < sync.indexOf('SDL_GameControllerUpdate();'));
  assert.doesNotMatch(sync, /SDL_(?:PumpEvents|PollEvent|WaitEvent)/u, 'do not steal the stream window SDL event consumer');
});

test('batched controller removal frees its player slot before the next arrival', () => {
  const update = sync.indexOf('SDL_GameControllerUpdate();');
  const detach = sync.indexOf('nl_detach_controller(manager, state);');
  const attach = sync.indexOf('nl_attach_controller(manager, i);');
  assert.ok(update >= 0 && detach > update && attach > detach);
  assert.match(sync.slice(update, attach), /!SDL_GameControllerGetAttached\(state->controller\)/u);
  assert.match(manager, /nl_find_gamepad_by_instance_id\(manager, SDL_JoystickInstanceID\(joystick\)\) != NULL/u);
  assert.match(manager, /SDL_SetHint\(SDL_HINT_JOYSTICK_ALLOW_BACKGROUND_EVENTS, "1"\)/u);
});

test('native virtual-controller coverage includes Windows message delivery, mapping and unsupported devices', () => {
  const nativeTest = source('src-tauri/native/noland-moonlight/tests/controller_manager_test.c');
  const cmake = source('src-tauri/native/noland-moonlight/CMakeLists.txt');
  assert.match(nativeTest, /#include "\.\.\/src\/noland_controller_manager\.c"/u);
  assert.match(nativeTest, /PostMessageW\(window, WM_APP \+ 1, 0, 0\)/u);
  assert.match(nativeTest, /state->buttons & NL_B_FLAG/u);
  assert.match(nativeTest, /runtime\.events\[0\]\.type == LI_CTYPE_XBOX/u);
  assert.match(nativeTest, /type == LI_CTYPE_PS/u);
  assert.match(nativeTest, /!SDL_IsGameController\(unmapped_index\)/u);
  assert.match(nativeTest, /SDL_NumJoysticks\(\) != 0/u, 'skip rather than touch physical controllers');
  assert.match(nativeTest, /#undef NDEBUG/u, 'Release builds must retain assertions and their checks');
  assert.match(cmake, /add_test\(NAME noland_controller_manager_test COMMAND noland_controller_manager_test\)/u);
  assert.match(cmake, /noland_controller_manager_test PROPERTIES SKIP_RETURN_CODE 77/u);
});

test('Windows production controller dependencies remain native SDL, not an unbundled SDL or GameInput DLL', () => {
  const bootstrap = source('scripts/bootstrap-native-deps.mjs');
  const build = source('src-tauri/build.rs');
  assert.match(bootstrap, /SDL2-2\.30\.10/u);
  assert.match(bootstrap, /const shared = isMacTarget\(targetTriple\) \? 'ON' : 'OFF'/u);
  assert.match(bootstrap, /const staticLibrary = isMacTarget\(targetTriple\) \? 'OFF' : 'ON'/u);
  assert.match(build, /cargo:rustc-link-lib=static=SDL2-static/u);
  for (const library of ['dinput8', 'dxguid', 'setupapi', 'user32']) {
    assert.ok(build.includes(`"${library}"`), library);
  }
});

test('Windows uses the supported native controller branch and sends only while streaming', () => {
  assert.match(manager, /#if defined\(__APPLE__\) \|\| defined\(__linux__\) \|\| defined\(_WIN32\)/u);
  const native = source('src-tauri/native/noland-moonlight/src/noland_moonlight.c');
  const connected = native.split('static void nl_connection_started(void)')[1].split('static void nl_connection_terminated(')[0];
  const streaming = connected.indexOf('NL_STREAM_STATE_STREAMING');
  assert.ok(streaming >= 0 && streaming < connected.indexOf('nl_controller_manager_start('));
  const terminated = native.split('static void nl_connection_terminated(')[1].split('static void nl_connection_log_message(')[0];
  assert.match(terminated, /nl_controller_manager_stop\(runtime->controller_manager\)/u);
  const canSend = native.split('static bool nl_runtime_can_send_input(')[1].split('static void nl_runtime_finish_worker(')[0];
  assert.match(canSend, /ready = runtime->state == NL_STREAM_STATE_STREAMING;/u);
  for (const name of ['arrival', 'state']) {
    const send = native.split(`nl_result_t nl_send_controller_${name}(`)[1];
    const guard = send.indexOf('if (!nl_runtime_can_send_input(runtime))');
    assert.ok(guard >= 0 && guard < send.indexOf('result = LiSend'));
  }
});

test('frontend does not silently duplicate native controllers through the Web Gamepad API', () => {
  const files = directory => readdirSync(new URL(`../../${directory}/`, import.meta.url), { withFileTypes: true })
    .flatMap(entry => entry.isDirectory()
      ? files(`${directory}/${entry.name}`)
      : /\.(?:ts|tsx)$/u.test(entry.name) ? [`${directory}/${entry.name}`] : []);
  for (const path of files('src')) {
    assert.doesNotMatch(source(path), /\bgetGamepads\s*\(|["']gamepad(?:connected|disconnected)["']/u, path);
  }
  const native = source('src-tauri/native/noland-moonlight/src/noland_moonlight.c');
  assert.match(native, /nl_controller_manager_start\(runtime->controller_manager, runtime\)/u);
  assert.match(native, /LiSendControllerArrivalEvent\(controller_number, active_gamepad_mask/u);
  assert.match(native, /LiSendMultiControllerEvent\(\(short\)controller_number/u);
});
