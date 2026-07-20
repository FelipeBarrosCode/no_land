#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use std::sync::Arc;

use tauri::{Runtime, Window};

use crate::{
    input::{manager::InputManager, state::MouseMode},
    moonlight::domain::MoonlightError,
};

pub trait NativeInputBackend: Send {
    fn install_window<R: Runtime>(
        &mut self,
        window: &Window<R>,
        input: Arc<InputManager>,
    ) -> Result<(), MoonlightError>;
    fn start_capture(&mut self, mode: MouseMode) -> Result<bool, MoonlightError>;
    fn stop_capture(&mut self) -> Result<bool, MoonlightError>;
    fn set_focused(&mut self, focused: bool);
    fn uninstall(&mut self) -> Result<(), MoonlightError>;
}
