pub const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS apps (
    app_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    canonical_executable TEXT,
    desktop_entry_id TEXT,
    steam_app_id INTEGER,
    launcher_kind TEXT,
    aliases_json TEXT NOT NULL DEFAULT '[]',
    identity_confidence REAL NOT NULL,
    icon_path TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS app_sessions (
    session_id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    root_pid INTEGER NOT NULL,
    cgroup_path TEXT,
    source TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    identity_confidence REAL NOT NULL,
    FOREIGN KEY(app_id) REFERENCES apps(app_id)
);

CREATE INDEX IF NOT EXISTS idx_app_sessions_app_id ON app_sessions(app_id);
CREATE INDEX IF NOT EXISTS idx_app_sessions_root_pid ON app_sessions(root_pid);

CREATE TABLE IF NOT EXISTS session_pids (
    pid INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    ppid INTEGER,
    executable TEXT,
    attached_at INTEGER NOT NULL,
    FOREIGN KEY(session_id) REFERENCES app_sessions(session_id)
);

CREATE TABLE IF NOT EXISTS paths (
    path_id INTEGER PRIMARY KEY AUTOINCREMENT,
    canonical_path TEXT NOT NULL UNIQUE,
    logical_root TEXT,
    relative_path TEXT,
    file_type TEXT,
    inode INTEGER,
    mount_id INTEGER,
    size INTEGER,
    mtime_ns INTEGER,
    mode INTEGER,
    uid INTEGER,
    gid INTEGER,
    content_hash TEXT,
    last_scanned_at INTEGER
);

CREATE TABLE IF NOT EXISTS path_associations (
    path_id INTEGER NOT NULL,
    app_id TEXT NOT NULL,
    confidence REAL NOT NULL,
    persistence_class TEXT NOT NULL,
    semantic_role TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    PRIMARY KEY(path_id, app_id),
    FOREIGN KEY(path_id) REFERENCES paths(path_id),
    FOREIGN KEY(app_id) REFERENCES apps(app_id)
);

CREATE INDEX IF NOT EXISTS idx_path_assoc_app ON path_associations(app_id, confidence);

CREATE TABLE IF NOT EXISTS image_baseline (
    image_id TEXT NOT NULL,
    canonical_path TEXT NOT NULL,
    file_type TEXT,
    size INTEGER,
    mode INTEGER,
    package_owner TEXT,
    baseline_hash TEXT,
    PRIMARY KEY(image_id, canonical_path)
);

CREATE TABLE IF NOT EXISTS installer_transactions (
    transaction_id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    session_id TEXT,
    transaction_type TEXT NOT NULL,
    confidence REAL NOT NULL,
    candidate_roots_json TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER
);

CREATE TABLE IF NOT EXISTS dirty_apps (
    app_id TEXT PRIMARY KEY,
    first_dirty_at INTEGER NOT NULL,
    last_dirty_at INTEGER NOT NULL,
    requires_reconciliation INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS dirty_paths (
    app_id TEXT NOT NULL,
    path_id INTEGER NOT NULL,
    PRIMARY KEY(app_id, path_id)
);

CREATE TABLE IF NOT EXISTS bundle_commits (
    commit_id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    bundle_id TEXT NOT NULL,
    manifest_hash TEXT NOT NULL,
    cloud_path TEXT,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    committed_at INTEGER
);

CREATE TABLE IF NOT EXISTS checkpoints (
    checkpoint_id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    state TEXT NOT NULL,
    cloud_path TEXT,
    created_at INTEGER NOT NULL,
    committed_at INTEGER
);

CREATE TABLE IF NOT EXISTS sync_journal (
    operation_id TEXT NOT NULL,
    item_key TEXT NOT NULL,
    direction TEXT NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(operation_id, item_key)
);

CREATE TABLE IF NOT EXISTS restore_operations (
    restore_id TEXT PRIMARY KEY,
    bundle_id TEXT NOT NULL,
    app_id TEXT NOT NULL,
    state TEXT NOT NULL,
    staging_path TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_error TEXT
);

CREATE TABLE IF NOT EXISTS operations (
    operation_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    app_id TEXT,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_error TEXT,
    detail_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS known_roots (
    app_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    canonical_path TEXT NOT NULL,
    PRIMARY KEY(app_id, kind, canonical_path)
);

CREATE TABLE IF NOT EXISTS path_policies (
    canonical_path TEXT NOT NULL,
    app_id TEXT,
    policy TEXT NOT NULL,
    PRIMARY KEY(canonical_path, app_id)
);

CREATE TABLE IF NOT EXISTS seals (
    seal_id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    image_id TEXT NOT NULL,
    state TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    committed_at INTEGER
);

CREATE TABLE IF NOT EXISTS chunk_index (
    chunk_hash TEXT PRIMARY KEY,
    pack_id TEXT,
    size INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
"#;

/// Additive shared-storage optimization state. Existing v1 tables remain valid
/// and are intentionally not altered or copied during migration.
pub const MIGRATION_V2: &str = r#"
CREATE TABLE IF NOT EXISTS app_mutation_journal (
    mutation_id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    path TEXT NOT NULL,
    previous_path TEXT,
    kind TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    session_id TEXT,
    provenance_json TEXT,
    processed_at_ms INTEGER,
    FOREIGN KEY(app_id) REFERENCES apps(app_id)
);

CREATE INDEX IF NOT EXISTS idx_app_mutation_pending
    ON app_mutation_journal(app_id, processed_at_ms, observed_at_ms);
CREATE INDEX IF NOT EXISTS idx_app_mutation_session
    ON app_mutation_journal(session_id, observed_at_ms);

CREATE TABLE IF NOT EXISTS dirty_roots (
    app_id TEXT NOT NULL,
    canonical_root TEXT NOT NULL,
    logical_root TEXT,
    first_dirty_at_ms INTEGER NOT NULL,
    last_dirty_at_ms INTEGER NOT NULL,
    mutation_count INTEGER NOT NULL DEFAULT 1,
    requires_reconciliation INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(app_id, canonical_root),
    FOREIGN KEY(app_id) REFERENCES apps(app_id)
);

CREATE INDEX IF NOT EXISTS idx_dirty_roots_last_dirty
    ON dirty_roots(last_dirty_at_ms);

CREATE TABLE IF NOT EXISTS file_state_index (
    app_id TEXT NOT NULL,
    logical_root TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    canonical_path TEXT,
    file_type TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    inode INTEGER,
    mount_id INTEGER,
    mode INTEGER,
    content_hash TEXT,
    trust_state TEXT NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    last_hashed_at_ms INTEGER,
    PRIMARY KEY(app_id, logical_root, relative_path),
    FOREIGN KEY(app_id) REFERENCES apps(app_id)
);

CREATE INDEX IF NOT EXISTS idx_file_state_canonical
    ON file_state_index(canonical_path);
CREATE INDEX IF NOT EXISTS idx_file_state_hash
    ON file_state_index(content_hash, trust_state);

CREATE TABLE IF NOT EXISTS local_cas_index (
    object_kind TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    local_path TEXT NOT NULL,
    size INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    verified_at_ms INTEGER,
    last_accessed_at_ms INTEGER NOT NULL,
    PRIMARY KEY(object_kind, content_hash)
);

CREATE INDEX IF NOT EXISTS idx_local_cas_accessed
    ON local_cas_index(last_accessed_at_ms);

CREATE TABLE IF NOT EXISTS remote_content_index (
    storage_id TEXT NOT NULL,
    object_kind TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    remote_path TEXT NOT NULL,
    size INTEGER,
    etag TEXT,
    state TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER,
    PRIMARY KEY(storage_id, object_kind, content_hash)
);

CREATE INDEX IF NOT EXISTS idx_remote_content_path
    ON remote_content_index(storage_id, remote_path, observed_at_ms);
CREATE INDEX IF NOT EXISTS idx_remote_content_expiry
    ON remote_content_index(storage_id, state, expires_at_ms);

CREATE TABLE IF NOT EXISTS sync_journal_items (
    operation_id TEXT NOT NULL,
    item_key TEXT NOT NULL,
    item_kind TEXT NOT NULL,
    direction TEXT NOT NULL,
    state TEXT NOT NULL,
    local_path TEXT,
    remote_path TEXT,
    content_hash TEXT,
    size INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    bytes_transferred INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    completed_at_ms INTEGER,
    next_retry_at_ms INTEGER,
    detail_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(operation_id, item_key)
);

CREATE INDEX IF NOT EXISTS idx_sync_journal_items_state
    ON sync_journal_items(operation_id, state, updated_at_ms);
CREATE INDEX IF NOT EXISTS idx_sync_journal_items_retry
    ON sync_journal_items(state, next_retry_at_ms);

CREATE TABLE IF NOT EXISTS operation_runtime (
    operation_id TEXT PRIMARY KEY,
    progress_json TEXT,
    metrics_json TEXT,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(operation_id) REFERENCES operations(operation_id) ON DELETE CASCADE
);
"#;
