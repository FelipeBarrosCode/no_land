use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VastOffer {
    pub id: u64,
    pub host_id: Option<u64>,
    pub host_label: String,
    pub city: String,
    pub region: String,
    pub country: String,
    pub latitude: f64,
    pub longitude: f64,
    pub reliability: f64,
    pub gpu_name: String,
    pub gpu_ram_mb: u64,
    pub gpu_count: u32,
    pub cpu_name: String,
    pub cpu_cores: f64,
    pub internet_down_mbps: f64,
    pub internet_up_mbps: f64,
    pub hourly_price: f64,
    pub available_storage_gb: u32,
    pub raw_geolocation: String,
    pub time_remaining_hours: f64,
    pub is_verified: bool,
    pub is_datacenter: bool,
    pub offer_type: String,
    pub has_static_ip: bool,
    pub has_avx: bool,
}

impl VastOffer {
    pub fn from_value(value: &Value) -> Option<Self> {
        let id = value.get("id")?.as_u64()?;

        let geolocation = value
            .get("geolocation")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (city, region, country) = parse_geolocation(&geolocation);

        let latitude = value
            .get("latitude")
            .and_then(Value::as_f64)
            .or_else(|| value.get("lat").and_then(Value::as_f64))
            .unwrap_or_default();
        let longitude = value
            .get("longitude")
            .and_then(Value::as_f64)
            .or_else(|| value.get("lon").and_then(Value::as_f64))
            .unwrap_or_default();

        let host_label = if let Some(hostname) = value.get("hostname").and_then(Value::as_str) {
            hostname.to_string()
        } else if let Some(machine_id) = value.get("machine_id").and_then(Value::as_u64) {
            format!("Host-{machine_id}")
        } else {
            "Vast Host".to_string()
        };

        Some(Self {
            id,
            host_id: value.get("host_id").and_then(Value::as_u64),
            host_label,
            city,
            region,
            country,
            latitude,
            longitude,
            reliability: value
                .get("reliability")
                .or_else(|| value.get("reliability2"))
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            gpu_name: value
                .get("gpu_name")
                .and_then(Value::as_str)
                .unwrap_or("Unknown GPU")
                .to_string(),
            gpu_ram_mb: value
                .get("gpu_ram")
                .or_else(|| value.get("gpu_total_ram"))
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            gpu_count: value.get("num_gpus").and_then(Value::as_u64).unwrap_or(1) as u32,
            cpu_name: value
                .get("cpu_name")
                .or_else(|| value.get("cpu_model"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            cpu_cores: value
                .get("cpu_cores_effective")
                .or_else(|| value.get("cpu_cores"))
                .or_else(|| value.get("cpu_num_cores"))
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            internet_down_mbps: value
                .get("inet_down")
                .or_else(|| value.get("internet_down"))
                .or_else(|| value.get("dlperf"))
                .or_else(|| value.get("net_down"))
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            internet_up_mbps: value
                .get("inet_up")
                .or_else(|| value.get("internet_up"))
                .or_else(|| value.get("ulperf"))
                .or_else(|| value.get("net_up"))
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            hourly_price: value
                .get("dph_total")
                .or_else(|| value.get("discounted_dph_total"))
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            available_storage_gb: value
                .get("disk_space")
                .and_then(Value::as_f64)
                .unwrap_or(50.0)
                .round() as u32,
            raw_geolocation: geolocation,
            time_remaining_hours: parse_time_remaining(value),
            is_verified: value
                .get("verification")
                .and_then(Value::as_str)
                .map(|v| v.eq_ignore_ascii_case("verified"))
                .unwrap_or(false),
            is_datacenter: value
                .get("datacenter")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            offer_type: parse_offer_type(value),
            has_static_ip: value
                .get("static_ip")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            has_avx: value
                .get("has_avx")
                .and_then(Value::as_bool)
                .or_else(|| value.get("has_avx").and_then(Value::as_i64).map(|v| v != 0))
                .unwrap_or(false),
        })
    }
}

fn parse_offer_type(value: &Value) -> String {
    let normalized = ["type", "instance_type", "rental_type", "offer_type", "kind"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(|raw| raw.trim().to_ascii_lowercase())
        .filter(|raw| !raw.is_empty());

    if let Some(kind) = normalized {
        if kind.contains("interrupt") || kind.contains("bid") || kind == "spot" {
            return "interruptible".to_string();
        }
        if kind.contains("reserve") {
            return "reserved".to_string();
        }
        if kind.contains("demand") || kind == "ondemand" || kind == "on_demand" {
            return "on-demand".to_string();
        }
        return kind;
    }

    if value
        .get("is_bid")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "interruptible".to_string();
    }

    if value
        .get("reserved")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "reserved".to_string();
    }

    "on-demand".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VastInstance {
    pub id: u64,
    pub label: String,
    pub status: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub wireguard_port: u16,
    #[serde(default)]
    pub wireguard_host_ip: String,
    pub ssh_command: String,
    pub public_ip: String,
    pub gpu_name: String,
    #[serde(default)]
    pub image_runtype: String,
    #[serde(default)]
    pub hosting_type: String,
}

impl VastInstance {
    pub fn from_value(value: &Value) -> Option<Self> {
        Self::from_value_with_fallback_id(value, None)
    }

    pub fn from_value_with_fallback_id(value: &Value, fallback_id: Option<u64>) -> Option<Self> {
        let id = field_as_u64(value, &["id", "new_contract", "instance_id", "contract_id"])
            .or(fallback_id)?;

        let status = value
            .get("actual_status")
            .or_else(|| value.get("status"))
            .or_else(|| value.get("cur_state"))
            .and_then(Value::as_str)
            .unwrap_or("pending")
            .to_string();

        let public_ip = field_as_str(value, &["public_ipaddr", "public_ip", "publicIp"])
            .unwrap_or_default()
            .to_string();

        let ssh_host = field_as_str(value, &["ssh_host", "sshHostname", "ssh_hostname"])
            .filter(|host| !host.trim().is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| public_ip.clone());

        let ssh_port = field_as_u64(value, &["ssh_port", "sshPort"])
            .map(|port| port as u16)
            .filter(|port| *port > 0)
            .unwrap_or_else(|| extract_ssh_port_from_ports(value));
        let wireguard_port = extract_wireguard_port_from_ports(value);
        let wireguard_host_ip = extract_wireguard_host_ip_from_ports(value);

        let ssh_command = field_as_str(
            value,
            &[
                "ssh_command",
                "sshCommand",
                "connect_cmd",
                "connect_cmd_str",
            ],
        )
        .unwrap_or_default()
        .to_string();

        let image_runtype = field_as_str(value, &["image_runtype", "runtype", "runtime"])
            .unwrap_or_default()
            .to_string();

        let hosting_type = value
            .get("hosting_type")
            .map(|raw| {
                raw.as_str()
                    .map(ToString::to_string)
                    .or_else(|| raw.as_i64().map(|v| v.to_string()))
                    .or_else(|| raw.as_u64().map(|v| v.to_string()))
                    .or_else(|| raw.as_bool().map(|v| v.to_string()))
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        Some(Self {
            id,
            label: value
                .get("label")
                .or_else(|| value.get("hostname"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            status,
            ssh_host,
            ssh_port,
            wireguard_port,
            wireguard_host_ip,
            ssh_command,
            public_ip,
            gpu_name: value
                .get("gpu_name")
                .or_else(|| value.get("gpu_model"))
                .and_then(Value::as_str)
                .unwrap_or("Unknown GPU")
                .to_string(),
            image_runtype,
            hosting_type,
        })
    }

    pub fn ssh_ready(&self) -> bool {
        let status = self.status.to_ascii_lowercase();
        // Check for states that indicate the instance is ready for SSH
        ["running", "loaded", "started", "ready", "active"]
            .iter()
            .any(|state| status.contains(state))
    }

    /// Check if instance is still loading/booting (not yet ready but not failed)
    pub fn is_loading(&self) -> bool {
        let status = self.status.to_ascii_lowercase();
        [
            "loading",
            "creating",
            "initializing",
            "provisioning",
            "starting",
            "pending",
        ]
        .iter()
        .any(|state| status.contains(state))
    }

    pub fn is_vm_runtime(&self) -> bool {
        let runtime = self.image_runtype.trim().to_ascii_lowercase();
        runtime == "vm" || runtime == "kvm" || runtime == "qemu"
    }

    pub fn wireguard_endpoint_host(&self) -> String {
        if let Some(host) = normalize_host_ip(&self.wireguard_host_ip) {
            return host;
        }

        if let Some(host) = normalize_host_ip(&self.public_ip) {
            return host;
        }

        self.ssh_host.trim().to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VastSshKey {
    pub id: u64,
    pub key: String,
}

pub fn parse_geolocation(raw: &str) -> (String, String, String) {
    let mut segments = raw
        .split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    if segments.is_empty() {
        return (String::new(), String::new(), String::new());
    }

    let city = segments.remove(0).to_string();
    let region = segments.first().copied().unwrap_or_default().to_string();
    let country = segments.last().copied().unwrap_or_default().to_string();

    (city, region, country)
}

fn field_as_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(raw) = value.get(key) {
            if let Some(number) = raw.as_u64() {
                return Some(number);
            }

            if let Some(text) = raw.as_str() {
                if let Ok(parsed) = text.trim().parse::<u64>() {
                    return Some(parsed);
                }
            }
        }
    }

    None
}

fn field_as_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(raw) = value.get(key).and_then(Value::as_str) {
            return Some(raw);
        }
    }

    None
}

fn extract_ssh_port_from_ports(value: &Value) -> u16 {
    extract_port_from_ports(value, "22/tcp", 22)
}

fn extract_wireguard_port_from_ports(value: &Value) -> u16 {
    extract_port_from_ports(value, "51820/udp", 0)
}

fn extract_wireguard_host_ip_from_ports(value: &Value) -> String {
    extract_host_ip_from_ports(value, "51820/udp").unwrap_or_default()
}

fn extract_port_from_ports(value: &Value, key: &str, default: u16) -> u16 {
    if let Some(ports) = value.get("ports") {
        if let Some(ports_map) = ports.as_object() {
            if let Some(entry) = ports_map.get(key) {
                if let Some(port) = extract_host_port_from_entry(entry) {
                    return port;
                }
            }

            for (entry_key, entry) in ports_map {
                if !port_mapping_key_matches(entry_key, key) {
                    continue;
                }

                if let Some(port) = extract_host_port_from_entry(entry) {
                    return port;
                }
            }
        }

        if let Some(ports_array) = ports.as_array() {
            let (target_port, target_proto) = parse_port_key(key);

            for entry in ports_array {
                let Some(object) = entry.as_object() else {
                    continue;
                };

                let container_port = object
                    .get("Port")
                    .or_else(|| object.get("port"))
                    .or_else(|| object.get("container_port"))
                    .or_else(|| object.get("private_port"));

                let Some(container_port) = container_port else {
                    continue;
                };

                let Some(container_port) = parse_port_number(container_port) else {
                    continue;
                };

                let proto = object
                    .get("Protocol")
                    .or_else(|| object.get("protocol"))
                    .or_else(|| object.get("Proto"))
                    .or_else(|| object.get("proto"))
                    .and_then(Value::as_str)
                    .map(|raw| raw.trim().to_ascii_lowercase());

                if container_port != target_port {
                    continue;
                }

                if let Some(expected) = target_proto.as_deref() {
                    if let Some(actual) = proto.as_deref() {
                        if actual != expected {
                            continue;
                        }
                    }
                }

                if let Some(port) = object
                    .get("HostPort")
                    .or_else(|| object.get("host_port"))
                    .or_else(|| object.get("public_port"))
                    .and_then(parse_port_number)
                {
                    return port;
                }
            }
        }
    }

    default
}

fn extract_host_ip_from_ports(value: &Value, key: &str) -> Option<String> {
    if let Some(ports) = value.get("ports") {
        if let Some(ports_map) = ports.as_object() {
            if let Some(entry) = ports_map.get(key) {
                if let Some(ip) = extract_host_ip_from_entry(entry) {
                    return Some(ip);
                }
            }

            for (entry_key, entry) in ports_map {
                if !port_mapping_key_matches(entry_key, key) {
                    continue;
                }

                if let Some(ip) = extract_host_ip_from_entry(entry) {
                    return Some(ip);
                }
            }
        }

        if let Some(ports_array) = ports.as_array() {
            let (target_port, target_proto) = parse_port_key(key);

            for entry in ports_array {
                let Some(object) = entry.as_object() else {
                    continue;
                };

                let container_port = object
                    .get("Port")
                    .or_else(|| object.get("port"))
                    .or_else(|| object.get("container_port"))
                    .or_else(|| object.get("private_port"));

                let Some(container_port) = container_port else {
                    continue;
                };

                let Some(container_port) = parse_port_number(container_port) else {
                    continue;
                };

                let proto = object
                    .get("Protocol")
                    .or_else(|| object.get("protocol"))
                    .or_else(|| object.get("Proto"))
                    .or_else(|| object.get("proto"))
                    .and_then(Value::as_str)
                    .map(|raw| raw.trim().to_ascii_lowercase());

                if container_port != target_port {
                    continue;
                }

                if let Some(expected) = target_proto.as_deref() {
                    if let Some(actual) = proto.as_deref() {
                        if actual != expected {
                            continue;
                        }
                    }
                }

                if let Some(ip) = object
                    .get("HostIp")
                    .or_else(|| object.get("host_ip"))
                    .or_else(|| object.get("public_ip"))
                    .and_then(Value::as_str)
                    .and_then(normalize_host_ip)
                {
                    return Some(ip);
                }
            }
        }
    }

    None
}

fn extract_host_port_from_entry(entry: &Value) -> Option<u16> {
    if let Some(array) = entry.as_array() {
        for item in array {
            if let Some(port) = item.get("HostPort").and_then(parse_port_number) {
                return Some(port);
            }
            if let Some(port) = item.get("host_port").and_then(parse_port_number) {
                return Some(port);
            }
        }
    }

    if let Some(object) = entry.as_object() {
        if let Some(port) = object.get("HostPort").and_then(parse_port_number) {
            return Some(port);
        }
        if let Some(port) = object.get("host_port").and_then(parse_port_number) {
            return Some(port);
        }
    }

    parse_port_number(entry)
}

fn extract_host_ip_from_entry(entry: &Value) -> Option<String> {
    if let Some(array) = entry.as_array() {
        for item in array {
            if let Some(ip) = item
                .get("HostIp")
                .or_else(|| item.get("host_ip"))
                .or_else(|| item.get("public_ip"))
                .and_then(Value::as_str)
                .and_then(normalize_host_ip)
            {
                return Some(ip);
            }
        }
    }

    if let Some(object) = entry.as_object() {
        if let Some(ip) = object
            .get("HostIp")
            .or_else(|| object.get("host_ip"))
            .or_else(|| object.get("public_ip"))
            .and_then(Value::as_str)
            .and_then(normalize_host_ip)
        {
            return Some(ip);
        }
    }

    entry.as_str().and_then(normalize_host_ip)
}

fn port_mapping_key_matches(actual: &str, expected: &str) -> bool {
    let (actual_port, actual_proto) = parse_port_key(actual);
    let (expected_port, expected_proto) = parse_port_key(expected);

    if actual_port == 0 || expected_port == 0 || actual_port != expected_port {
        return false;
    }

    match (actual_proto.as_deref(), expected_proto.as_deref()) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

fn parse_port_key(key: &str) -> (u16, Option<String>) {
    let mut parts = key.split('/');
    let port = parts
        .next()
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or_default();
    let proto = parts.next().map(|raw| raw.trim().to_ascii_lowercase());
    (port, proto)
}

fn normalize_host_ip(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }

    let normalized = value.to_ascii_lowercase();
    if normalized == "0.0.0.0" || normalized == "::" || normalized == "[::]" {
        return None;
    }

    Some(value.to_string())
}

fn parse_port_number(raw: &Value) -> Option<u16> {
    if let Some(value) = raw.as_u64() {
        return Some(value as u16);
    }

    if let Some(value) = raw.as_str() {
        let first = value.split('/').next().unwrap_or(value);
        if let Ok(parsed) = first.trim().parse::<u16>() {
            return Some(parsed);
        }
    }

    None
}

fn parse_time_remaining(value: &Value) -> f64 {
    // Try duration field (seconds)
    if let Some(duration) = value.get("duration").and_then(Value::as_f64) {
        return duration / 3600.0; // Convert to hours
    }

    // Try time_remaining string parsing (e.g., "2d 5h 30m" or "5h 30m")
    if let Some(time_str) = value.get("time_remaining").and_then(Value::as_str) {
        return parse_time_string(time_str);
    }

    // Calculate from start_date + duration if available
    if let (Some(start), Some(duration)) = (
        value.get("start_date").and_then(Value::as_f64),
        value.get("duration").and_then(Value::as_f64),
    ) {
        let end_time = start + duration;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let remaining = end_time - now;
        if remaining > 0.0 {
            return remaining / 3600.0;
        }
    }

    0.0
}

fn parse_time_string(time_str: &str) -> f64 {
    let mut total_hours = 0.0;
    let normalized = time_str.to_lowercase();

    // Parse days (e.g., "2d" or "2 days")
    if let Some(days) = extract_number_before(&normalized, &["d", "day", "days"]) {
        total_hours += days * 24.0;
    }

    // Parse hours (e.g., "5h" or "5 hours")
    if let Some(hours) = extract_number_before(&normalized, &["h", "hr", "hour", "hours"]) {
        total_hours += hours;
    }

    // Parse minutes (e.g., "30m" or "30 minutes")
    if let Some(minutes) = extract_number_before(&normalized, &["m", "min", "minute", "minutes"]) {
        total_hours += minutes / 60.0;
    }

    total_hours
}

fn extract_number_before(text: &str, suffixes: &[&str]) -> Option<f64> {
    for suffix in suffixes {
        if let Some(pos) = text.find(suffix) {
            let before = &text[..pos];
            // Extract trailing number
            let num_str: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if let Ok(num) = num_str.parse::<f64>() {
                return Some(num);
            }
        }
    }
    None
}

/// Convert full region/state name to code (e.g., "British Columbia" -> "BC")
pub fn region_to_code(full_name: &str) -> String {
    let normalized = full_name.trim().to_ascii_lowercase();

    // Canadian provinces
    match normalized.as_str() {
        "british columbia" => "BC".to_string(),
        "alberta" => "AB".to_string(),
        "saskatchewan" => "SK".to_string(),
        "manitoba" => "MB".to_string(),
        "ontario" => "ON".to_string(),
        "quebec" => "QC".to_string(),
        "new brunswick" => "NB".to_string(),
        "nova scotia" => "NS".to_string(),
        "prince edward island" => "PE".to_string(),
        "newfoundland and labrador" => "NL".to_string(),
        "yukon" => "YT".to_string(),
        "northwest territories" => "NT".to_string(),
        "nunavut" => "NU".to_string(),
        // US states
        "alabama" => "AL".to_string(),
        "alaska" => "AK".to_string(),
        "arizona" => "AZ".to_string(),
        "arkansas" => "AR".to_string(),
        "california" => "CA".to_string(),
        "colorado" => "CO".to_string(),
        "connecticut" => "CT".to_string(),
        "delaware" => "DE".to_string(),
        "florida" => "FL".to_string(),
        "georgia" => "GA".to_string(),
        "hawaii" => "HI".to_string(),
        "idaho" => "ID".to_string(),
        "illinois" => "IL".to_string(),
        "indiana" => "IN".to_string(),
        "iowa" => "IA".to_string(),
        "kansas" => "KS".to_string(),
        "kentucky" => "KY".to_string(),
        "louisiana" => "LA".to_string(),
        "maine" => "ME".to_string(),
        "maryland" => "MD".to_string(),
        "massachusetts" => "MA".to_string(),
        "michigan" => "MI".to_string(),
        "minnesota" => "MN".to_string(),
        "mississippi" => "MS".to_string(),
        "missouri" => "MO".to_string(),
        "montana" => "MT".to_string(),
        "nebraska" => "NE".to_string(),
        "nevada" => "NV".to_string(),
        "new hampshire" => "NH".to_string(),
        "new jersey" => "NJ".to_string(),
        "new mexico" => "NM".to_string(),
        "new york" => "NY".to_string(),
        "north carolina" => "NC".to_string(),
        "north dakota" => "ND".to_string(),
        "ohio" => "OH".to_string(),
        "oklahoma" => "OK".to_string(),
        "oregon" => "OR".to_string(),
        "pennsylvania" => "PA".to_string(),
        "rhode island" => "RI".to_string(),
        "south carolina" => "SC".to_string(),
        "south dakota" => "SD".to_string(),
        "tennessee" => "TN".to_string(),
        "texas" => "TX".to_string(),
        "utah" => "UT".to_string(),
        "vermont" => "VT".to_string(),
        "virginia" => "VA".to_string(),
        "washington" => "WA".to_string(),
        "west virginia" => "WV".to_string(),
        "wisconsin" => "WI".to_string(),
        "wyoming" => "WY".to_string(),
        // If it's already short (2-3 chars), return as-is
        _ if normalized.len() <= 3 => full_name.to_ascii_uppercase(),
        // Otherwise return original
        _ => full_name.to_string(),
    }
}
