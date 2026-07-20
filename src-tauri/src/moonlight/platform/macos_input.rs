use std::{
    ffi::c_void,
    sync::{Arc, OnceLock, Weak},
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

#[cfg(target_os = "macos")]
unsafe extern "C" {
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
        RawWindowHandle::AppKit(handle) => Ok(handle.ns_view.as_ptr()),
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
    if let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) {
        manager.relative_motion(delta_x.round() as i32, delta_y.round() as i32);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_absolute_mouse(x: f64, y: f64) {
    if let Some(manager) = INPUT_MANAGER.get().and_then(Weak::upgrade) {
        manager.absolute_motion(x, y);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_mouse_button(button: u8, pressed: bool) {
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
