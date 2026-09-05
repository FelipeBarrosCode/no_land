use crate::{
    models::{
        app_state::{LocationState, OfferCandidate},
        vast::VastOffer,
    },
    services::{app_config::OfferScoring, location::LocationService},
};

#[derive(Debug, Clone)]
pub struct OfferSelector {
    pub scoring: OfferScoring,
}

impl OfferSelector {
    pub fn rank_offers(
        &self,
        offers: Vec<VastOffer>,
        location: &LocationState,
    ) -> Vec<OfferCandidate> {
        // Keep all offers returned by the API/category merge.
        // Ranking should sort, not silently remove market options.
        let expanded = offers;

        let mut candidates = expanded
            .into_iter()
            .map(|offer| {
                let distance = if location.latitude.abs() > f64::EPSILON
                    || location.longitude.abs() > f64::EPSILON
                {
                    LocationService::haversine_km(
                        location.latitude,
                        location.longitude,
                        offer.latitude,
                        offer.longitude,
                    )
                } else {
                    99999.0
                };

                let score = self.score_offer(distance, offer.hourly_price, offer.gpu_ram_mb as f64);

                OfferCandidate {
                    id: offer.id,
                    host_id: offer.host_id,
                    host_label: offer.host_label.clone(),
                    location_label: format_location_label(&offer),
                    city: offer.city.clone(),
                    region: offer.region.clone(),
                    country: offer.country.clone(),
                    latitude: offer.latitude,
                    longitude: offer.longitude,
                    reliability: offer.reliability,
                    gpu_name: offer.gpu_name,
                    gpu_ram_mb: offer.gpu_ram_mb,
                    gpu_count: offer.gpu_count,
                    cpu_name: offer.cpu_name,
                    cpu_cores: offer.cpu_cores,
                    internet_down_mbps: offer.internet_down_mbps,
                    internet_up_mbps: offer.internet_up_mbps,
                    hourly_price: offer.hourly_price,
                    compute_hourly_price: offer.compute_hourly_price,
                    storage_hourly_price: offer.storage_hourly_price,
                    available_storage_gb: offer.available_storage_gb,
                    estimated_distance_km: distance,
                    score,
                    time_remaining_hours: offer.time_remaining_hours,
                    is_verified: offer.is_verified,
                    is_datacenter: offer.is_datacenter,
                    offer_type: offer.offer_type,
                    has_static_ip: offer.has_static_ip,
                    has_avx: offer.has_avx,
                }
            })
            .collect::<Vec<_>>();

        candidates.sort_by(|left, right| {
            location_match_rank(left, location)
                .cmp(&location_match_rank(right, location))
                .then_with(|| {
                    gpu_preference_rank(&left.gpu_name).cmp(&gpu_preference_rank(&right.gpu_name))
                })
                .then_with(|| {
                    left.hourly_price
                        .partial_cmp(&right.hourly_price)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    left.estimated_distance_km
                        .partial_cmp(&right.estimated_distance_km)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| right.gpu_ram_mb.cmp(&left.gpu_ram_mb))
                .then_with(|| {
                    right
                        .internet_down_mbps
                        .partial_cmp(&left.internet_down_mbps)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        candidates
    }

    fn score_offer(&self, distance_km: f64, hourly_price: f64, vram_mb: f64) -> f64 {
        let normalized_distance = 1.0 / (1.0 + distance_km.max(0.0));
        let normalized_price = 1.0 / (1.0 + hourly_price.max(0.0));
        let normalized_vram = (vram_mb / 24576.0).min(2.0);

        normalized_distance * self.scoring.distance_weight
            + normalized_price * self.scoring.price_weight
            + normalized_vram * self.scoring.vram_weight
    }
}

fn location_match_rank(candidate: &OfferCandidate, location: &LocationState) -> u8 {
    let city_match = !location.city.is_empty()
        && !candidate.city.is_empty()
        && normalize_ascii(&candidate.city) == normalize_ascii(&location.city);

    let region_match = region_matches(&candidate.region, &location.region);
    let country_match = country_matches(&candidate.country, &location.country);

    if city_match && region_match {
        0
    } else if region_match && country_match {
        1
    } else if country_match {
        2
    } else {
        3
    }
}

fn region_matches(offer_region: &str, user_region: &str) -> bool {
    let offer = canonical_region(offer_region);
    let user = canonical_region(user_region);

    if offer.is_empty() || user.is_empty() {
        return false;
    }

    offer == user
}

fn country_matches(offer_country: &str, user_country: &str) -> bool {
    let offer = canonical_country(offer_country);
    let user = canonical_country(user_country);

    if offer.is_empty() || user.is_empty() {
        return true;
    }

    offer == user
}

fn canonical_country(value: &str) -> String {
    let normalized = normalize_ascii(value);
    match normalized.as_str() {
        "us" | "usa" | "unitedstates" | "unitedstatesofamerica" => "us".to_string(),
        "ca" | "canada" => "ca".to_string(),
        "uk" | "gb" | "greatbritain" | "unitedkingdom" => "gb".to_string(),
        _ => normalized,
    }
}

fn canonical_region(value: &str) -> String {
    let normalized = normalize_ascii(value);
    match normalized.as_str() {
        "alabama" | "al" => "us-al".to_string(),
        "alaska" | "ak" => "us-ak".to_string(),
        "arizona" | "az" => "us-az".to_string(),
        "arkansas" | "ar" => "us-ar".to_string(),
        "california" | "ca" => "us-ca".to_string(),
        "colorado" | "co" => "us-co".to_string(),
        "connecticut" | "ct" => "us-ct".to_string(),
        "delaware" | "de" => "us-de".to_string(),
        "florida" | "fl" => "us-fl".to_string(),
        "georgia" | "ga" => "us-ga".to_string(),
        "hawaii" | "hi" => "us-hi".to_string(),
        "idaho" | "id" => "us-id".to_string(),
        "illinois" | "il" => "us-il".to_string(),
        "indiana" | "in" => "us-in".to_string(),
        "iowa" | "ia" => "us-ia".to_string(),
        "kansas" | "ks" => "us-ks".to_string(),
        "kentucky" | "ky" => "us-ky".to_string(),
        "louisiana" | "la" => "us-la".to_string(),
        "maine" | "me" => "us-me".to_string(),
        "maryland" | "md" => "us-md".to_string(),
        "massachusetts" | "ma" => "us-ma".to_string(),
        "michigan" | "mi" => "us-mi".to_string(),
        "minnesota" | "mn" => "us-mn".to_string(),
        "mississippi" | "ms" => "us-ms".to_string(),
        "missouri" | "mo" => "us-mo".to_string(),
        "montana" | "mt" => "us-mt".to_string(),
        "nebraska" | "ne" => "us-ne".to_string(),
        "nevada" | "nv" => "us-nv".to_string(),
        "newhampshire" | "nh" => "us-nh".to_string(),
        "newjersey" | "nj" => "us-nj".to_string(),
        "newmexico" | "nm" => "us-nm".to_string(),
        "newyork" | "ny" => "us-ny".to_string(),
        "northcarolina" | "nc" => "us-nc".to_string(),
        "northdakota" | "nd" => "us-nd".to_string(),
        "ohio" | "oh" => "us-oh".to_string(),
        "oklahoma" | "ok" => "us-ok".to_string(),
        "oregon" | "or" => "us-or".to_string(),
        "pennsylvania" | "pa" => "us-pa".to_string(),
        "rhodeisland" | "ri" => "us-ri".to_string(),
        "southcarolina" | "sc" => "us-sc".to_string(),
        "southdakota" | "sd" => "us-sd".to_string(),
        "tennessee" | "tn" => "us-tn".to_string(),
        "texas" | "tx" => "us-tx".to_string(),
        "utah" | "ut" => "us-ut".to_string(),
        "vermont" | "vt" => "us-vt".to_string(),
        "virginia" | "va" => "us-va".to_string(),
        "washington" | "wa" => "us-wa".to_string(),
        "westvirginia" | "wv" => "us-wv".to_string(),
        "wisconsin" | "wi" => "us-wi".to_string(),
        "wyoming" | "wy" => "us-wy".to_string(),
        "alberta" | "ab" => "ca-ab".to_string(),
        "britishcolumbia" | "bc" => "ca-bc".to_string(),
        "manitoba" | "mb" => "ca-mb".to_string(),
        "newbrunswick" | "nb" => "ca-nb".to_string(),
        "newfoundlandandlabrador" | "nl" => "ca-nl".to_string(),
        "novascotia" | "ns" => "ca-ns".to_string(),
        "ontario" | "on" => "ca-on".to_string(),
        "princeedwardisland" | "pe" => "ca-pe".to_string(),
        "quebec" | "qc" => "ca-qc".to_string(),
        "saskatchewan" | "sk" => "ca-sk".to_string(),
        "northwestterritories" | "nt" => "ca-nt".to_string(),
        "nunavut" | "nu" => "ca-nu".to_string(),
        "yukon" | "yt" => "ca-yt".to_string(),
        _ => normalized,
    }
}

fn normalize_ascii(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
}

fn gpu_preference_rank(gpu_name: &str) -> u8 {
    let normalized = gpu_name.to_ascii_lowercase();
    if normalized.contains("rtx") {
        0
    } else {
        1
    }
}

fn format_location_label(offer: &VastOffer) -> String {
    use crate::models::vast::region_to_code;

    let mut parts = Vec::new();
    if !offer.city.is_empty() {
        parts.push(offer.city.clone());
    }
    if !offer.region.is_empty() {
        // Convert full region name to code (e.g., "British Columbia" -> "BC")
        parts.push(region_to_code(&offer.region));
    }
    if !offer.country.is_empty() {
        parts.push(offer.country.clone());
    }

    if parts.is_empty() {
        "Unknown region".to_string()
    } else {
        parts.join(", ")
    }
}
