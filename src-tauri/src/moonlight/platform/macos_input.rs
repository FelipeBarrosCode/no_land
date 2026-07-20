use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{Mutex, OnceLock},
};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tauri::{Runtime, Window};
use tokio::sync::mpsc;

use crate::moonlight::{domain::MoonlightError, runtime::MoonlightRuntimeHandle};

#[derive(Debug)]
enum NativeInputEvent {
    RelativeMouse { delta_x: f64, delta_y: f64 },
    MouseButton { button: u8, pressed: bool },
}

static INPUT_TX: OnceLock<mpsc::UnboundedSender<NativeInputEvent>> = OnceLock::new();
static PENDING_EVENTS: OnceLock<Mutex<VecDeque<NativeInputEvent>>> = OnceLock::new();

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn noland_macos_input_install(ns_view: *mut c_void) -> i32;
    fn noland_macos_input_uninstall(ns_view: *mut c_void);
    fn noland_macos_input_set_capture_active(ns_view: *mut c_void, active: bool) -> i32;
}

pub fn install_native_stream_input<R: Runtime>(
    window: &Window<R>,
    runtime: MoonlightRuntimeHandle,
) -> Result<(), MoonlightError> {
    ensure_input_bridge(runtime);

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
) -> Result<bool, MoonlightError> {
    #[cfg(target_os = "macos")]
    {
        let view = appkit_view_ptr(window)?;
        let result = unsafe { noland_macos_input_set_capture_active(view, true) };
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
        Ok(false)
    }
}

pub fn deactivate_native_stream_input<R: Runtime>(
    window: &Window<R>,
) -> Result<bool, MoonlightError> {
    #[cfg(target_os = "macos")]
    {
        let view = appkit_view_ptr(window)?;
        let result = unsafe { noland_macos_input_set_capture_active(view, false) };
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

fn ensure_input_bridge(runtime: MoonlightRuntimeHandle) {
    if INPUT_TX.get().is_some() {
        return;
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    let _ = INPUT_TX.set(tx);
    let _ = PENDING_EVENTS.get_or_init(|| Mutex::new(VecDeque::new()));

    tauri::async_runtime::spawn(async move {
        loop {
            let next = if let Some(event) = PENDING_EVENTS
                .get()
                .and_then(|pending| pending.lock().ok().and_then(|mut queue| queue.pop_front()))
            {
                Some(event)
            } else {
                rx.recv().await
            };

            let Some(event) = next else {
                break;
            };

            match event {
                NativeInputEvent::RelativeMouse { delta_x, delta_y } => {
                    let mut sum_x = delta_x;
                    let mut sum_y = delta_y;

                    while let Ok(more) = rx.try_recv() {
                        match more {
                            NativeInputEvent::RelativeMouse { delta_x, delta_y } => {
                                sum_x += delta_x;
                                sum_y += delta_y;
                            }
                            other => {
                                if let Some(pending) = PENDING_EVENTS.get() {
                                    if let Ok(mut queue) = pending.lock() {
                                        queue.push_back(other);
                                    }
                                }
                                break;
                            }
                        }
                    }

                    let clamped_x = clamp_i16(sum_x.round());
                    let clamped_y = clamp_i16(sum_y.round());
                    if clamped_x != 0 || clamped_y != 0 {
                        let _ = runtime.send_relative_mouse(clamped_x, clamped_y).await;
                    }
                }
                NativeInputEvent::MouseButton { button, pressed } => {
                    let _ = runtime.send_mouse_button(button, pressed).await;
                }
            }
        }
    });
}

fn clamp_i16(value: f64) -> i16 {
    value.max(i16::MIN as f64).min(i16::MAX as f64) as i16
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

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_relative_mouse(delta_x: f64, delta_y: f64) {
    if let Some(tx) = INPUT_TX.get() {
        let _ = tx.send(NativeInputEvent::RelativeMouse { delta_x, delta_y });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_mouse_button(button: u8, pressed: bool) {
    if let Some(tx) = INPUT_TX.get() {
        let _ = tx.send(NativeInputEvent::MouseButton { button, pressed });
    }
}
