#include "noland_moonlight.h"

#include <SDL.h>

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#include <windows.h>
#endif

#if !defined(__APPLE__)

#define NL_CAPTURE_NONE 0
#define NL_CAPTURE_RELATIVE 1
#define NL_CAPTURE_ABSOLUTE 2

#define NL_MOD_SHIFT 0x01
#define NL_MOD_CTRL 0x02
#define NL_MOD_ALT 0x04
#define NL_MOD_META 0x08

extern void noland_desktop_input_on_relative_mouse(double delta_x, double delta_y);
extern void noland_desktop_input_on_absolute_mouse(double x,
                                                   double y,
                                                   double content_width,
                                                   double content_height);
extern void noland_desktop_input_on_mouse_button(uint8_t button, bool pressed);
extern void noland_desktop_input_on_keyboard(uint16_t virtual_key,
                                             bool pressed,
                                             uint8_t modifiers);
extern void noland_desktop_input_on_vertical_scroll(double amount,
                                                    bool high_resolution);
extern void noland_desktop_input_on_horizontal_scroll(double amount,
                                                      bool high_resolution);
extern void noland_desktop_input_on_focus_changed(bool focused);
extern void noland_desktop_input_on_capture_changed(bool active, int mode);
extern int noland_desktop_input_request_capture(void);
extern void noland_desktop_input_debug_native_event(int kind);

typedef struct nl_desktop_input_context {
  SDL_Window* window;
  SDL_Thread* thread;
  SDL_mutex* mutex;
  uint32_t window_id;
  bool running;
  bool capture_active;
  int capture_mode;
  bool initialized_video;
  bool suppress_next_left_up;
#if defined(_WIN32)
  HWND native_window;
  WNDPROC previous_window_proc;
  volatile LONG cursor_hidden;
#endif
} nl_desktop_input_context_t;

static nl_desktop_input_context_t g_input;

#if defined(_WIN32)
static LRESULT CALLBACK nl_stream_window_proc(HWND window,
                                              UINT message,
                                              WPARAM w_param,
                                              LPARAM l_param) {
  nl_desktop_input_context_t* context = &g_input;
  if (message == WM_SETCURSOR && LOWORD(l_param) == HTCLIENT &&
      InterlockedCompareExchange(&context->cursor_hidden, 0, 0) != 0) {
    SetCursor(NULL);
    return TRUE;
  }

  if (context->previous_window_proc != NULL) {
    return CallWindowProcW(context->previous_window_proc,
                           window,
                           message,
                           w_param,
                           l_param);
  }
  return DefWindowProcW(window, message, w_param, l_param);
}

static bool nl_install_windows_cursor_guard(nl_desktop_input_context_t* context,
                                            void* window_handle) {
  LONG_PTR previous;
  if (context == NULL || window_handle == NULL) return false;

  context->native_window = (HWND)window_handle;
  SetLastError(0);
  previous = SetWindowLongPtrW(context->native_window,
                               GWLP_WNDPROC,
                               (LONG_PTR)nl_stream_window_proc);
  if (previous == 0 && GetLastError() != 0) {
    context->native_window = NULL;
    return false;
  }
  context->previous_window_proc = (WNDPROC)previous;
  return true;
}

static void nl_uninstall_windows_cursor_guard(nl_desktop_input_context_t* context) {
  if (context == NULL) return;
  InterlockedExchange(&context->cursor_hidden, 0);
  if (context->native_window != NULL && context->previous_window_proc != NULL &&
      IsWindow(context->native_window)) {
    SetWindowLongPtrW(context->native_window,
                      GWLP_WNDPROC,
                      (LONG_PTR)context->previous_window_proc);
  }
  context->previous_window_proc = NULL;
  context->native_window = NULL;
  SetCursor(LoadCursorW(NULL, IDC_ARROW));
}
#endif

static uint8_t nl_modifier_bits(SDL_Keymod modifiers) {
  uint8_t bits = 0;
  if ((modifiers & KMOD_SHIFT) != 0) bits |= NL_MOD_SHIFT;
  if ((modifiers & KMOD_CTRL) != 0) bits |= NL_MOD_CTRL;
  if ((modifiers & KMOD_ALT) != 0) bits |= NL_MOD_ALT;
  if ((modifiers & KMOD_GUI) != 0) bits |= NL_MOD_META;
  return bits;
}

static uint8_t nl_mouse_button(uint8_t button) {
  switch (button) {
    case SDL_BUTTON_LEFT: return 0x01;
    case SDL_BUTTON_MIDDLE: return 0x02;
    case SDL_BUTTON_RIGHT: return 0x03;
    case SDL_BUTTON_X1: return 0x04;
    case SDL_BUTTON_X2: return 0x05;
    default: return 0;
  }
}

static uint16_t nl_virtual_key(SDL_Scancode scan, SDL_Keycode key) {
  if (key >= SDLK_a && key <= SDLK_z) return (uint16_t)('A' + (key - SDLK_a));
  if (key >= SDLK_0 && key <= SDLK_9) return (uint16_t)key;
  if (key >= SDLK_F1 && key <= SDLK_F12) return (uint16_t)(0x70 + (key - SDLK_F1));
  if (key >= SDLK_KP_0 && key <= SDLK_KP_9) return (uint16_t)(0x60 + (key - SDLK_KP_0));

  switch (scan) {
    case SDL_SCANCODE_RETURN:
    case SDL_SCANCODE_KP_ENTER: return 0x0D;
    case SDL_SCANCODE_TAB: return 0x09;
    case SDL_SCANCODE_SPACE: return 0x20;
    case SDL_SCANCODE_BACKSPACE: return 0x08;
    case SDL_SCANCODE_ESCAPE: return 0x1B;
    case SDL_SCANCODE_LGUI: return 0x5B;
    case SDL_SCANCODE_RGUI: return 0x5C;
    case SDL_SCANCODE_LSHIFT:
    case SDL_SCANCODE_RSHIFT: return 0x10;
    case SDL_SCANCODE_LCTRL:
    case SDL_SCANCODE_RCTRL: return 0x11;
    case SDL_SCANCODE_LALT:
    case SDL_SCANCODE_RALT: return 0x12;
    case SDL_SCANCODE_CAPSLOCK: return 0x14;
    case SDL_SCANCODE_INSERT: return 0x2D;
    case SDL_SCANCODE_DELETE: return 0x2E;
    case SDL_SCANCODE_HOME: return 0x24;
    case SDL_SCANCODE_END: return 0x23;
    case SDL_SCANCODE_PAGEUP: return 0x21;
    case SDL_SCANCODE_PAGEDOWN: return 0x22;
    case SDL_SCANCODE_LEFT: return 0x25;
    case SDL_SCANCODE_UP: return 0x26;
    case SDL_SCANCODE_RIGHT: return 0x27;
    case SDL_SCANCODE_DOWN: return 0x28;
    case SDL_SCANCODE_MINUS: return 0xBD;
    case SDL_SCANCODE_EQUALS: return 0xBB;
    case SDL_SCANCODE_LEFTBRACKET: return 0xDB;
    case SDL_SCANCODE_RIGHTBRACKET: return 0xDD;
    case SDL_SCANCODE_BACKSLASH: return 0xDC;
    case SDL_SCANCODE_SEMICOLON: return 0xBA;
    case SDL_SCANCODE_APOSTROPHE: return 0xDE;
    case SDL_SCANCODE_GRAVE: return 0xC0;
    case SDL_SCANCODE_COMMA: return 0xBC;
    case SDL_SCANCODE_PERIOD: return 0xBE;
    case SDL_SCANCODE_SLASH: return 0xBF;
    case SDL_SCANCODE_KP_DECIMAL: return 0x6E;
    case SDL_SCANCODE_KP_MULTIPLY: return 0x6A;
    case SDL_SCANCODE_KP_PLUS: return 0x6B;
    case SDL_SCANCODE_NUMLOCKCLEAR: return 0x90;
    case SDL_SCANCODE_KP_DIVIDE: return 0x6F;
    case SDL_SCANCODE_KP_MINUS: return 0x6D;
    default: return 0;
  }
}

static bool nl_release_shortcut(const SDL_KeyboardEvent* event) {
  SDL_Keymod required = (SDL_Keymod)(KMOD_CTRL | KMOD_ALT | KMOD_SHIFT);
  if (event == NULL || event->type != SDL_KEYDOWN ||
      (event->keysym.mod & required) != required) {
    return false;
  }
  return event->keysym.scancode == SDL_SCANCODE_Z ||
         event->keysym.scancode == SDL_SCANCODE_Q;
}

static void nl_apply_capture_locked(nl_desktop_input_context_t* context,
                                    bool active,
                                    int mode) {
  int target_mode = active ? mode : NL_CAPTURE_NONE;
  if (context == NULL || context->window == NULL) return;
  if (context->capture_active == active && context->capture_mode == target_mode) return;

  context->capture_active = active;
  context->capture_mode = target_mode;
#if defined(_WIN32)
  InterlockedExchange(&context->cursor_hidden, active ? 1 : 0);
#endif
  SDL_SetWindowGrab(context->window, active ? SDL_TRUE : SDL_FALSE);
  SDL_CaptureMouse(active ? SDL_TRUE : SDL_FALSE);
  SDL_SetRelativeMouseMode(active && mode == NL_CAPTURE_RELATIVE ? SDL_TRUE : SDL_FALSE);
  SDL_ShowCursor(active ? SDL_DISABLE : SDL_ENABLE);
#if defined(_WIN32)
  if (active) {
    SetCursor(NULL);
  } else {
    SetCursor(LoadCursorW(NULL, IDC_ARROW));
  }
#endif
  noland_desktop_input_on_capture_changed(active, target_mode);
}

static bool nl_event_targets_window(const SDL_Event* event, uint32_t window_id) {
  if (event == NULL) return false;
  switch (event->type) {
    case SDL_WINDOWEVENT: return event->window.windowID == window_id;
    case SDL_KEYDOWN:
    case SDL_KEYUP: return event->key.windowID == window_id;
    case SDL_MOUSEMOTION: return event->motion.windowID == window_id;
    case SDL_MOUSEBUTTONDOWN:
    case SDL_MOUSEBUTTONUP: return event->button.windowID == window_id;
    case SDL_MOUSEWHEEL: return event->wheel.windowID == window_id;
    default: return false;
  }
}

static void nl_handle_event(nl_desktop_input_context_t* context, const SDL_Event* event) {
  bool active;
  int mode;
  if (context == NULL || event == NULL) return;

  SDL_LockMutex(context->mutex);
  active = context->capture_active;
  mode = context->capture_mode;

  switch (event->type) {
    case SDL_WINDOWEVENT:
      if (event->window.event == SDL_WINDOWEVENT_FOCUS_GAINED) {
        noland_desktop_input_on_focus_changed(true);
      } else if (event->window.event == SDL_WINDOWEVENT_FOCUS_LOST) {
        noland_desktop_input_on_focus_changed(false);
        nl_apply_capture_locked(context, false, NL_CAPTURE_NONE);
      }
      break;
    case SDL_MOUSEMOTION:
      noland_desktop_input_debug_native_event(1);
      if (active && mode == NL_CAPTURE_RELATIVE) {
        noland_desktop_input_on_relative_mouse((double)event->motion.xrel,
                                               (double)event->motion.yrel);
      } else if (active && mode == NL_CAPTURE_ABSOLUTE) {
        int width = 0;
        int height = 0;
        SDL_GetWindowSize(context->window, &width, &height);
        if (width > 0 && height > 0) {
          noland_desktop_input_on_absolute_mouse((double)event->motion.x,
                                                 (double)event->motion.y,
                                                 (double)width,
                                                 (double)height);
        }
      }
      break;
    case SDL_MOUSEBUTTONDOWN: {
      uint8_t button;
      noland_desktop_input_debug_native_event(2);
      button = nl_mouse_button(event->button.button);
      if (!active && event->button.button == SDL_BUTTON_LEFT) {
        int requested_mode = noland_desktop_input_request_capture();
        if (requested_mode != NL_CAPTURE_NONE) {
          context->suppress_next_left_up = true;
          nl_apply_capture_locked(context, true, requested_mode);
        }
      } else if (active && button != 0) {
        noland_desktop_input_on_mouse_button(button, true);
      }
      break;
    }
    case SDL_MOUSEBUTTONUP: {
      uint8_t button;
      noland_desktop_input_debug_native_event(3);
      button = nl_mouse_button(event->button.button);
      if (event->button.button == SDL_BUTTON_LEFT && context->suppress_next_left_up) {
        context->suppress_next_left_up = false;
      } else if (active && button != 0) {
        noland_desktop_input_on_mouse_button(button, false);
      }
      break;
    }
    case SDL_MOUSEWHEEL:
      if (active) {
        bool high_resolution = event->wheel.preciseX != (float)event->wheel.x ||
                               event->wheel.preciseY != (float)event->wheel.y;
        double x = high_resolution ? event->wheel.preciseX * 120.0 : (double)event->wheel.x;
        double y = high_resolution ? event->wheel.preciseY * 120.0 : (double)event->wheel.y;
        if (event->wheel.direction == SDL_MOUSEWHEEL_FLIPPED) {
          x = -x;
          y = -y;
        }
        if (y != 0.0) noland_desktop_input_on_vertical_scroll(y, high_resolution);
        if (x != 0.0) noland_desktop_input_on_horizontal_scroll(x, high_resolution);
      }
      break;
    case SDL_KEYDOWN:
    case SDL_KEYUP:
      noland_desktop_input_debug_native_event(4);
      if (active && event->key.repeat == 0) {
        if (nl_release_shortcut(&event->key)) {
          nl_apply_capture_locked(context, false, NL_CAPTURE_NONE);
        } else {
          uint16_t key = nl_virtual_key(event->key.keysym.scancode,
                                        event->key.keysym.sym);
          if (key != 0) {
            noland_desktop_input_on_keyboard(
                key,
                event->type == SDL_KEYDOWN,
                nl_modifier_bits((SDL_Keymod)event->key.keysym.mod));
          }
        }
      }
      break;
    default:
      break;
  }

  SDL_UnlockMutex(context->mutex);
}

static int SDLCALL nl_input_thread(void* data) {
  nl_desktop_input_context_t* context = (nl_desktop_input_context_t*)data;
  while (context != NULL) {
    SDL_Event event;
    bool running;

    SDL_LockMutex(context->mutex);
    running = context->running;
    SDL_UnlockMutex(context->mutex);
    if (!running) break;

    if (SDL_WaitEventTimeout(&event, 8) == SDL_TRUE &&
        nl_event_targets_window(&event, context->window_id)) {
      nl_handle_event(context, &event);
    }
  }
  return 0;
}

int nl_desktop_input_install(const nl_surface_descriptor_t* surface) {
  nl_desktop_input_context_t* context = &g_input;
  if (surface == NULL || surface->window_handle == NULL) return -1;
  if (surface->surface_type != NL_SURFACE_WINDOWS_HWND &&
      surface->surface_type != NL_SURFACE_X11_WINDOW &&
      surface->surface_type != NL_SURFACE_WAYLAND_SURFACE) {
    return -2;
  }
  if (context->window != NULL) return 0;

  memset(context, 0, sizeof(*context));
  context->mutex = SDL_CreateMutex();
  if (context->mutex == NULL) return -3;

  SDL_SetHint(SDL_HINT_MOUSE_RELATIVE_MODE_WARP, "0");
  SDL_SetHint(SDL_HINT_MOUSE_RELATIVE_SCALING, "0");
  SDL_SetHint(SDL_HINT_JOYSTICK_ALLOW_BACKGROUND_EVENTS, "1");
  if ((SDL_WasInit(SDL_INIT_VIDEO) & SDL_INIT_VIDEO) == 0) {
    if (SDL_InitSubSystem(SDL_INIT_VIDEO) != 0) {
      SDL_DestroyMutex(context->mutex);
      memset(context, 0, sizeof(*context));
      return -4;
    }
    context->initialized_video = true;
  }

  context->window = SDL_CreateWindowFrom(surface->window_handle);
  if (context->window == NULL) {
    if (context->initialized_video) SDL_QuitSubSystem(SDL_INIT_VIDEO);
    SDL_DestroyMutex(context->mutex);
    memset(context, 0, sizeof(*context));
    return -5;
  }

#if defined(_WIN32)
  if (!nl_install_windows_cursor_guard(context, surface->window_handle)) {
    SDL_DestroyWindow(context->window);
    if (context->initialized_video) SDL_QuitSubSystem(SDL_INIT_VIDEO);
    SDL_DestroyMutex(context->mutex);
    memset(context, 0, sizeof(*context));
    return -6;
  }
#endif

  context->window_id = SDL_GetWindowID(context->window);
  context->running = true;
  context->thread = SDL_CreateThread(nl_input_thread, "noland-input", context);
  if (context->thread == NULL) {
#if defined(_WIN32)
    nl_uninstall_windows_cursor_guard(context);
#endif
    SDL_DestroyWindow(context->window);
    if (context->initialized_video) SDL_QuitSubSystem(SDL_INIT_VIDEO);
    SDL_DestroyMutex(context->mutex);
    memset(context, 0, sizeof(*context));
    return -6;
  }

  return 0;
}

void nl_desktop_input_uninstall(void) {
  nl_desktop_input_context_t* context = &g_input;
  if (context->mutex == NULL) return;

  SDL_LockMutex(context->mutex);
  nl_apply_capture_locked(context, false, NL_CAPTURE_NONE);
  context->running = false;
  SDL_UnlockMutex(context->mutex);
  {
    SDL_Event wake_event;
    SDL_zero(wake_event);
    wake_event.type = SDL_USEREVENT;
    SDL_PushEvent(&wake_event);
  }

  if (context->thread != NULL) {
    SDL_WaitThread(context->thread, NULL);
    context->thread = NULL;
  }
#if defined(_WIN32)
  nl_uninstall_windows_cursor_guard(context);
#endif
  if (context->window != NULL) {
    SDL_DestroyWindow(context->window);
    context->window = NULL;
  }
  if (context->initialized_video) SDL_QuitSubSystem(SDL_INIT_VIDEO);
  SDL_DestroyMutex(context->mutex);
  memset(context, 0, sizeof(*context));
}

int nl_desktop_input_set_capture_active(bool active, int mode) {
  nl_desktop_input_context_t* context = &g_input;
  if (context->mutex == NULL || context->window == NULL) return 1;
  if (active && mode != NL_CAPTURE_RELATIVE && mode != NL_CAPTURE_ABSOLUTE) return -1;

  SDL_LockMutex(context->mutex);
  nl_apply_capture_locked(context, active, mode);
  SDL_UnlockMutex(context->mutex);
  return 0;
}

#else

int nl_desktop_input_install(const nl_surface_descriptor_t* surface) {
  (void)surface;
  return -1;
}

void nl_desktop_input_uninstall(void) {}

int nl_desktop_input_set_capture_active(bool active, int mode) {
  (void)active;
  (void)mode;
  return -1;
}

#endif
