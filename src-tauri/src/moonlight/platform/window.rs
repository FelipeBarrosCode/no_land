use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

#[cfg(all(target_os = "macos", not(test)))]
unsafe extern "C" {
    fn noland_macos_resolve_stream_target_view(ns_view: *mut c_void) -> *mut c_void;
}
use tauri::{AppHandle, Manager, Runtime, Window};

use crate::moonlight::{
    domain::MoonlightError, native, platform::desktop_input::uninstall_native_stream_input,
};

pub const STREAM_WINDOW_LABEL: &str = "moonlight-stream";

#[derive(Debug, Default)]
pub struct StreamWindowCloseState {
    allow_close: AtomicBool,
    closing_in_progress: AtomicBool,
}

impl StreamWindowCloseState {
    pub fn allow_close(&self) -> bool {
        self.allow_close.load(Ordering::SeqCst)
    }

    pub fn set_allow_close(&self, value: bool) {
        self.allow_close.store(value, Ordering::SeqCst);
    }

    pub fn begin_close_intercept(&self) -> bool {
        !self.closing_in_progress.swap(true, Ordering::SeqCst)
    }

    pub fn finish_close_intercept(&self) {
        self.closing_in_progress.store(false, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.allow_close.store(false, Ordering::SeqCst);
        self.closing_in_progress.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeSurfaceDescriptor {
    pub surface_type: native::nl_surface_type_t,
    pub window_handle: usize,
    pub display_handle: usize,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

impl NativeSurfaceDescriptor {
    pub fn to_native(&self) -> native::nl_surface_descriptor_t {
        native::nl_surface_descriptor_t {
            surface_type: self.surface_type,
            window_handle: self.window_handle as *mut c_void,
            display_handle: self.display_handle as *mut c_void,
            width: self.width,
            height: self.height,
            scale_factor: self.scale_factor,
        }
    }
}

pub fn create_or_reuse_stream_window<R: Runtime>(
    app: &AppHandle<R>,
    width: u32,
    height: u32,
    title: &str,
) -> Result<Window<R>, MoonlightError> {
    if let Some(window) = app.get_window(STREAM_WINDOW_LABEL) {
        app.state::<StreamWindowCloseState>().reset();
        let _ = window.set_fullscreen(false);
        let _ = window.set_title(title);
        window
            .hide()
            .map_err(|error| MoonlightError::Native(error.to_string()))?;
        return Ok(window);
    }

    app.state::<StreamWindowCloseState>().reset();
    let window = tauri::window::WindowBuilder::new(app, STREAM_WINDOW_LABEL)
        .title(title)
        .inner_size(width as f64, height as f64)
        .resizable(true)
        .decorations(true)
        .visible(false)
        .title_bar_style(tauri::utils::TitleBarStyle::Overlay)
        .hidden_title(true)
        .build()
        .map_err(|error| MoonlightError::Native(error.to_string()))?;
    Ok(window)
}

pub fn close_stream_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), MoonlightError> {
    app.state::<StreamWindowCloseState>().set_allow_close(true);
    app.state::<StreamWindowCloseState>()
        .finish_close_intercept();
    if let Some(window) = app.get_window(STREAM_WINDOW_LABEL) {
        let _ = uninstall_native_stream_input(&window);
        window
            .close()
            .map_err(|error| MoonlightError::Native(error.to_string()))?;
    }
    Ok(())
}

pub fn stream_window_surface_descriptor<R: Runtime>(
    window: &Window<R>,
) -> Result<NativeSurfaceDescriptor, MoonlightError> {
    let size = window
        .inner_size()
        .map_err(|error| MoonlightError::Native(error.to_string()))?;
    let scale_factor = window
        .scale_factor()
        .map_err(|error| MoonlightError::Native(error.to_string()))? as f32;
    let raw_window = window
        .window_handle()
        .map_err(|error| MoonlightError::Native(error.to_string()))?
        .as_raw();
    let raw_display = window
        .display_handle()
        .map_err(|error| MoonlightError::Native(error.to_string()))?
        .as_raw();

    surface_descriptor_from_raw_handles(
        raw_window,
        raw_display,
        size.width,
        size.height,
        scale_factor,
    )
}

fn surface_descriptor_from_raw_handles(
    raw_window: RawWindowHandle,
    raw_display: RawDisplayHandle,
    width: u32,
    height: u32,
    scale_factor: f32,
) -> Result<NativeSurfaceDescriptor, MoonlightError> {
    match raw_window {
        RawWindowHandle::AppKit(handle) => {
            let resolved_view = {
                #[cfg(all(target_os = "macos", not(test)))]
                {
                    let resolved =
                        unsafe { noland_macos_resolve_stream_target_view(handle.ns_view.as_ptr()) };
                    if resolved.is_null() {
                        handle.ns_view.as_ptr()
                    } else {
                        resolved
                    }
                }
                #[cfg(any(not(target_os = "macos"), test))]
                {
                    handle.ns_view.as_ptr()
                }
            };
            Ok(NativeSurfaceDescriptor {
                surface_type: native::nl_surface_type_NL_SURFACE_MACOS_NSVIEW,
                window_handle: resolved_view as usize,
                display_handle: match raw_display {
                    RawDisplayHandle::AppKit(_) => 0,
                    _ => 0,
                },
                width,
                height,
                scale_factor,
            })
        }
        RawWindowHandle::Win32(handle) => Ok(NativeSurfaceDescriptor {
            surface_type: native::nl_surface_type_NL_SURFACE_WINDOWS_HWND,
            window_handle: handle.hwnd.get() as usize,
            display_handle: handle
                .hinstance
                .map(|value| value.get() as usize)
                .unwrap_or(0),
            width,
            height,
            scale_factor,
        }),
        RawWindowHandle::Xlib(handle) => {
            let display_handle = match raw_display {
                RawDisplayHandle::Xlib(display) => display
                    .display
                    .map(|value| value.as_ptr() as usize)
                    .unwrap_or(0),
                _ => 0,
            };
            Ok(NativeSurfaceDescriptor {
                surface_type: native::nl_surface_type_NL_SURFACE_X11_WINDOW,
                window_handle: handle.window as usize,
                display_handle,
                width,
                height,
                scale_factor,
            })
        }
        RawWindowHandle::Wayland(handle) => {
            let display_handle = match raw_display {
                RawDisplayHandle::Wayland(display) => display.display.as_ptr() as usize,
                _ => 0,
            };
            Ok(NativeSurfaceDescriptor {
                surface_type: native::nl_surface_type_NL_SURFACE_WAYLAND_SURFACE,
                window_handle: handle.surface.as_ptr() as usize,
                display_handle,
                width,
                height,
                scale_factor,
            })
        }
        other => Err(MoonlightError::Native(format!(
            "unsupported raw window handle for moonlight stream surface: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::c_void, num::NonZeroIsize, ptr::NonNull};

    use raw_window_handle::{
        AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle,
        Win32WindowHandle, WindowsDisplayHandle,
    };

    use super::surface_descriptor_from_raw_handles;
    use crate::moonlight::native;

    #[test]
    fn maps_appkit_surface_descriptor() {
        let view = NonNull::<c_void>::dangling();
        let descriptor = surface_descriptor_from_raw_handles(
            RawWindowHandle::AppKit(AppKitWindowHandle::new(view)),
            RawDisplayHandle::AppKit(AppKitDisplayHandle::new()),
            1920,
            1080,
            2.0,
        )
        .unwrap();
        assert_eq!(
            descriptor.surface_type,
            native::nl_surface_type_NL_SURFACE_MACOS_NSVIEW
        );
        assert_eq!(descriptor.window_handle, view.as_ptr() as usize);
    }

    #[test]
    fn maps_win32_surface_descriptor() {
        let hwnd = NonZeroIsize::new(100).unwrap();
        let descriptor = surface_descriptor_from_raw_handles(
            RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)),
            RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
            1280,
            720,
            1.0,
        )
        .unwrap();
        assert_eq!(
            descriptor.surface_type,
            native::nl_surface_type_NL_SURFACE_WINDOWS_HWND
        );
        assert_eq!(descriptor.window_handle, 100isize as usize);
    }
}
