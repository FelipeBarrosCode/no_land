use std::collections::BTreeMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use noland_rclone_adapter::{
    session_from_input, AdapterCredential, AdapterInput, ProviderKind, TokenMode,
};

#[test]
fn gcs_compacts_service_account_json_without_changing_embedded_newlines() {
    let credentials = serde_json::json!({
        "type": "service_account",
        "private_key": "test key line one\ntest key line two\n",
    });
    let mut input = AdapterInput {
        provider: ProviderKind::GoogleCloudStorage,
        remote_name: "store".into(),
        credentials: AdapterCredential::ServiceAccount {
            json: serde_json::to_string_pretty(&credentials).unwrap(),
        },
        fields: BTreeMap::new(),
        bucket: Some("test-bucket".into()),
        prefix: None,
    };
    let session = session_from_input(&input, "gcs", TokenMode::Operation).unwrap();
    let json = session
        .config_ini
        .lines()
        .find_map(|line| line.strip_prefix("service_account_credentials = "))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(json).unwrap(),
        credentials
    );
    input.credentials = AdapterCredential::ServiceAccount { json: "{".into() };
    assert!(session_from_input(&input, "gcs", TokenMode::Operation).is_err());
}

#[test]
fn sftp_key_auth_escapes_pem_and_obscures_optional_passphrases() {
    for passphrase in [
        None,
        Some(""),
        Some("test passphrase longer than one AES block"),
    ] {
        let input = AdapterInput {
            provider: ProviderKind::Sftp,
            remote_name: "store".into(),
            credentials: AdapterCredential::SshKey {
                username: "test-user".into(),
                private_key: "test key line one\r\ntest key line two\n".into(),
                passphrase: passphrase.map(str::to_string),
            },
            fields: BTreeMap::from([
                ("host".into(), "sftp.example.test".into()),
                ("port".into(), "2222".into()),
                ("host_keys".into(), "test-host-key".into()),
            ]),
            bucket: None,
            prefix: Some("/".into()),
        };
        let session = session_from_input(&input, "ssh-key", TokenMode::Operation).unwrap();
        assert_eq!(session.root, "/");
        let entries: BTreeMap<_, _> = session
            .config_ini
            .lines()
            .filter_map(|line| line.split_once(" = "))
            .collect();
        assert_eq!(
            entries["key_pem"],
            "test key line one\\ntest key line two\\n"
        );
        assert_eq!(entries["user"], "test-user");
        assert_eq!(entries["host"], "sftp.example.test");
        assert_eq!(entries["port"], "2222");
        assert_eq!(entries["host_keys"], "test-host-key");
        assert!(!entries.contains_key("pass"));
        if let Some(passphrase) = passphrase.filter(|value| !value.is_empty()) {
            let obscured = entries["key_file_pass"];
            assert_ne!(obscured, passphrase);
            assert_eq!(
                URL_SAFE_NO_PAD.decode(obscured).unwrap().len(),
                16 + passphrase.len()
            );
        } else {
            assert!(!entries.contains_key("key_file_pass"));
        }
    }
}
