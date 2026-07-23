#include "noland_controller_manager.h"

#include "noland_moonlight.h"
#include "Limelight.h"

#include <stdlib.h>
#include <string.h>

#if defined(__APPLE__)

#include <SDL.h>
#include <pthread.h>

#define NL_MAX_GAMEPADS 16

#define NL_A_FLAG 0x1000
#define NL_B_FLAG 0x2000
#define NL_X_FLAG 0x4000
#define NL_Y_FLAG 0x8000
#define NL_BACK_FLAG 0x0020
#define NL_SPECIAL_FLAG 0x0400
#define NL_PLAY_FLAG 0x0010
#define NL_LS_CLK_FLAG 0x0040
#define NL_RS_CLK_FLAG 0x0080
#define NL_LB_FLAG 0x0100
#define NL_RB_FLAG 0x0200
#define NL_UP_FLAG 0x0001
#define NL_DOWN_FLAG 0x0002
#define NL_LEFT_FLAG 0x0004
#define NL_RIGHT_FLAG 0x0008
#define NL_MISC_FLAG 0x0800
#define NL_PADDLE1_FLAG 0x10000
#define NL_PADDLE2_FLAG 0x20000
#define NL_PADDLE3_FLAG 0x40000
#define NL_PADDLE4_FLAG 0x80000
#define NL_TOUCHPAD_FLAG 0x100000

typedef struct nl_gamepad_state {
  SDL_GameController* controller;
  SDL_JoystickID joystick_id;
  uint8_t index;
  uint32_t supported_button_flags;
  uint16_t capabilities;
  uint32_t buttons;
  int16_t ls_x;
  int16_t ls_y;
  int16_t rs_x;
  int16_t rs_y;
  uint8_t lt;
  uint8_t rt;
#if SDL_VERSION_ATLEAST(2, 0, 14)
  uint8_t gyro_report_period_ms;
  float last_gyro_event_data[3];
  uint32_t last_gyro_event_time;
  uint8_t accel_report_period_ms;
  float last_accel_event_data[3];
  uint32_t last_accel_event_time;
#endif
} nl_gamepad_state_t;

struct nl_controller_manager {
  nl_runtime_t* runtime;
  bool running;
  bool thread_started;
  bool initialized;
  uint16_t gamepad_mask;
  pthread_t thread;
  pthread_mutex_t mutex;
  nl_gamepad_state_t gamepads[NL_MAX_GAMEPADS];
};

static const uint32_t k_button_map[] = {
  NL_A_FLAG, NL_B_FLAG, NL_X_FLAG, NL_Y_FLAG,
  NL_BACK_FLAG, NL_SPECIAL_FLAG, NL_PLAY_FLAG,
  NL_LS_CLK_FLAG, NL_RS_CLK_FLAG,
  NL_LB_FLAG, NL_RB_FLAG,
  NL_UP_FLAG, NL_DOWN_FLAG, NL_LEFT_FLAG, NL_RIGHT_FLAG,
  NL_MISC_FLAG,
  NL_PADDLE1_FLAG, NL_PADDLE2_FLAG, NL_PADDLE3_FLAG, NL_PADDLE4_FLAG,
  NL_TOUCHPAD_FLAG,
};

static void nl_controller_manager_lock(nl_controller_manager_t* manager) {
  pthread_mutex_lock(&manager->mutex);
}

static void nl_controller_manager_unlock(nl_controller_manager_t* manager) {
  pthread_mutex_unlock(&manager->mutex);
}

static int nl_next_free_slot(nl_controller_manager_t* manager) {
  int i;
  for (i = 0; i < NL_MAX_GAMEPADS; i++) {
    if (manager->gamepads[i].controller == NULL) {
      return i;
    }
  }
  return -1;
}

static nl_gamepad_state_t* nl_find_gamepad_by_instance_id(nl_controller_manager_t* manager, SDL_JoystickID instance_id) {
  int i;
  for (i = 0; i < NL_MAX_GAMEPADS; i++) {
    if (manager->gamepads[i].controller != NULL && manager->gamepads[i].joystick_id == instance_id) {
      return &manager->gamepads[i];
    }
  }
  return NULL;
}

static uint8_t nl_controller_type(SDL_GameController* controller) {
#if SDL_VERSION_ATLEAST(2, 0, 12)
  switch (SDL_GameControllerGetType(controller)) {
    case SDL_CONTROLLER_TYPE_XBOX360:
    case SDL_CONTROLLER_TYPE_XBOXONE:
      return LI_CTYPE_XBOX;
    case SDL_CONTROLLER_TYPE_PS3:
    case SDL_CONTROLLER_TYPE_PS4:
    case SDL_CONTROLLER_TYPE_PS5:
      return LI_CTYPE_PS;
    case SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_PRO:
#if SDL_VERSION_ATLEAST(2, 24, 0)
    case SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_JOYCON_LEFT:
    case SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_JOYCON_RIGHT:
    case SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_JOYCON_PAIR:
#endif
      return LI_CTYPE_NINTENDO;
    default:
      return LI_CTYPE_UNKNOWN;
  }
#else
  (void)controller;
  return LI_CTYPE_UNKNOWN;
#endif
}

static uint16_t nl_controller_capabilities(SDL_GameController* controller) {
  uint16_t capabilities = 0;
#if SDL_VERSION_ATLEAST(2, 0, 14)
  if (SDL_GameControllerGetBindForAxis(controller, SDL_CONTROLLER_AXIS_TRIGGERLEFT).bindType == SDL_CONTROLLER_BINDTYPE_AXIS ||
      SDL_GameControllerGetBindForAxis(controller, SDL_CONTROLLER_AXIS_TRIGGERRIGHT).bindType == SDL_CONTROLLER_BINDTYPE_AXIS) {
    capabilities |= LI_CCAP_ANALOG_TRIGGERS;
  }
  if (SDL_GameControllerHasRumble(controller)) {
    capabilities |= LI_CCAP_RUMBLE;
  }
  if (SDL_GameControllerHasRumbleTriggers(controller)) {
    capabilities |= LI_CCAP_TRIGGER_RUMBLE;
  }
  if (SDL_GameControllerGetNumTouchpads(controller) > 0) {
    capabilities |= LI_CCAP_TOUCHPAD;
    if (SDL_GameControllerGetNumTouchpads(controller) > 1) {
      capabilities |= LI_CCAP_DUAL_TOUCHPAD;
    }
  }
  if (SDL_GameControllerHasSensor(controller, SDL_SENSOR_ACCEL)) {
    capabilities |= LI_CCAP_ACCEL;
  }
  if (SDL_GameControllerHasSensor(controller, SDL_SENSOR_GYRO)) {
    capabilities |= LI_CCAP_GYRO;
  }
  if (SDL_JoystickCurrentPowerLevel(SDL_GameControllerGetJoystick(controller)) != SDL_JOYSTICK_POWER_UNKNOWN) {
    capabilities |= LI_CCAP_BATTERY_STATE;
  }
  if (SDL_GameControllerHasLED(controller)) {
    capabilities |= LI_CCAP_RGB_LED;
  }
#else
  capabilities |= LI_CCAP_ANALOG_TRIGGERS;
#endif
  return capabilities;
}

static uint32_t nl_supported_button_flags(SDL_GameController* controller) {
  uint32_t flags = 0;
#if SDL_VERSION_ATLEAST(2, 0, 14)
  int i;
  for (i = 0; i < (int)(sizeof(k_button_map) / sizeof(k_button_map[0])); i++) {
    if (SDL_GameControllerHasButton(controller, (SDL_GameControllerButton)i)) {
      flags |= k_button_map[i];
    }
  }
#else
  (void)controller;
#endif
  return flags;
}

static const char* nl_result_name(nl_result_t result) {
  switch (result) {
    case NL_RESULT_OK:
      return "OK";
    case NL_RESULT_INVALID_ARGUMENT:
      return "INVALID_ARGUMENT";
    case NL_RESULT_OUT_OF_MEMORY:
      return "OUT_OF_MEMORY";
    case NL_RESULT_NOT_READY:
      return "NOT_READY";
    case NL_RESULT_INVALID_STATE:
      return "INVALID_STATE";
    case NL_RESULT_QUEUE_EMPTY:
      return "QUEUE_EMPTY";
    default:
      return "UNKNOWN";
  }
}

static void nl_send_battery_state(nl_controller_manager_t* manager, nl_gamepad_state_t* state, SDL_JoystickPowerLevel level) {
  uint8_t battery_state;
  uint8_t battery_percentage;

  (void)manager;

  switch (level) {
    case SDL_JOYSTICK_POWER_UNKNOWN:
      battery_state = LI_BATTERY_STATE_UNKNOWN;
      battery_percentage = LI_BATTERY_PERCENTAGE_UNKNOWN;
      break;
    case SDL_JOYSTICK_POWER_WIRED:
      battery_state = LI_BATTERY_STATE_CHARGING;
      battery_percentage = LI_BATTERY_PERCENTAGE_UNKNOWN;
      break;
    case SDL_JOYSTICK_POWER_EMPTY:
      battery_state = LI_BATTERY_STATE_DISCHARGING;
      battery_percentage = 5;
      break;
    case SDL_JOYSTICK_POWER_LOW:
      battery_state = LI_BATTERY_STATE_DISCHARGING;
      battery_percentage = 20;
      break;
    case SDL_JOYSTICK_POWER_MEDIUM:
      battery_state = LI_BATTERY_STATE_DISCHARGING;
      battery_percentage = 50;
      break;
    case SDL_JOYSTICK_POWER_FULL:
      battery_state = LI_BATTERY_STATE_DISCHARGING;
      battery_percentage = 90;
      break;
    default:
      return;
  }

  (void)LiSendControllerBatteryEvent(state->index, battery_state, battery_percentage);
}

static void nl_send_gamepad_state(nl_controller_manager_t* manager, nl_gamepad_state_t* state) {
  if (manager == NULL || state == NULL || manager->runtime == NULL) {
    return;
  }

  {
    nl_result_t result = nl_send_controller_state(
        manager->runtime,
        (int16_t)state->index,
        (int16_t)manager->gamepad_mask,
        (int32_t)state->buttons,
        state->lt,
        state->rt,
        state->ls_x,
        state->ls_y,
        state->rs_x,
        state->rs_y);
    if (result != NL_RESULT_OK) {
      SDL_LogWarn(SDL_LOG_CATEGORY_APPLICATION,
                  "[noland-controller] state send failed slot=%u mask=0x%04x result=%s buttons=0x%08x lt=%u rt=%u ls=(%d,%d) rs=(%d,%d)",
                  state->index,
                  manager->gamepad_mask,
                  nl_result_name(result),
                  state->buttons,
                  state->lt,
                  state->rt,
                  state->ls_x,
                  state->ls_y,
                  state->rs_x,
                  state->rs_y);
    }
  }
}

static bool nl_refresh_gamepad_state(nl_gamepad_state_t* state) {
  uint32_t buttons = 0;
  int16_t ls_x;
  int16_t ls_y;
  int16_t rs_x;
  int16_t rs_y;
  uint8_t lt;
  uint8_t rt;

  if (state == NULL || state->controller == NULL) {
    return false;
  }

  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_A)) buttons |= NL_A_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_B)) buttons |= NL_B_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_X)) buttons |= NL_X_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_Y)) buttons |= NL_Y_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_BACK)) buttons |= NL_BACK_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_GUIDE)) buttons |= NL_SPECIAL_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_START)) buttons |= NL_PLAY_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_LEFTSTICK)) buttons |= NL_LS_CLK_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_RIGHTSTICK)) buttons |= NL_RS_CLK_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_LEFTSHOULDER)) buttons |= NL_LB_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_RIGHTSHOULDER)) buttons |= NL_RB_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_DPAD_UP)) buttons |= NL_UP_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_DPAD_DOWN)) buttons |= NL_DOWN_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_DPAD_LEFT)) buttons |= NL_LEFT_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_DPAD_RIGHT)) buttons |= NL_RIGHT_FLAG;
#if SDL_VERSION_ATLEAST(2, 0, 14)
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_MISC1)) buttons |= NL_MISC_FLAG;
#endif
#if SDL_VERSION_ATLEAST(2, 0, 14)
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_PADDLE1)) buttons |= NL_PADDLE1_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_PADDLE2)) buttons |= NL_PADDLE2_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_PADDLE3)) buttons |= NL_PADDLE3_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_PADDLE4)) buttons |= NL_PADDLE4_FLAG;
  if (SDL_GameControllerGetButton(state->controller, SDL_CONTROLLER_BUTTON_TOUCHPAD)) buttons |= NL_TOUCHPAD_FLAG;
#endif

  ls_x = SDL_GameControllerGetAxis(state->controller, SDL_CONTROLLER_AXIS_LEFTX);
  ls_y = (int16_t)-SDL_max(SDL_GameControllerGetAxis(state->controller, SDL_CONTROLLER_AXIS_LEFTY), (int16_t)-32767);
  rs_x = SDL_GameControllerGetAxis(state->controller, SDL_CONTROLLER_AXIS_RIGHTX);
  rs_y = (int16_t)-SDL_max(SDL_GameControllerGetAxis(state->controller, SDL_CONTROLLER_AXIS_RIGHTY), (int16_t)-32767);
  lt = (uint8_t)(((uint32_t)SDL_GameControllerGetAxis(state->controller, SDL_CONTROLLER_AXIS_TRIGGERLEFT)) * 255U / 32767U);
  rt = (uint8_t)(((uint32_t)SDL_GameControllerGetAxis(state->controller, SDL_CONTROLLER_AXIS_TRIGGERRIGHT)) * 255U / 32767U);

  if (state->buttons == buttons &&
      state->ls_x == ls_x &&
      state->ls_y == ls_y &&
      state->rs_x == rs_x &&
      state->rs_y == rs_y &&
      state->lt == lt &&
      state->rt == rt) {
    return false;
  }

  state->buttons = buttons;
  state->ls_x = ls_x;
  state->ls_y = ls_y;
  state->rs_x = rs_x;
  state->rs_y = rs_y;
  state->lt = lt;
  state->rt = rt;
  return true;
}

#if SDL_VERSION_ATLEAST(2, 0, 14)
static void nl_poll_sensor_state(nl_gamepad_state_t* state) {
  float data[3];
  uint32_t now = SDL_GetTicks();

  if (state == NULL || state->controller == NULL) {
    return;
  }

  if (state->accel_report_period_ms != 0 &&
      SDL_TICKS_PASSED(now, state->last_accel_event_time + state->accel_report_period_ms) &&
      SDL_GameControllerGetSensorData(state->controller, SDL_SENSOR_ACCEL, data, 3) == 0) {
    if (memcmp(data, state->last_accel_event_data, sizeof(data)) != 0) {
      memcpy(state->last_accel_event_data, data, sizeof(data));
      state->last_accel_event_time = now;
      (void)LiSendControllerMotionEvent(state->index, LI_MOTION_TYPE_ACCEL, data[0], data[1], data[2]);
    }
  }

  if (state->gyro_report_period_ms != 0 &&
      SDL_TICKS_PASSED(now, state->last_gyro_event_time + state->gyro_report_period_ms) &&
      SDL_GameControllerGetSensorData(state->controller, SDL_SENSOR_GYRO, data, 3) == 0) {
    if (memcmp(data, state->last_gyro_event_data, sizeof(data)) != 0) {
      memcpy(state->last_gyro_event_data, data, sizeof(data));
      state->last_gyro_event_time = now;
      (void)LiSendControllerMotionEvent(
          state->index,
          LI_MOTION_TYPE_GYRO,
          data[0] * 57.2957795f,
          data[1] * 57.2957795f,
          data[2] * 57.2957795f);
    }
  }
}
#endif

static void nl_attach_controller(nl_controller_manager_t* manager, int device_index) {
  SDL_GameController* controller;
  SDL_Joystick* joystick;
  nl_gamepad_state_t* state;
  const char* name;
  char guid[33];
  uint16_t vendor;
  uint16_t product;
  int slot;

  if (manager == NULL || manager->runtime == NULL) {
    return;
  }

  controller = SDL_GameControllerOpen(device_index);
  if (controller == NULL) {
    SDL_LogWarn(SDL_LOG_CATEGORY_APPLICATION,
                "[noland-controller] SDL_GameControllerOpen failed index=%d error=%s",
                device_index,
                SDL_GetError());
    return;
  }

  joystick = SDL_GameControllerGetJoystick(controller);
  if (joystick == NULL) {
    SDL_GameControllerClose(controller);
    return;
  }

  if (nl_find_gamepad_by_instance_id(manager, SDL_JoystickInstanceID(joystick)) != NULL) {
    SDL_GameControllerClose(controller);
    return;
  }

  slot = nl_next_free_slot(manager);
  if (slot < 0) {
    SDL_GameControllerClose(controller);
    return;
  }

  name = SDL_GameControllerName(controller);
  vendor = SDL_GameControllerGetVendor(controller);
  product = SDL_GameControllerGetProduct(controller);
  SDL_JoystickGetGUIDString(SDL_JoystickGetGUID(joystick), guid, sizeof(guid));

  state = &manager->gamepads[slot];
  memset(state, 0, sizeof(*state));
  state->controller = controller;
  state->joystick_id = SDL_JoystickInstanceID(joystick);
  state->index = (uint8_t)slot;
  state->supported_button_flags = nl_supported_button_flags(controller);
  state->capabilities = nl_controller_capabilities(controller);
  manager->gamepad_mask |= (uint16_t)(1U << state->index);

#if SDL_VERSION_ATLEAST(2, 0, 12)
  SDL_GameControllerSetPlayerIndex(controller, state->index);
#endif

  {
    nl_result_t result;
    uint8_t controller_type = nl_controller_type(controller);
    SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION,
                "[noland-controller] attached index=%d slot=%u name=%s vid=0x%04x pid=0x%04x guid=%s type=%u caps=0x%04x buttons=0x%08x mask=0x%04x",
                device_index,
                state->index,
                name != NULL ? name : "<unknown>",
                vendor,
                product,
                guid,
                controller_type,
                state->capabilities,
                state->supported_button_flags,
                manager->gamepad_mask);
    result = nl_send_controller_arrival(
        manager->runtime,
        state->index,
        manager->gamepad_mask,
        controller_type,
        state->supported_button_flags,
        state->capabilities);
    if (result != NL_RESULT_OK) {
      SDL_LogWarn(SDL_LOG_CATEGORY_APPLICATION,
                  "[noland-controller] arrival send failed slot=%u result=%s",
                  state->index,
                  nl_result_name(result));
    } else {
      SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION,
                  "[noland-controller] arrival send ok slot=%u mask=0x%04x",
                  state->index,
                  manager->gamepad_mask);
    }
  }

  (void)nl_refresh_gamepad_state(state);
  nl_send_gamepad_state(manager, state);

  if ((state->capabilities & LI_CCAP_BATTERY_STATE) != 0) {
    nl_send_battery_state(manager, state, SDL_JoystickCurrentPowerLevel(joystick));
  }
}

static void nl_detach_controller(nl_controller_manager_t* manager, nl_gamepad_state_t* state) {
  if (manager == NULL || state == NULL) {
    return;
  }

  SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION,
              "[noland-controller] detached slot=%u joystick_id=%d mask_before=0x%04x",
              state->index,
              (int)state->joystick_id,
              manager->gamepad_mask);
  if (state->controller != NULL) {
    SDL_GameControllerClose(state->controller);
  }
  manager->gamepad_mask &= (uint16_t)~(1U << state->index);
  if (manager->runtime != NULL) {
    (void)nl_send_controller_state(manager->runtime, state->index, (int16_t)manager->gamepad_mask, 0, 0, 0, 0, 0, 0, 0);
  }
  memset(state, 0, sizeof(*state));
}

static void nl_sync_controllers(nl_controller_manager_t* manager) {
  int i;

  SDL_GameControllerUpdate();

  for (i = 0; i < SDL_NumJoysticks(); i++) {
    if (SDL_IsGameController(i)) {
      nl_attach_controller(manager, i);
    }
  }

  for (i = 0; i < NL_MAX_GAMEPADS; i++) {
    nl_gamepad_state_t* state = &manager->gamepads[i];
    if (state->controller == NULL) {
      continue;
    }

    if (!SDL_GameControllerGetAttached(state->controller)) {
      nl_detach_controller(manager, state);
      continue;
    }

    if (nl_refresh_gamepad_state(state)) {
      nl_send_gamepad_state(manager, state);
    }

#if SDL_VERSION_ATLEAST(2, 0, 14)
    nl_poll_sensor_state(state);
#endif
  }
}

static void* nl_controller_thread_main(void* context) {
  nl_controller_manager_t* manager = (nl_controller_manager_t*)context;

  SDL_SetHint(SDL_HINT_JOYSTICK_ALLOW_BACKGROUND_EVENTS, "1");
  SDL_SetHint(SDL_HINT_JOYSTICK_HIDAPI_PS4_RUMBLE, "1");
  SDL_SetHint(SDL_HINT_JOYSTICK_HIDAPI_PS5_RUMBLE, "1");

  SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION, "[noland-controller] thread starting");

  if (SDL_InitSubSystem(SDL_INIT_JOYSTICK) != 0) {
    SDL_LogError(SDL_LOG_CATEGORY_APPLICATION,
                 "[noland-controller] SDL_InitSubSystem(SDL_INIT_JOYSTICK) failed: %s",
                 SDL_GetError());
    return NULL;
  }
  if (SDL_InitSubSystem(SDL_INIT_GAMECONTROLLER) != 0) {
    SDL_LogError(SDL_LOG_CATEGORY_APPLICATION,
                 "[noland-controller] SDL_InitSubSystem(SDL_INIT_GAMECONTROLLER) failed: %s",
                 SDL_GetError());
    SDL_QuitSubSystem(SDL_INIT_JOYSTICK);
    return NULL;
  }

  nl_controller_manager_lock(manager);
  manager->initialized = true;
  manager->gamepad_mask = 0;
  nl_controller_manager_unlock(manager);

  SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION,
              "[noland-controller] SDL initialized joysticks=%d",
              SDL_NumJoysticks());
  for (int i = 0; i < SDL_NumJoysticks(); i++) {
    const char* joy_name = SDL_JoystickNameForIndex(i);
    SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION,
                "[noland-controller] joystick index=%d is_game_controller=%d name=%s",
                i,
                SDL_IsGameController(i),
                joy_name != NULL ? joy_name : "<unknown>");
  }

  while (1) {
    bool running;

    nl_controller_manager_lock(manager);
    running = manager->running;
    if (running) {
      nl_sync_controllers(manager);
    }
    nl_controller_manager_unlock(manager);

    if (!running) {
      break;
    }

    SDL_Delay(4);
  }

  nl_controller_manager_lock(manager);
  for (int i = 0; i < NL_MAX_GAMEPADS; i++) {
    if (manager->gamepads[i].controller != NULL) {
      SDL_GameControllerClose(manager->gamepads[i].controller);
      memset(&manager->gamepads[i], 0, sizeof(manager->gamepads[i]));
    }
  }
  manager->gamepad_mask = 0;
  manager->initialized = false;
  nl_controller_manager_unlock(manager);

  SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION, "[noland-controller] thread stopping");
  SDL_QuitSubSystem(SDL_INIT_GAMECONTROLLER);
  SDL_QuitSubSystem(SDL_INIT_JOYSTICK);
  return NULL;
}

nl_controller_manager_t* nl_controller_manager_create(void) {
  nl_controller_manager_t* manager = (nl_controller_manager_t*)calloc(1, sizeof(*manager));
  if (manager == NULL) {
    return NULL;
  }
  pthread_mutex_init(&manager->mutex, NULL);
  return manager;
}

void nl_controller_manager_destroy(nl_controller_manager_t* manager) {
  if (manager == NULL) {
    return;
  }
  nl_controller_manager_stop(manager);
  pthread_mutex_destroy(&manager->mutex);
  free(manager);
}

bool nl_controller_manager_start(nl_controller_manager_t* manager, nl_runtime_t* runtime) {
  if (manager == NULL) {
    return false;
  }

  nl_controller_manager_lock(manager);
  if (manager->thread_started) {
    manager->runtime = runtime;
    manager->running = true;
    nl_controller_manager_unlock(manager);
    return true;
  }

  manager->runtime = runtime;
  manager->running = true;
  if (pthread_create(&manager->thread, NULL, nl_controller_thread_main, manager) != 0) {
    manager->running = false;
    manager->runtime = NULL;
    nl_controller_manager_unlock(manager);
    return false;
  }

  manager->thread_started = true;
  nl_controller_manager_unlock(manager);
  return true;
}

void nl_controller_manager_stop(nl_controller_manager_t* manager) {
  bool thread_started;

  if (manager == NULL) {
    return;
  }

  nl_controller_manager_lock(manager);
  thread_started = manager->thread_started;
  manager->running = false;
  nl_controller_manager_unlock(manager);

  if (thread_started) {
    pthread_join(manager->thread, NULL);
    nl_controller_manager_lock(manager);
    manager->thread_started = false;
    manager->runtime = NULL;
    nl_controller_manager_unlock(manager);
  }
}

void nl_controller_manager_rumble(nl_controller_manager_t* manager, uint16_t controller_number, uint16_t low_freq_motor, uint16_t high_freq_motor) {
  nl_gamepad_state_t* state;

  if (manager == NULL || controller_number >= NL_MAX_GAMEPADS) {
    return;
  }

  nl_controller_manager_lock(manager);
  state = &manager->gamepads[controller_number];
#if SDL_VERSION_ATLEAST(2, 0, 9)
  if (state->controller != NULL) {
    SDL_GameControllerRumble(state->controller, low_freq_motor, high_freq_motor, 30000);
  }
#else
  (void)low_freq_motor;
  (void)high_freq_motor;
#endif
  nl_controller_manager_unlock(manager);
}

void nl_controller_manager_rumble_triggers(nl_controller_manager_t* manager, uint16_t controller_number, uint16_t left_trigger, uint16_t right_trigger) {
  nl_gamepad_state_t* state;

  if (manager == NULL || controller_number >= NL_MAX_GAMEPADS) {
    return;
  }

  nl_controller_manager_lock(manager);
  state = &manager->gamepads[controller_number];
#if SDL_VERSION_ATLEAST(2, 0, 14)
  if (state->controller != NULL) {
    SDL_GameControllerRumbleTriggers(state->controller, left_trigger, right_trigger, 30000);
  }
#else
  (void)left_trigger;
  (void)right_trigger;
#endif
  nl_controller_manager_unlock(manager);
}

void nl_controller_manager_set_motion_event_state(nl_controller_manager_t* manager, uint16_t controller_number, uint8_t motion_type, uint16_t report_rate_hz) {
  nl_gamepad_state_t* state;

  if (manager == NULL || controller_number >= NL_MAX_GAMEPADS) {
    return;
  }

  nl_controller_manager_lock(manager);
  state = &manager->gamepads[controller_number];
#if SDL_VERSION_ATLEAST(2, 0, 14)
  if (state->controller != NULL) {
    uint8_t report_period_ms = report_rate_hz ? (uint8_t)(1000U / report_rate_hz) : 0U;
    switch (motion_type) {
      case LI_MOTION_TYPE_ACCEL:
        state->accel_report_period_ms = report_period_ms;
        SDL_GameControllerSetSensorEnabled(state->controller, SDL_SENSOR_ACCEL, report_rate_hz ? SDL_TRUE : SDL_FALSE);
        break;
      case LI_MOTION_TYPE_GYRO:
        state->gyro_report_period_ms = report_period_ms;
        SDL_GameControllerSetSensorEnabled(state->controller, SDL_SENSOR_GYRO, report_rate_hz ? SDL_TRUE : SDL_FALSE);
        break;
      default:
        break;
    }
  }
#else
  (void)motion_type;
  (void)report_rate_hz;
#endif
  nl_controller_manager_unlock(manager);
}

void nl_controller_manager_set_led(nl_controller_manager_t* manager, uint16_t controller_number, uint8_t r, uint8_t g, uint8_t b) {
  nl_gamepad_state_t* state;

  if (manager == NULL || controller_number >= NL_MAX_GAMEPADS) {
    return;
  }

  nl_controller_manager_lock(manager);
  state = &manager->gamepads[controller_number];
#if SDL_VERSION_ATLEAST(2, 0, 14)
  if (state->controller != NULL) {
    SDL_GameControllerSetLED(state->controller, r, g, b);
  }
#else
  (void)r;
  (void)g;
  (void)b;
#endif
  nl_controller_manager_unlock(manager);
}

void nl_controller_manager_set_adaptive_triggers(nl_controller_manager_t* manager, uint16_t controller_number, const nl_dualsense_output_report_t* report) {
  nl_gamepad_state_t* state;

  if (manager == NULL || controller_number >= NL_MAX_GAMEPADS || report == NULL) {
    return;
  }

  nl_controller_manager_lock(manager);
  state = &manager->gamepads[controller_number];
#if SDL_VERSION_ATLEAST(2, 0, 16)
  if (state->controller != NULL && SDL_GameControllerGetType(state->controller) == SDL_CONTROLLER_TYPE_PS5) {
    SDL_GameControllerSendEffect(state->controller, report, sizeof(*report));
  }
#endif
  nl_controller_manager_unlock(manager);
}

#else

struct nl_controller_manager {
  int unused;
};

nl_controller_manager_t* nl_controller_manager_create(void) {
  return (nl_controller_manager_t*)calloc(1, sizeof(nl_controller_manager_t));
}

void nl_controller_manager_destroy(nl_controller_manager_t* manager) {
  free(manager);
}

bool nl_controller_manager_start(nl_controller_manager_t* manager, nl_runtime_t* runtime) {
  (void)manager;
  (void)runtime;
  return true;
}

void nl_controller_manager_stop(nl_controller_manager_t* manager) {
  (void)manager;
}

void nl_controller_manager_rumble(nl_controller_manager_t* manager, uint16_t controller_number, uint16_t low_freq_motor, uint16_t high_freq_motor) {
  (void)manager;
  (void)controller_number;
  (void)low_freq_motor;
  (void)high_freq_motor;
}

void nl_controller_manager_rumble_triggers(nl_controller_manager_t* manager, uint16_t controller_number, uint16_t left_trigger, uint16_t right_trigger) {
  (void)manager;
  (void)controller_number;
  (void)left_trigger;
  (void)right_trigger;
}

void nl_controller_manager_set_motion_event_state(nl_controller_manager_t* manager, uint16_t controller_number, uint8_t motion_type, uint16_t report_rate_hz) {
  (void)manager;
  (void)controller_number;
  (void)motion_type;
  (void)report_rate_hz;
}

void nl_controller_manager_set_led(nl_controller_manager_t* manager, uint16_t controller_number, uint8_t r, uint8_t g, uint8_t b) {
  (void)manager;
  (void)controller_number;
  (void)r;
  (void)g;
  (void)b;
}

void nl_controller_manager_set_adaptive_triggers(nl_controller_manager_t* manager, uint16_t controller_number, const nl_dualsense_output_report_t* report) {
  (void)manager;
  (void)controller_number;
  (void)report;
}

#endif
