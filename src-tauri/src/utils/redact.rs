pub fn redact_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }

    if value.len() <= 8 {
        return "***".to_string();
    }

    let head = &value[..4];
    let tail = &value[value.len() - 4..];
    format!("{head}...{tail}")
}
