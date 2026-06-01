use std::sync::Arc;

use axum::{extract::{Query, State}, Json};
use serde::Deserialize;

use crate::{
    api::{error::ApiError, state::ApiState},
    models::app_state::{OfferCandidate, PersistedAppState},
    services::{offer_selector::OfferSelector, vast_api::VastApiClient},
};

#[derive(Deserialize)]
pub struct SearchOffersQuery {
    pub page: Option<usize>,
    #[serde(rename = "pageSize")]
    pub page_size: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectOfferRequest {
    pub offer_id: u64,
    pub storage_gb: u32,
}

pub async fn search_offers(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SearchOffersQuery>,
) -> Result<Json<Vec<OfferCandidate>>, ApiError> {
    let state_snapshot = state.context.state.read().await.clone();
    if state_snapshot.credentials.vast_api_key.trim().is_empty() {
        return Err(ApiError::from_frontend(
            crate::errors::AppError::InvalidInput(
                "Missing Vast.ai API key. Complete onboarding first.".to_string(),
            )
            .into(),
        ));
    }
    let requested_page_size = query
        .page_size
        .unwrap_or(24)
        .clamp(1, state.context.config.offers_search_limit);
    let requested_page = query.page.unwrap_or(1).max(1);
    let page_start = (requested_page - 1).saturating_mul(requested_page_size);
    let needed_rows = page_start.saturating_add(requested_page_size).saturating_add(1);
    let fetch_limit = needed_rows.clamp(1, state.context.config.offers_search_limit);

    let vast = VastApiClient::new(
        state.context.http_client.clone(),
        state.context.config.vast_base_url.clone(),
        state_snapshot.credentials.vast_api_key.clone(),
    );
    let offers = vast
        .search_offers(
            state_snapshot.server_preferences.min_reliability,
            fetch_limit,
            Some(state_snapshot.server_preferences.geolocation_country_code.as_str()),
            state_snapshot.server_preferences.require_verified,
            state_snapshot.server_preferences.require_datacenter,
            state_snapshot.server_preferences.require_avx,
        )
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;

    let selector = OfferSelector {
        scoring: state.context.config.scoring.clone(),
    };
    let ranked = selector.rank_offers(offers, &state_snapshot.location);
    let paged = ranked
        .iter()
        .skip(page_start)
        .take(requested_page_size)
        .cloned()
        .collect::<Vec<_>>();
    {
        let mut cache = state.context.offer_cache.write().await;
        *cache = paged.clone();
    }
    Ok(Json(paged))
}

pub async fn select_offer(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<SelectOfferRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    if payload.storage_gb < 30 {
        return Err(ApiError::from_frontend(
            crate::errors::AppError::InvalidInput("Storage must be at least 30GB".to_string())
                .into(),
        ));
    }
    let selected = {
        let cache = state.context.offer_cache.read().await;
        cache.iter().find(|offer| offer.id == payload.offer_id).cloned()
    }
    .ok_or_else(|| {
        ApiError::from_frontend(
            crate::errors::AppError::NotFound(
                "Offer not found in current search results. Refresh offers and try again."
                    .to_string(),
            )
            .into(),
        )
    })?;

    let next_state = state
        .context
        .update_state(|state| {
            state.selected_offer = Some(selected);
            state.server_preferences.storage_gb = payload.storage_gb;
            state.orchestration_state = crate::models::app_state::OrchestrationState::ServerSelected;
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}
