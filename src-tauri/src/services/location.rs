use serde_json::Value;

use crate::{
    errors::{AppError, AppResult},
    models::app_state::{LocationSource, LocationState, ManualLocationInput},
};

#[derive(Clone)]
pub struct LocationService {
    client: reqwest::Client,
}

impl LocationService {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    pub async fn detect_ip_location(&self) -> AppResult<LocationState> {
        let primary = self
            .client
            .get("https://ipapi.co/json/")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(AppError::from)?
            .json::<Value>()
            .await
            .map_err(AppError::from);

        if let Ok(value) = primary {
            return Ok(LocationState {
                source: LocationSource::Ip,
                city: value
                    .get("city")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                region: value
                    .get("region")
                    .or_else(|| value.get("region_code"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                country: value
                    .get("country_name")
                    .or_else(|| value.get("country"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                latitude: value
                    .get("latitude")
                    .and_then(Value::as_f64)
                    .unwrap_or_default(),
                longitude: value
                    .get("longitude")
                    .and_then(Value::as_f64)
                    .unwrap_or_default(),
            });
        }

        let fallback = self
            .client
            .get("https://ipinfo.io/json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(AppError::from)?
            .json::<Value>()
            .await
            .map_err(AppError::from)?;

        let (lat, lon) = fallback
            .get("loc")
            .and_then(Value::as_str)
            .and_then(|loc| {
                let mut parts = loc.split(',');
                let lat = parts.next()?.parse::<f64>().ok()?;
                let lon = parts.next()?.parse::<f64>().ok()?;
                Some((lat, lon))
            })
            .unwrap_or((0.0, 0.0));

        Ok(LocationState {
            source: LocationSource::Ip,
            city: fallback
                .get("city")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            region: fallback
                .get("region")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            country: fallback
                .get("country")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            latitude: lat,
            longitude: lon,
        })
    }

    pub fn from_manual(input: ManualLocationInput) -> AppResult<LocationState> {
        if !input.latitude.is_finite() || !input.longitude.is_finite() {
            return Err(AppError::InvalidInput(
                "Manual coordinates must be valid numbers".to_string(),
            ));
        }

        Ok(LocationState {
            source: LocationSource::Manual,
            city: input.city,
            region: input.region,
            country: input.country,
            latitude: input.latitude,
            longitude: input.longitude,
        })
    }

    pub async fn resolve_os_location(
        &self,
        input: ManualLocationInput,
    ) -> AppResult<LocationState> {
        if !input.latitude.is_finite() || !input.longitude.is_finite() {
            return Err(AppError::InvalidInput(
                "OS coordinates must be valid numbers".to_string(),
            ));
        }

        let from_api = self
            .reverse_geocode(input.latitude, input.longitude)
            .await
            .ok();

        let city = input.city.trim().to_string();
        let region = input.region.trim().to_string();
        let country = input.country.trim().to_string();

        let city = if city.is_empty() {
            from_api
                .as_ref()
                .map(|loc| loc.city.clone())
                .unwrap_or_default()
        } else {
            city
        };
        let region = if region.is_empty() {
            from_api
                .as_ref()
                .map(|loc| loc.region.clone())
                .unwrap_or_default()
        } else {
            region
        };
        let country = if country.is_empty() {
            from_api
                .as_ref()
                .map(|loc| loc.country.clone())
                .unwrap_or_default()
        } else {
            country
        };

        Ok(LocationState {
            source: LocationSource::Os,
            city,
            region,
            country,
            latitude: input.latitude,
            longitude: input.longitude,
        })
    }

    async fn reverse_geocode(&self, latitude: f64, longitude: f64) -> AppResult<LocationState> {
        let primary_url = format!(
            "https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat={latitude}&lon={longitude}&zoom=10&addressdetails=1"
        );

        let primary = self
            .client
            .get(primary_url)
            .header(reqwest::header::USER_AGENT, "noland-connect/0.1")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(AppError::from)?
            .json::<Value>()
            .await
            .map_err(AppError::from);

        if let Ok(value) = primary {
            let address = value.get("address").cloned().unwrap_or(Value::Null);
            let city = address
                .get("city")
                .or_else(|| address.get("town"))
                .or_else(|| address.get("village"))
                .or_else(|| address.get("hamlet"))
                .or_else(|| address.get("county"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let region = address
                .get("state")
                .or_else(|| address.get("state_district"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let country = address
                .get("country")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            if !city.is_empty() || !region.is_empty() || !country.is_empty() {
                return Ok(LocationState {
                    source: LocationSource::Os,
                    city,
                    region,
                    country,
                    latitude,
                    longitude,
                });
            }
        }

        let fallback_url = format!(
            "https://api.bigdatacloud.net/data/reverse-geocode-client?latitude={latitude}&longitude={longitude}&localityLanguage=en"
        );
        let fallback = self
            .client
            .get(fallback_url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(AppError::from)?
            .json::<Value>()
            .await
            .map_err(AppError::from)?;

        Ok(LocationState {
            source: LocationSource::Os,
            city: fallback
                .get("city")
                .or_else(|| fallback.get("locality"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            region: fallback
                .get("principalSubdivision")
                .or_else(|| fallback.get("region"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            country: fallback
                .get("countryName")
                .or_else(|| fallback.get("country"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            latitude,
            longitude,
        })
    }

    pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
        let earth_radius_km = 6371.0;
        let dlat = (lat2 - lat1).to_radians();
        let dlon = (lon2 - lon1).to_radians();
        let lat1_rad = lat1.to_radians();
        let lat2_rad = lat2.to_radians();

        let a = (dlat / 2.0).sin().powi(2)
            + lat1_rad.cos() * lat2_rad.cos() * (dlon / 2.0).sin().powi(2);
        let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
        earth_radius_km * c
    }
}
