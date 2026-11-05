use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewWindow};

pub const QUIT_REQUESTED_EVENT: &str = "app:quit-requested";

#[derive(Default)]
pub struct AppExitState {
    approved: AtomicBool,
    pending: AtomicBool,
}

impl AppExitState {
    pub fn needs_confirmation(&self, code: Option<i32>) -> bool {
        // An updater restart has already been explicitly requested by the user.
        code != Some(tauri::RESTART_EXIT_CODE) && !self.approved.load(Ordering::SeqCst)
    }

    fn approve(&self) {
        self.approved.store(true, Ordering::SeqCst);
    }

    pub fn request(&self) {
        self.pending.store(true, Ordering::SeqCst);
    }

    fn take_request(&self) -> bool {
        self.pending.swap(false, Ordering::SeqCst)
    }
}

#[tauri::command]
pub fn take_app_quit_request(app: AppHandle, window: WebviewWindow) -> Result<bool, String> {
    if window.label() != "main" {
        return Err("Only the main window can read application quit requests".to_string());
    }
    Ok(app.state::<AppExitState>().take_request())
}

#[tauri::command]
pub fn confirm_app_exit(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Only the main window can confirm application exit".to_string());
    }
    app.state::<AppExitState>().approve();
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::AppExitState;

    #[test]
    fn user_and_programmatic_quit_require_confirmation() {
        let state = AppExitState::default();
        assert!(state.needs_confirmation(None));
        assert!(state.needs_confirmation(Some(0)));
    }

    #[test]
    fn confirmed_exit_does_not_reopen_confirmation() {
        let state = AppExitState::default();
        state.approve();
        assert!(!state.needs_confirmation(Some(0)));
        assert!(!state.needs_confirmation(None));
    }

    #[test]
    fn updater_restart_is_not_intercepted() {
        let state = AppExitState::default();
        assert!(!state.needs_confirmation(Some(tauri::RESTART_EXIT_CODE)));
        assert!(state.needs_confirmation(None));
    }

    #[test]
    fn quit_before_frontend_subscription_is_replayed_once() {
        let state = AppExitState::default();
        assert!(!state.take_request());
        state.request();
        state.request();
        assert!(state.take_request());
        assert!(!state.take_request());
    }
}
