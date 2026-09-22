//! Temporary one-off verification tool: verify Drive-uploaded bundles by
//! mirroring them into LocalStorage and running the production restore
//! verification path (committed manifest + per-chunk content hashing).
//!
//! Usage:
//!   cargo run -p noland-state-agent --example verify_bundle -- \
//!     <mirror-root> <master-key-hex> <app_id> <bundle_id>

use noland_crypto::MasterKey;
use noland_restore::{download_and_verify_to, prepare_restore, DownloadOptions, RestoreTarget};
use noland_state_core::*;
use noland_storage::{read_committed_manifest, read_pack_index, LocalStorage};

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = &args[1];
    let key_hex = &args[2];
    let app_id_raw = &args[3];
    let bundle_id_raw = &args[4];

    let master = MasterKey::from_slice(&hex_bytes(key_hex)).expect("master key");
    let storage = LocalStorage::new(root);
    let app_id = AppId(app_id_raw.clone());
    let bundle_id: uuid::Uuid = bundle_id_raw.parse().expect("bundle id");

    let manifest = read_committed_manifest(&storage, &master, &app_id, bundle_id)
        .await
        .expect("committed manifest must verify");
    println!(
        "manifest OK  mode={:?} files={} logical_size={}",
        manifest.mode,
        manifest.files.len(),
        manifest.logical_size()
    );
    for file in manifest.files.iter().take(15) {
        println!("  file: {:?}", file.relative_path);
    }
    if manifest.files.len() > 15 {
        println!("  ... {} more", manifest.files.len() - 15);
    }

    let index = read_pack_index(&storage, &master, &app_id, bundle_id)
        .await
        .expect("pack index must decrypt");
    println!("index OK  entries={}", index.len());

    let paths = AgentPaths::from_roots(
        std::path::PathBuf::from(format!("{root}/_state")),
        std::path::PathBuf::from(format!("{root}/_run")),
    );
    paths.ensure_dirs().expect("agent dirs");
    let mode = match manifest.mode {
        BackupMode::PersonalState => RestoreMode::PersonalState,
        BackupMode::CompleteApplication => RestoreMode::CompleteApplication,
        BackupMode::Custom => RestoreMode::Custom,
    };
    let plan = prepare_restore(&storage, &master, &paths, &app_id, bundle_id, mode)
        .await
        .expect("restore plan");
    let report = download_and_verify_to(
        &storage,
        &master,
        &plan,
        &index,
        RestoreTarget::Complete,
        DownloadOptions::default(),
        None,
    )
    .await
    .expect("every chunk must verify");
    println!(
        "chunks OK packs_downloaded={} packs_reused={} chunks_extracted={} chunks_reused={}",
        report.packs_downloaded, report.packs_reused, report.chunks_extracted, report.chunks_reused
    );
    println!("VERIFIED {}", bundle_id);
}
