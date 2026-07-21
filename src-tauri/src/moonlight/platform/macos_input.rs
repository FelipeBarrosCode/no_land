use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        Arc, OnceLock, Weak,
    },
};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tauri::{Runtime, Window};

use crate::{
    input::{
        event::{ButtonState, MouseButton},
        manager::InputManager,
        state::MouseMode,
    },
    moonlight::domain::MoonlightError,
};

static INPUT_MANAGER: OnceLock<Weak<InputManager>> = OnceLock::new();
static DEBUG_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static DEBUG_CAPTURE_MODE: AtomicI32 = AtomicI32::new(0);
static DEBUG_CAPTURE_REQUESTS: AtomicU64 = AtomicU64::new(0);
static DEBUG_NATIVE_MOUSE_MOVES: AtomicU64 = AtomicU64::new(0);
static DEBUG_NATIVE_MOUSE_DOWNS: AtomicU64 = AtomicU64::new(0);
static DEBUG_NATIVE_MOUSE_UPS: AtomicU64 = AtomicU64::new(0);
static DEBUG_NATIVE_KEYS: AtomicU64 = AtomicU64::new(0);
static DEBUG_RUST_RELATIVE_CALLBACKS: AtomicU64 = AtomicU64::new(0);
static DEBUG_RUST_ABSOLUTE_CALLBACKS: AtomicU64 = AtomicU64::new(0);
static DEBUG_RUST_BUTTON_CALLBACKS: AtomicU64 = AtomicU64::new(0);
static DEBUG_RUST_KEY_CALLBACKS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacosInputDebugSnapshot {
    pub capture_active: bool,
    pub capture_mode: i32,
    pub capture_requests: u64,
    pub native_mouse_moves: u64,
    pub native_mouse_downs: u64,
    pub native_mouse_ups: u64,
    pub native_keys: u64,
    pub rust_relative_callbacks: u64,
    pub rust_absolute_callbacks: u64,
    pub rust_button_callbacks: u64,
    pub rust_key_callbacks: u64,
}

pub fn macos_input_debug_snapshot() -> MacosInputDebugSnapshot {
    MacosInputDebugSnapshot {
        capture_active: DEBUG_CAPTURE_ACTIVE.load(Ordering::Relaxed),
        capture_mode: DEBUG_CAPTURE_MODE.load(Ordering::Relaxed),
        capture_requests: DEBUG_CAPTURE_REQUESTS.load(Ordering::Relaxed),
        native_mouse_moves: DEBUG_NATIVE_MOUSE_MOVES.load(Ordering::Relaxed),
        native_mouse_downs: DEBUG_NATIVE_MOUSE_DOWNS.load(Ordering::Relaxed),
        native_mouse_ups: DEBUG_NATIVE_MOUSE_UPS.load(Ordering::Relaxed),
        native_keys: DEBUG_NATIVE_KEYS.load(Ordering::Relaxed),
        rust_relative_callbacks: DEBUG_RUST_RELATIVE_CALLBACKS.load(Ordering::Relaxed),
        rust_absolute_callbacks: DEBUG_RUST_ABSOLUTE_CALLBACKS.load(Ordering::Relaxed),
        rust_button_callbacks: DEBUG_RUST_BUTTON_CALLBACKS.load(Ordering::Relaxed),
        rust_key_callbacks: DEBUG_RUST_KEY_CALLBACKS.load(Ordering::Relaxed),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_capture_active() -> bool {
    DEBUG_CAPTURE_ACTIVE.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_capture_mode() -> i32 {
    DEBUG_CAPTURE_MODE.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_capture_requests() -> u64 {
    DEBUG_CAPTURE_REQUESTS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_mouse_moves() -> u64 {
    DEBUG_NATIVE_MOUSE_MOVES.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_mouse_downs() -> u64 {
    DEBUG_NATIVE_MOUSE_DOWNS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_mouse_ups() -> u64 {
    DEBUG_NATIVE_MOUSE_UPS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_keys() -> u64 {
    DEBUG_NATIVE_KEYS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_rust_relative_callbacks() -> u64 {
    DEBUG_RUST_RELATIVE_CALLBACKS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_rust_absolute_callbacks() -> u64 {
    DEBUG_RUST_ABSOLUTE_CALLBACKS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_rust_button_callbacks() -> u64 {
    DEBUG_RUST_BUTTON_CALLBACKS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_rust_key_callbacks() -> u64 {
    DEBUG_RUST_KEY_CALLBACKS.load(Ordering::Relaxed)
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn noland_macos_resolve_stream_target_view(ns_view: *mut c_void) -> *mut c_void;
    fn noland_macos_input_install(ns_view: *mut c_void) -> i32;
    fn noland_macos_input_uninstall(ns_view: *mut c_void);
    fn noland_macos_input_set_capture_active(ns_view: *mut c_void, active: bool, mode: i32) -> i32;
}

pub fn install_native_stream_input<R: Runtime>(
    window: &Window<R>,
    input: Arc<InputManager>,
) -> Result<(), MoonlightError> {
    let _ = INPUT_MANAGER.set(Arc::downgrade(&input));

    #[cfg(target_os = "macos")]
    {
        let view = appkit_view_ptr(window)?;
        let result = unsafe { noland_macos_input_install(view) };
        if result != 0 {
            return Err(MoonlightError::Native(format!(
                "failed to install macOS native stream input bridge: {result}"
            )));
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        let _ = input;
    }

    Ok(())
}

pub fn uninstall_native_stream_input<R: Runtime>(window: &Window<R>) -> Result<(), MoonlightError> {
    #[cfg(target_os = "macos")]
    {
        let view = appkit_view_ptr(window)?;
        unsafe { noland_macos_input_uninstall(view) };
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
    }

    Ok(())
}

pub fn activate_native_stream_input<R: Runtime>(
    window: &Window<R>,
    mode: MouseMode,
) -> Result<bool, MoonlightError> {
    #[cfg(target_os = "macos")]
    {
        let view = appkit_view_ptr(window)?;
        let result =
            unsafe { noland_macos_input_set_capture_active(view, true, native_capture_mode(mode)) };
        return match result {
            0 => Ok(true),
            1 => Ok(false),
            other => Err(MoonlightError::Native(format!(
                "failed to activate macOS native stream input capture: {other}"
            ))),
        };
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        let _ = mode;
        Ok(false)
    }
}

pub fn deactivate_native_stream_input<R: Runtime>(
    window: &Window<R>,
) -> Result<bool, MoonlightError> {
    #[cfg(target_os = "macos")]
    {
        let view = appkit_view_ptr(window)?;
        let result = unsafe { noland_macos_input_set_capture_active(view, false, 0) };
        return match result {
            0 => Ok(true),
            1 => Ok(false),
            other => Err(MoonlightError::Native(format!(
                "failed to deactivate macOS native stream input capture: {other}"
            ))),
        };
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        Ok(false)
    }
}

#[cfg(target_os = "macos")]
fn appkit_view_ptr<R: Runtime>(window: &Window<R>) -> Result<*mut c_void, MoonlightError> {
    let raw_window = window
        .window_handle()
        .map_err(|error| MoonlightError::Native(error.to_string()))?
        .as_raw();
    match raw_window {
        RawWindowHandle::AppKit(handle) => {
            let resolved =
                unsafe { noland_macos_resolve_stream_target_view(handle.ns_view.as_ptr()) };
            Ok(if resolved.is_null() {
                handle.ns_view.as_ptr()
            } else {
                resolved
            })
        }
        other => Err(MoonlightError::Native(format!(
            "expected AppKit window handle for native stream input, got {other:?}"
        ))),
    }
}

#[cfg(not(target_os = "macos"))]
fn appkit_view_ptr<R: Runtime>(_window: &Window<R>) -> Result<*mut c_void, MoonlightError> {
    Err(MoonlightError::Native(
        "native stream input is only available on macOS".to_string(),
    ))
}

#[cfg(target_os = "macos")]
fn native_capture_mode(mode: MouseMode) -> i32 {
    match mode {
        MouseMode::Relative => 1,
        MouseMode::Absolute => 2,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_relative_mouse(delta_x: f64, delta_y: f64) {
    DEBUG_RUST_RELATIVE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    if let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) {
        manager.relative_motion(delta_x.round() as i32, delta_y.round() as i32);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_absolute_mouse(
    x: f64,
    y: f64,
    content_width: f64,
    content_height: f64,
) {
    DEBUG_RUST_ABSOLUTE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    if let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) {
        manager.absolute_motion_in_content(x, y, content_width, content_height);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_mouse_button(button: u8, pressed: bool) {
    DEBUG_RUST_BUTTON_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) else {
        return;
    };
    let Some(button) = map_mouse_button(button) else {
        return;
    };
    manager.mouse_button(
        button,
        if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        },
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_vertical_scroll(amount: f64, high_resolution: bool) {
    if let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) {
        manager.vertical_scroll(amount.round() as i32, high_resolution);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_horizontal_scroll(amount: f64, high_resolution: bool) {
    if let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) {
        manager.horizontal_scroll(amount.round() as i32, high_resolution);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_keyboard(virtual_key: u16, pressed: bool, modifiers: u8) {
    DEBUG_RUST_KEY_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    if let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) {
        manager.key(
            virtual_key,
            if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
            modifiers,
            false,
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_focus_changed(focused: bool) {
    if let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) {
        manager.set_focus(focused);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_capture_changed(active: bool, mode: i32) {
    DEBUG_CAPTURE_ACTIVE.store(active, Ordering::Relaxed);
    DEBUG_CAPTURE_MODE.store(mode, Ordering::Relaxed);
    let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) else {
        return;
    };

    if active {
        let mode = match mode {
            1 => MouseMode::Relative,
            2 => MouseMode::Absolute,
            _ => manager.capture_state().mouse_mode,
        };
        manager.begin_capture(mode);
    } else {
        manager.end_capture();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_request_capture() -> i32 {
    DEBUG_CAPTURE_REQUESTS.fetch_add(1, Ordering::Relaxed);
    let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) else {
        return 0;
    };

    native_capture_mode(manager.request_native_capture())
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_event(kind: i32) {
    match kind {
        1 => {
            DEBUG_NATIVE_MOUSE_MOVES.fetch_add(1, Ordering::Relaxed);
        }
        2 => {
            DEBUG_NATIVE_MOUSE_DOWNS.fetch_add(1, Ordering::Relaxed);
        }
        3 => {
            DEBUG_NATIVE_MOUSE_UPS.fetch_add(1, Ordering::Relaxed);
        }
        4 => {
            DEBUG_NATIVE_KEYS.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

fn map_mouse_button(button: u8) -> Option<MouseButton> {
    match button {
        0x01 => Some(MouseButton::Left),
        0x02 => Some(MouseButton::Middle),
        0x03 => Some(MouseButton::Right),
        0x04 => Some(MouseButton::X1),
        0x05 => Some(MouseButton::X2),
        _ => None,
    }
}
