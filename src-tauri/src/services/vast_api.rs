use std::{collections::HashSet, time::Instant};

use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::{
    errors::{AppError, AppResult},
    models::vast::{VastInstance, VastOffer, VastSshKey},
};

#[derive(Clone)]
pub struct VastApiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl VastApiClient {
    pub fn new(http: reqwest::Client, base_url: String, api_key: String) -> Self {
        Self {
            http,
            base_url,
            api_key,
        }
    }

    pub async fn search_offers(
        &self,
        min_reliability: f64,
        limit: usize,
        geolocation_country_code: Option<&str>,
        require_verified: bool,
        require_datacenter: bool,
        require_avx: bool,
    ) -> AppResult<Vec<VastOffer>> {
        let categories = ["ondemand", "bid", "reserved"];
        let mut merged = Vec::new();
        let mut seen = HashSet::new();
        let mut last_error: Option<AppError> = None;

        for category in categories {
            match self
                .search_offers_for_category(
                    min_reliability,
                    limit,
                    category,
                    geolocation_country_code,
                    require_verified,
                    require_datacenter,
                    require_avx,
                )
                .await
            {
                Ok(offers) => {
                    for offer in offers {
                        if seen.insert(offer.id) {
                            merged.push(offer);
                        }
                    }
                }
                Err(error) => {
                    warn!(
                        "Vast search_offers category={} failed (continuing): {}",
                        category, error
                    );
                    last_error = Some(error);
                }
            }
        }

        if merged.is_empty() {
            if let Some(error) = last_error {
                return Err(error);
            }
        }

        debug!(
            "Vast returned {} offers across all categories",
            merged.len()
        );
        Ok(merged)
    }

    async fn search_offers_for_category(
        &self,
        min_reliability: f64,
        limit: usize,
        category: &str,
        geolocation_country_code: Option<&str>,
        require_verified: bool,
        require_datacenter: bool,
        require_avx: bool,
    ) -> AppResult<Vec<VastOffer>> {
        let url = format!("{}/api/v0/bundles/", self.base_url.trim_end_matches('/'));
        let mut payload = json!({
            "limit": limit,
            "type": category,
            "rentable": { "eq": true },
            "rented": { "eq": false },
            "reliability": { "gte": min_reliability.max(0.8) },
            "gpu_arch": { "eq": "nvidia" },
            "vms_enabled": { "eq": true },
            "num_gpus": { "eq": 1 },
            "order": [["dph_total", "asc"]]
        });

        if let Some(country_code) = geolocation_country_code {
            let trimmed = country_code.trim();
            if !trimmed.is_empty() {
                payload["geolocation"] = json!({ "in": [trimmed.to_uppercase()] });
            }
        }

        if require_verified {
            payload["verification"] = json!({ "eq": "verified" });
        }

        if require_datacenter {
            payload["datacenter"] = json!({ "eq": true });
        }

        if require_avx {
            payload["has_avx"] = json!({ "eq": true });
        }

        info!(
            "Vast request search_offers category={} limit={} min_reliability={} endpoint={}",
            category, limit, min_reliability, url
        );
        let started = Instant::now();

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|error| map_send_error("POST", &url, error))?;

        let body = parse_response(response, "POST", &url, started).await?;
        let offers_value = body
            .get("offers")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let normalized_category = normalize_offer_category(category);
        let offers = offers_value
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| VastOffer::from_value(&value))
            .map(|mut offer| {
                offer.offer_type = normalized_category.clone();
                offer
            })
            .collect::<Vec<_>>();

        debug!(
            "Vast returned {} offers for category {}",
            offers.len(),
            category
        );
        Ok(offers)
    }

    pub async fn create_instance(
        &self,
        offer_id: u64,
        template_hash: &str,
        storage_gb: u32,
        label: &str,
    ) -> AppResult<VastInstance> {
        let url = format!(
            "{}/api/v0/asks/{offer_id}/",
            self.base_url.trim_end_matches('/')
        );

        let payload = json!({
            "template_hash_id": template_hash,
            "disk": storage_gb,
            "runtype": "vm",
            "vm": true,
            "target_state": "running",
            "label": label,
            "cancel_unavail": true
        });

        info!(
            "Vast request create_instance offer_id={} storage_gb={} template_hash={} endpoint={}",
            offer_id, storage_gb, template_hash, url
        );
        let started = Instant::now();

        let response = self
            .http
            .put(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|error| map_send_error("PUT", &url, error))?;

        let body = parse_response(response, "PUT", &url, started).await?;
        info!(
            "Vast create_instance raw response offer_id={} body={}",
            offer_id,
            abbreviate_text(&body.to_string())
        );
        let contract_id = body
            .get("new_contract")
            .or_else(|| body.get("instance_id"))
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AppError::Api(format!(
                    "Vast create instance response did not include contract id: {}",
                    abbreviate_text(&body.to_string())
                ))
            })?;

        info!(
            "Vast create_instance accepted offer_id={} contract_id={}",
            offer_id, contract_id
        );

        let instance = self.get_instance(contract_id).await.map_err(|error| {
            warn!(
                "Vast create_instance contract_id={} accepted but get_instance failed: {}",
                contract_id, error
            );
            AppError::Api(format!(
                "Vast created contract {} for offer {} but fetching instance details failed: {}",
                contract_id, offer_id, error
            ))
        })?;

        info!(
            "Vast create_instance hydrated contract_id={} status={} ssh={}:{}",
            instance.id, instance.status, instance.ssh_host, instance.ssh_port
        );
        Ok(instance)
    }

    pub async fn get_instance(&self, instance_id: u64) -> AppResult<VastInstance> {
        let url = format!(
            "{}/api/v0/instances/{instance_id}/",
            self.base_url.trim_end_matches('/')
        );

        info!(
            "Vast request get_instance instance_id={} endpoint={}",
            instance_id, url
        );
        let started = Instant::now();

        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|error| map_send_error("GET", &url, error))?;

        let body = parse_response(response, "GET", &url, started).await?;
        let parsed_instances = extract_instance_values(&body)
            .into_iter()
            .filter_map(|value| {
                VastInstance::from_value_with_fallback_id(&value, Some(instance_id))
            })
            .collect::<Vec<_>>();

        if let Some(exact_match) = parsed_instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .cloned()
        {
            return Ok(exact_match);
        }

        parsed_instances.into_iter().next().ok_or_else(|| {
            AppError::Api(format!(
                "Vast instance payload missing expected fields for instance {}: {}",
                instance_id,
                abbreviate_text(&body.to_string())
            ))
        })
    }

    pub async fn list_instances(&self) -> AppResult<Vec<VastInstance>> {
        let url = format!("{}/api/v0/instances/", self.base_url.trim_end_matches('/'));

        info!("Vast request list_instances endpoint={}", url);
        let started = Instant::now();

        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|error| map_send_error("GET", &url, error))?;
        let body = parse_response(response, "GET", &url, started).await?;

        let values = extract_instance_values(&body);
        let instances = values
            .into_iter()
            .filter_map(|value| VastInstance::from_value(&value))
            .collect::<Vec<_>>();

        Ok(instances)
    }

    pub async fn list_ssh_keys(&self) -> AppResult<Vec<VastSshKey>> {
        let url = format!("{}/api/v0/ssh/", self.base_url.trim_end_matches('/'));

        info!("Vast request list_ssh_keys endpoint={}", url);
        let started = Instant::now();

        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|error| map_send_error("GET", &url, error))?;

        let body = parse_response(response, "GET", &url, started).await?;

        let keys = body
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| {
                Some(VastSshKey {
                    id: item.get("id")?.as_u64()?,
                    key: item
                        .get("key")
                        .or_else(|| item.get("public_key"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect::<Vec<_>>();

        Ok(keys)
    }

    pub async fn upload_ssh_key(&self, public_key: &str) -> AppResult<()> {
        let url = format!("{}/api/v0/ssh/", self.base_url.trim_end_matches('/'));
        let payload = json!({ "ssh_key": public_key });

        info!(
            "Vast request upload_ssh_key endpoint={} key_length={}",
            url,
            public_key.len()
        );
        let started = Instant::now();

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|error| map_send_error("POST", &url, error))?;

        let _ = parse_response(response, "POST", &url, started).await?;
        Ok(())
    }

    pub async fn pause_instance(&self, instance_id: u64) -> AppResult<VastInstance> {
        let url = format!(
            "{}/api/v0/instances/{instance_id}/",
            self.base_url.trim_end_matches('/')
        );
        let payload = json!({ "target_state": "stopped" });

        info!(
            "Vast request pause_instance instance_id={} endpoint={}",
            instance_id, url
        );
        let started = Instant::now();

        let response = self
            .http
            .put(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|error| map_send_error("PUT", &url, error))?;

        let body = parse_response(response, "PUT", &url, started).await?;
        info!(
            "Vast pause_instance instance_id={} response={}",
            instance_id,
            abbreviate_text(&body.to_string())
        );

        self.get_instance(instance_id).await
    }

    pub async fn destroy_instance(&self, instance_id: u64) -> AppResult<()> {
        let url = format!(
            "{}/api/v0/instances/{instance_id}/",
            self.base_url.trim_end_matches('/')
        );

        info!(
            "Vast request destroy_instance instance_id={} endpoint={}",
            instance_id, url
        );
        let started = Instant::now();

        let response = self
            .http
            .delete(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|error| map_send_error("DELETE", &url, error))?;

        let _ = parse_response(response, "DELETE", &url, started).await?;
        info!("Vast destroy_instance instance_id={} succeeded", instance_id);
        Ok(())
    }
}

fn normalize_offer_category(category: &str) -> String {
    match category.trim().to_ascii_lowercase().as_str() {
        "bid" | "interruptible" => "interruptible".to_string(),
        "reserved" => "reserved".to_string(),
        _ => "on-demand".to_string(),
    }
}

fn extract_instance_values(body: &Value) -> Vec<Value> {
    if let Some(array) = body.as_array() {
        return array.clone();
    }

    if let Some(instance) = body.get("instance") {
        if instance.is_object() {
            return vec![instance.clone()];
        }
    }

    if let Some(instances) = body.get("instances") {
        if let Some(array) = instances.as_array() {
            return array.clone();
        }

        if let Some(object) = instances.as_object() {
            let mut values = Vec::new();

            if looks_like_instance_object(object) {
                values.push(Value::Object(object.clone()));
            }

            for (key, value) in object {
                if let Some(array) = value.as_array() {
                    values.extend(array.clone());
                } else if value.is_object() {
                    if let Ok(id) = key.parse::<u64>() {
                        if let Some(instance_obj) = value.as_object() {
                            let mut merged = instance_obj.clone();
                            merged
                                .entry("id".to_string())
                                .or_insert_with(|| Value::from(id));
                            values.push(Value::Object(merged));
                            continue;
                        }
                    }

                    values.push(value.clone());
                }
            }

            if values.is_empty() {
                for (key, value) in object {
                    if let Ok(id) = key.parse::<u64>() {
                        if let Some(instance_obj) = value.as_object() {
                            let mut merged = instance_obj.clone();
                            merged
                                .entry("id".to_string())
                                .or_insert_with(|| Value::from(id));
                            values.push(Value::Object(merged));
                        }
                    }
                }
            }

            if !values.is_empty() {
                return values;
            }
        }
    }

    if body.is_object() {
        return vec![body.clone()];
    }

    Vec::new()
}

fn looks_like_instance_object(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("actual_status")
        || object.contains_key("status")
        || object.contains_key("cur_state")
        || object.contains_key("ssh_host")
        || object.contains_key("ssh_port")
        || object.contains_key("public_ipaddr")
}

async fn parse_response(
    response: reqwest::Response,
    method: &str,
    url: &str,
    started: Instant,
) -> AppResult<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| AppError::Api(format!("{method} {url} response read failed: {error}")))?;
    let elapsed_ms = started.elapsed().as_millis();

    let parsed = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));

    let response_body_excerpt = abbreviate_text(&parsed.to_string());

    if !status.is_success() {
        let detail = parsed
            .get("msg")
            .or_else(|| parsed.get("detail"))
            .or_else(|| parsed.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown Vast API failure")
            .to_string();

        warn!(
            "Vast API {} {} -> {} in {}ms | detail: {} | body: {}",
            method, url, status, elapsed_ms, detail, response_body_excerpt
        );

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AppError::Authentication);
        }

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::NotFound(format!("{method} {url}: {detail}")));
        }

        return Err(AppError::Api(format!(
            "{method} {url} -> {status}: {detail}"
        )));
    }

    info!(
        "Vast API {} {} -> {} in {}ms",
        method, url, status, elapsed_ms
    );
    debug!(
        "Vast API response {} {} body: {}",
        method, url, response_body_excerpt
    );

    Ok(parsed)
}

fn map_send_error(method: &str, url: &str, error: reqwest::Error) -> AppError {
    if error.status() == Some(reqwest::StatusCode::UNAUTHORIZED)
        || error.status() == Some(reqwest::StatusCode::FORBIDDEN)
    {
        return AppError::Authentication;
    }

    AppError::Api(format!("{method} {url} request failed: {error}"))
}

fn abbreviate_text(raw: &str) -> String {
    const MAX_CHARS: usize = 600;
    if raw.chars().count() <= MAX_CHARS {
        return raw.to_string();
    }

    let mut output = raw.chars().take(MAX_CHARS).collect::<String>();
    output.push_str("... <truncated>");
    output
}
