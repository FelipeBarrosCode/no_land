use std::sync::Arc;

use axum::{extract::State, Json};

use crate::{api::state::ApiState, models::app_state::PersistedAppState};

pub async fn get_app_state(State(state): State<Arc<ApiState>>) -> Json<PersistedAppState> {
    Json(state.context.load_state().await)
}
