#define SDL_MAIN_HANDLED

#include <SDL.h>

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <assert.h>
#include <stdio.h>
#include <string.h>

// Exercise the real polling/slot logic without opening a stream or exporting
// test-only entry points from the controller manager.
#include "../src/noland_controller_manager.c"

typedef struct test_controller_event {
  bool arrival;
  int16_t slot;
  uint16_t mask;
  uint8_t type;
  uint32_t buttons;
  uint8_t lt;
  uint8_t rt;
  int16_t ls_x;
  int16_t ls_y;
  int16_t rs_x;
  int16_t rs_y;
} test_controller_event_t;

struct nl_runtime {
  test_controller_event_t events[128];
  size_t event_count;
  size_t arrival_count;
};

nl_result_t nl_send_controller_arrival(nl_runtime_t* runtime, uint8_t controller_number, uint16_t active_gamepad_mask, uint8_t controller_type, uint32_t supported_button_flags, uint16_t capabilities) {
  test_controller_event_t* event;
  (void)supported_button_flags;
  (void)capabilities;
  assert(runtime->event_count < SDL_arraysize(runtime->events));
  event = &runtime->events[runtime->event_count++];
  memset(event, 0, sizeof(*event));
  event->arrival = true;
  event->slot = controller_number;
  event->mask = active_gamepad_mask;
  event->type = controller_type;
  runtime->arrival_count++;
  return NL_RESULT_OK;
}

nl_result_t nl_send_controller_state(nl_runtime_t* runtime, int16_t controller_number, int16_t active_gamepad_mask, int32_t button_flags, uint8_t left_trigger, uint8_t right_trigger, int16_t left_stick_x, int16_t left_stick_y, int16_t right_stick_x, int16_t right_stick_y) {
  test_controller_event_t* event;
  assert(runtime->event_count < SDL_arraysize(runtime->events));
  event = &runtime->events[runtime->event_count++];
  memset(event, 0, sizeof(*event));
  event->slot = controller_number;
  event->mask = (uint16_t)active_gamepad_mask;
  event->buttons = (uint32_t)button_flags;
  event->lt = left_trigger;
  event->rt = right_trigger;
  event->ls_x = left_stick_x;
  event->ls_y = left_stick_y;
  event->rs_x = right_stick_x;
  event->rs_y = right_stick_y;
  return NL_RESULT_OK;
}

int LiSendControllerBatteryEvent(uint8_t controllerNumber, uint8_t batteryState, uint8_t batteryPercentage) {
  (void)controllerNumber;
  (void)batteryState;
  (void)batteryPercentage;
  return 0;
}

int LiSendControllerMotionEvent(uint8_t controllerNumber, uint8_t motionType, float x, float y, float z) {
  (void)controllerNumber;
  (void)motionType;
  (void)x;
  (void)y;
  (void)z;
  return 0;
}

#if SDL_VERSION_ATLEAST(2, 24, 0)
static SDL_Joystick* attach_controller(const char* name, uint16_t vendor, uint16_t product) {
  SDL_VirtualJoystickDesc descriptor;
  SDL_Joystick* joystick;
  int index;
  memset(&descriptor, 0, sizeof(descriptor));
  descriptor.version = SDL_VIRTUAL_JOYSTICK_DESC_VERSION;
  descriptor.type = SDL_JOYSTICK_TYPE_GAMECONTROLLER;
  descriptor.naxes = SDL_CONTROLLER_AXIS_MAX;
  descriptor.nbuttons = SDL_CONTROLLER_BUTTON_MAX;
  descriptor.vendor_id = vendor;
  descriptor.product_id = product;
  descriptor.name = name;
  index = SDL_JoystickAttachVirtualEx(&descriptor);
  assert(index >= 0);
  assert(SDL_IsGameController(index));
  joystick = SDL_JoystickOpen(index);
  assert(joystick != NULL);
  assert(SDL_JoystickSetVirtualAxis(joystick, SDL_CONTROLLER_AXIS_TRIGGERLEFT, -32768) == 0);
  assert(SDL_JoystickSetVirtualAxis(joystick, SDL_CONTROLLER_AXIS_TRIGGERRIGHT, -32768) == 0);
  return joystick;
}

static void detach_controller(SDL_Joystick* joystick) {
  SDL_JoystickID id = SDL_JoystickInstanceID(joystick);
  int i;
  for (i = 0; i < SDL_NumJoysticks(); i++) {
    if (SDL_JoystickGetDeviceInstanceID(i) == id) {
      assert(SDL_JoystickDetachVirtual(i) == 0);
      SDL_JoystickClose(joystick);
      return;
    }
  }
  assert(!"virtual controller not found");
}

#if defined(_WIN32)
static SDL_Joystick* message_controller;

static LRESULT CALLBACK test_controller_window_proc(HWND window, UINT message, WPARAM w_param, LPARAM l_param) {
  if (message == WM_APP + 1) {
    assert(SDL_JoystickSetVirtualButton(message_controller, SDL_CONTROLLER_BUTTON_B, SDL_PRESSED) == 0);
    return 0;
  }
  return DefWindowProcW(window, message, w_param, l_param);
}

static void test_windows_message_dispatch(nl_controller_manager_t* manager, nl_runtime_t* runtime, SDL_Joystick* joystick) {
  WNDCLASSW window_class;
  HWND window;
  const test_controller_event_t* state;
  memset(&window_class, 0, sizeof(window_class));
  window_class.hInstance = GetModuleHandleW(NULL);
  window_class.lpszClassName = L"NolandControllerMessageTest";
  window_class.lpfnWndProc = test_controller_window_proc;
  assert(RegisterClassW(&window_class) != 0);
  window = CreateWindowExW(0, window_class.lpszClassName, L"", 0, 0, 0, 0, 0, HWND_MESSAGE, NULL, window_class.hInstance, NULL);
  assert(window != NULL);
  message_controller = joystick;
  assert(PostMessageW(window, WM_APP + 1, 0, 0));
  nl_sync_controllers(manager);
  state = &runtime->events[runtime->event_count - 1];
  assert((state->buttons & NL_B_FLAG) != 0);
  assert(DestroyWindow(window));
  assert(UnregisterClassW(window_class.lpszClassName, window_class.hInstance));
}
#endif

static void test_controller_polling(void) {
  nl_runtime_t runtime;
  nl_controller_manager_t* manager;
  SDL_Joystick* xbox;
  SDL_Joystick* sony;
  SDL_Joystick* replacement;
  const test_controller_event_t* state;
  size_t event_count;
  size_t replacement_event;
  int unmapped_index;
  int i;
  memset(&runtime, 0, sizeof(runtime));
  manager = nl_controller_manager_create();
  assert(manager != NULL);
  manager->runtime = &runtime;

  xbox = attach_controller("Test Xbox Controller", 0x045e, 0x02ea);
  assert(SDL_JoystickSetVirtualButton(xbox, SDL_CONTROLLER_BUTTON_A, SDL_PRESSED) == 0);
  assert(SDL_JoystickSetVirtualButton(xbox, SDL_CONTROLLER_BUTTON_DPAD_UP, SDL_PRESSED) == 0);
  assert(SDL_JoystickSetVirtualAxis(xbox, SDL_CONTROLLER_AXIS_LEFTX, 32767) == 0);
  assert(SDL_JoystickSetVirtualAxis(xbox, SDL_CONTROLLER_AXIS_LEFTY, -32768) == 0);
  assert(SDL_JoystickSetVirtualAxis(xbox, SDL_CONTROLLER_AXIS_RIGHTY, 32767) == 0);
  nl_sync_controllers(manager);
  assert(runtime.arrival_count == 1);
  assert(runtime.events[0].arrival);
  assert(runtime.events[0].slot == 0);
  assert(runtime.events[0].mask == 1);
  assert(runtime.events[0].type == LI_CTYPE_XBOX);
  assert(SDL_JoystickSetVirtualAxis(xbox, SDL_CONTROLLER_AXIS_TRIGGERLEFT, 32767) == 0);
  nl_sync_controllers(manager);
  state = &runtime.events[runtime.event_count - 1];
  assert(state->buttons == (NL_A_FLAG | NL_UP_FLAG));
  assert(state->ls_x == 32767);
  assert(state->ls_y == 32767);
  assert(state->rs_y == -32767);
  assert(state->lt == 255);
  assert(state->rt == 0);

  event_count = runtime.event_count;
  nl_sync_controllers(manager);
  assert(runtime.arrival_count == 1);
  assert(runtime.event_count == event_count);
  assert(manager->gamepad_mask == 1);

#if defined(_WIN32)
  test_windows_message_dispatch(manager, &runtime, xbox);
#endif

  sony = attach_controller("Test DualSense Controller", 0x054c, 0x0ce6);
  nl_sync_controllers(manager);
  assert(runtime.arrival_count == 2);
  assert(runtime.events[runtime.event_count - 2].slot == 1);
  assert(runtime.events[runtime.event_count - 2].type == LI_CTYPE_PS);
  assert(manager->gamepad_mask == 3);

  // A removal and replacement may be delivered in the same poll. The old
  // player-one slot must be released before assigning the replacement.
  detach_controller(xbox);
  replacement = attach_controller("Replacement Xbox Controller", 0x045e, 0x02ea);
  replacement_event = runtime.event_count;
  nl_sync_controllers(manager);
  assert(runtime.arrival_count == 3);
  assert(runtime.events[replacement_event].slot == 0);
  assert(!runtime.events[replacement_event].arrival);
  assert(runtime.events[replacement_event].mask == 2);
  assert(runtime.events[replacement_event].buttons == 0);
  assert(runtime.events[replacement_event + 1].arrival);
  assert(runtime.events[replacement_event + 1].slot == 0);
  assert(runtime.events[replacement_event + 1].mask == 3);
  assert(manager->gamepad_mask == 3);

  detach_controller(replacement);
  nl_sync_controllers(manager);
  state = &runtime.events[runtime.event_count - 1];
  assert(!state->arrival && state->slot == 0 && state->mask == 2 && state->buttons == 0);
  detach_controller(sony);
  nl_sync_controllers(manager);
  state = &runtime.events[runtime.event_count - 1];
  assert(!state->arrival && state->slot == 1 && state->mask == 0 && state->buttons == 0);
  assert(manager->gamepad_mask == 0);

  event_count = runtime.event_count;
  unmapped_index = SDL_JoystickAttachVirtual(SDL_JOYSTICK_TYPE_UNKNOWN, 2, 2, 0);
  assert(unmapped_index >= 0);
  assert(!SDL_IsGameController(unmapped_index));
  nl_sync_controllers(manager);
  assert(runtime.event_count == event_count);
  assert(SDL_JoystickDetachVirtual(unmapped_index) == 0);

  for (i = 0; i < NL_MAX_GAMEPADS; i++) {
    assert(manager->gamepads[i].controller == NULL);
  }
  nl_controller_manager_destroy(manager);
}
#endif

int main(void) {
#if SDL_VERSION_ATLEAST(2, 24, 0)
  SDL_SetMainReady();
  // Do not open, change player LEDs on, or otherwise exercise physical
  // controllers attached to the developer/test machine.
  SDL_SetHint(SDL_HINT_JOYSTICK_HIDAPI, "0");
  SDL_SetHint(SDL_HINT_JOYSTICK_IOKIT, "0");
  SDL_SetHint(SDL_HINT_JOYSTICK_MFI, "0");
  SDL_SetHint(SDL_HINT_JOYSTICK_WGI, "0");
  SDL_SetHint(SDL_HINT_JOYSTICK_RAWINPUT, "0");
  SDL_SetHint(SDL_HINT_XINPUT_ENABLED, "0");
  SDL_SetHint(SDL_HINT_DIRECTINPUT_ENABLED, "0");
  SDL_SetHint(SDL_HINT_JOYSTICK_ALLOW_BACKGROUND_EVENTS, "1");
  assert(SDL_Init(SDL_INIT_GAMECONTROLLER) == 0);
  if (SDL_NumJoysticks() != 0) {
    fprintf(stderr, "SKIP: test requires an isolated virtual-controller environment\n");
    SDL_Quit();
    return 77;
  }
  test_controller_polling();
  SDL_Quit();
  puts("controller mapping, duplicate detection, hotplug and forwarding tests passed");
  return 0;
#else
  fprintf(stderr, "SKIP: virtual-controller tests require SDL 2.24 or newer\n");
  return 77;
#endif
}
