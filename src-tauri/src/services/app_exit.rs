use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

pub const QUIT_REQUESTED_EVENT: &str = "app:quit-requested";
pub const QUIT_DISMISSED_EVENT: &str = "app:quit-dismissed";

#[derive(Default)]
pub struct AppExitState {
    approved: AtomicBool,
    request: Mutex<QuitRequest>,
}

#[derive(Default)]
struct QuitRequest {
    pending: bool,
    handled: bool,
    presented: bool,
    fallback: bool,
    generation: u64,
}

impl AppExitState {
    pub fn needs_confirmation(&self, code: Option<i32>) -> bool {
        // An updater restart has already been explicitly requested by the user.
        code != Some(tauri::RESTART_EXIT_CODE) && !self.approved.load(Ordering::SeqCst)
    }

    fn approve(&self) {
        self.approved.store(true, Ordering::SeqCst);
    }

    pub fn request(&self) -> Option<u64> {
        let mut state = self.request.lock().unwrap_or_else(|e| e.into_inner());
        if state.pending {
            return None;
        }
        state.pending = true;
        state.handled = false;
        state.presented = false;
        state.generation += 1;
        Some(state.generation)
    }

    fn take_request(&self) -> bool {
        let mut state = self.request.lock().unwrap_or_else(|e| e.into_inner());
        if !state.pending || state.fallback || state.handled {
            return false;
        }
        state.handled = true;
        true
    }

    fn cancel(&self) {
        let mut state = self.request.lock().unwrap_or_else(|e| e.into_inner());
        if state.fallback {
            return;
        }
        state.pending = false;
        state.handled = false;
        state.presented = false;
    }

    fn present(&self) -> bool {
        let mut state = self.request.lock().unwrap_or_else(|e| e.into_inner());
        if !state.pending || state.fallback {
            return false;
        }
        state.presented = true;
        true
    }

    fn claim_fallback(&self, generation: u64) -> bool {
        let mut state = self.request.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation || !state.pending || state.presented || state.fallback {
            return false;
        }
        state.fallback = true;
        true
    }

    fn finish_fallback(&self, generation: u64) -> bool {
        let mut state = self.request.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return false;
        }
        state.fallback = false;
        true
    }
}

/// Called synchronously from BOTH native main-window CloseRequested and Quit.
/// The caller prevents close/exit before invoking this; React is not the guard.
pub fn request_app_quit(app: &AppHandle) {
    let generation = app.state::<AppExitState>().request();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit(QUIT_REQUESTED_EVENT, ());
        #[cfg(target_os = "macos")]
        let _ = app.show();
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    let Some(generation) = generation else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let state = app.state::<AppExitState>();
        if !state.claim_fallback(generation) {
            return;
        }
        let _ = app.emit_to("main", QUIT_DISMISSED_EVENT, ());
        let jobs = match app.try_state::<crate::mcp::McpManager>() {
            Some(manager) => manager.core.active_jobs().await,
            None => 0,
        };
        let handle = app.clone();
        app.dialog().message(format!("Quit Noland Connect? Cloud PCs are NOT stopped or deleted and may continue billing. {jobs} local agent jobs are active. Agent connections will disconnect; remote work may continue."))
            .title("Quit Noland Connect")
            .buttons(MessageDialogButtons::OkCancelCustom("Quit — leave cloud PCs running".into(), "Keep app open".into()))
            .show(move |quit| {
                let state = handle.state::<AppExitState>();
                if !state.finish_fallback(generation) {return;}
                if quit { state.approve(); handle.exit(0); } else { state.cancel(); let _ = handle.emit_to("main", QUIT_DISMISSED_EVENT, ()); }
            });
    });
}

#[tauri::command]
pub fn cancel_app_quit(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Only the main window can cancel exit".into());
    }
    app.state::<AppExitState>().cancel();
    let _ = app.emit_to("main", QUIT_DISMISSED_EVENT, ());
    Ok(())
}

#[tauri::command]
pub fn app_quit_dialog_ready(app: AppHandle, window: WebviewWindow) -> Result<bool, String> {
    if window.label() != "main" {
        return Err("Only the main window can present exit confirmation".into());
    }
    Ok(app.state::<AppExitState>().present())
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
        state.request();
        assert!(
            !state.take_request(),
            "repeated native requests reuse the same dialog"
        );
        state.cancel();
        state.request();
        assert!(
            state.take_request(),
            "a new close after cancel opens a new dialog"
        );
    }

    #[test]
    fn fallback_takes_over_only_if_the_dialog_did_not_render() {
        let state = AppExitState::default();
        let first = state.request().unwrap();
        assert!(state.take_request());
        assert!(
            state.claim_fallback(first),
            "delivery is not proof of rendering"
        );
        assert!(!state.take_request());
        assert!(
            !state.present(),
            "React cannot present a second dialog over native fallback"
        );
        state.finish_fallback(first);
        state.cancel();
        let second = state.request().unwrap();
        assert!(
            !state.claim_fallback(first),
            "old timeout cannot take over a new close"
        );
        assert!(state.present());
        assert!(!state.claim_fallback(second));
    }

    #[test]
    fn web_and_native_presenters_cannot_both_claim_the_dialog() {
        for _ in 0..50 {
            let state = std::sync::Arc::new(AppExitState::default());
            let generation = state.request().unwrap();
            let web = state.clone();
            let native = state.clone();
            let a = std::thread::spawn(move || web.present());
            let b = std::thread::spawn(move || native.claim_fallback(generation));
            assert_ne!(a.join().unwrap(), b.join().unwrap());
        }
    }
}
