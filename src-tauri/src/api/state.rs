use std::sync::Arc;

use tauri::AppHandle;

use crate::services::app_context::AppContext;

#[derive(Clone)]
pub struct ApiState {
    pub context: AppContext,
    pub app: AppHandle,
}

impl ApiState {
    pub fn new(context: AppContext, app: AppHandle) -> Arc<Self> {
        Arc::new(Self { context, app })
    }
}
