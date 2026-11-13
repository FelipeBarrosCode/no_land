-- This epoch distinguishes a rebuilt runtime DB from one whose generation was
-- already checkpointed. Counters and projections roll back with their writes.
CREATE TABLE learned_checkpoint_generation (
    singleton INTEGER PRIMARY KEY CHECK (singleton=1),
    database_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (typeof(generation)='integer' AND generation>=0)
);
INSERT INTO learned_checkpoint_generation VALUES (1, lower(hex(randomblob(16))), 0);

CREATE INDEX IF NOT EXISTS idx_bundle_commits_latest
    ON bundle_commits(app_id, state, committed_at DESC);
CREATE TABLE learned_checkpoint_latest_bundles (
    app_id TEXT PRIMARY KEY,
    commit_id TEXT NOT NULL,
    bundle_id TEXT NOT NULL
);
INSERT INTO learned_checkpoint_latest_bundles
SELECT app_id, commit_id, bundle_id FROM (
    SELECT app_id, commit_id, bundle_id,
        row_number() OVER (PARTITION BY app_id ORDER BY committed_at DESC, rowid DESC) AS rank
    FROM bundle_commits WHERE state='COMMITTED'
) WHERE rank=1;

CREATE TRIGGER learned_apps_insert AFTER INSERT ON apps BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;
CREATE TRIGGER learned_apps_delete AFTER DELETE ON apps BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;
CREATE TRIGGER learned_apps_update AFTER UPDATE ON apps
WHEN OLD.app_id IS NOT NEW.app_id OR OLD.display_name IS NOT NEW.display_name
    OR OLD.canonical_executable IS NOT NEW.canonical_executable
    OR OLD.desktop_entry_id IS NOT NEW.desktop_entry_id
    OR OLD.steam_app_id IS NOT NEW.steam_app_id OR OLD.launcher_kind IS NOT NEW.launcher_kind
    OR OLD.aliases_json IS NOT NEW.aliases_json
    OR OLD.identity_confidence IS NOT NEW.identity_confidence OR OLD.icon_path IS NOT NEW.icon_path
BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;

CREATE TRIGGER learned_associations_insert AFTER INSERT ON path_associations BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;
CREATE TRIGGER learned_associations_delete AFTER DELETE ON path_associations BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;
-- Compare exactly the ordered evidence-kind vector exported by read_checkpoint.
-- Details, session IDs and observation timestamps are intentionally excluded.
-- Triggers cover every writer, including scan CAS updates and restore batches.
CREATE TRIGGER learned_associations_update AFTER UPDATE ON path_associations
WHEN OLD.path_id IS NOT NEW.path_id OR OLD.app_id IS NOT NEW.app_id
    OR OLD.confidence IS NOT NEW.confidence
    OR OLD.persistence_class IS NOT NEW.persistence_class OR OLD.semantic_role IS NOT NEW.semantic_role
    OR (OLD.evidence_json IS NOT NEW.evidence_json AND
        (SELECT json_group_array(kind) FROM
            (SELECT json_extract(value, '$.kind') AS kind FROM json_each(OLD.evidence_json) ORDER BY key))
        IS NOT
        (SELECT json_group_array(kind) FROM
            (SELECT json_extract(value, '$.kind') AS kind FROM json_each(NEW.evidence_json) ORDER BY key)))
BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;

CREATE TRIGGER learned_paths_update AFTER UPDATE ON paths
WHEN (OLD.canonical_path IS NOT NEW.canonical_path OR OLD.logical_root IS NOT NEW.logical_root
    OR OLD.relative_path IS NOT NEW.relative_path OR OLD.path_id IS NOT NEW.path_id)
    AND EXISTS (SELECT 1 FROM path_associations WHERE path_id IN (OLD.path_id, NEW.path_id))
BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;

CREATE TRIGGER learned_roots_insert AFTER INSERT ON known_roots BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;
CREATE TRIGGER learned_roots_delete AFTER DELETE ON known_roots BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;
CREATE TRIGGER learned_roots_update AFTER UPDATE ON known_roots
WHEN OLD.app_id IS NOT NEW.app_id OR OLD.kind IS NOT NEW.kind OR OLD.canonical_path IS NOT NEW.canonical_path
BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;

-- The projection is maintained even for commits whose app is not present yet;
-- inserting that app later advances the generation through learned_apps_insert.
CREATE TRIGGER learned_latest_insert AFTER INSERT ON learned_checkpoint_latest_bundles
WHEN EXISTS (SELECT 1 FROM apps WHERE app_id=NEW.app_id) BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;
CREATE TRIGGER learned_latest_delete AFTER DELETE ON learned_checkpoint_latest_bundles
WHEN EXISTS (SELECT 1 FROM apps WHERE app_id=OLD.app_id) BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;
CREATE TRIGGER learned_latest_update AFTER UPDATE ON learned_checkpoint_latest_bundles
WHEN (OLD.commit_id IS NOT NEW.commit_id OR OLD.bundle_id IS NOT NEW.bundle_id)
    AND EXISTS (SELECT 1 FROM apps WHERE app_id=NEW.app_id) BEGIN
    UPDATE learned_checkpoint_generation SET generation=generation+1;
END;

CREATE TRIGGER learned_commits_insert AFTER INSERT ON bundle_commits
WHEN NEW.state='COMMITTED' BEGIN
    INSERT INTO learned_checkpoint_latest_bundles
    SELECT app_id, commit_id, bundle_id FROM bundle_commits
    WHERE app_id=NEW.app_id AND state='COMMITTED'
    ORDER BY committed_at DESC, rowid DESC LIMIT 1
    ON CONFLICT(app_id) DO UPDATE SET commit_id=excluded.commit_id, bundle_id=excluded.bundle_id
    WHERE commit_id IS NOT excluded.commit_id OR bundle_id IS NOT excluded.bundle_id;
END;
CREATE TRIGGER learned_commits_delete AFTER DELETE ON bundle_commits
WHEN OLD.state='COMMITTED' BEGIN
    INSERT INTO learned_checkpoint_latest_bundles
    SELECT app_id, commit_id, bundle_id FROM bundle_commits
    WHERE app_id=OLD.app_id AND state='COMMITTED'
    ORDER BY committed_at DESC, rowid DESC LIMIT 1
    ON CONFLICT(app_id) DO UPDATE SET commit_id=excluded.commit_id, bundle_id=excluded.bundle_id
    WHERE commit_id IS NOT excluded.commit_id OR bundle_id IS NOT excluded.bundle_id;
    DELETE FROM learned_checkpoint_latest_bundles WHERE app_id=OLD.app_id
        AND NOT EXISTS (SELECT 1 FROM bundle_commits WHERE app_id=OLD.app_id AND state='COMMITTED');
END;
CREATE TRIGGER learned_commits_update AFTER UPDATE ON bundle_commits
WHEN OLD.app_id IS NOT NEW.app_id OR OLD.commit_id IS NOT NEW.commit_id
    OR OLD.bundle_id IS NOT NEW.bundle_id OR OLD.state IS NOT NEW.state
    OR OLD.committed_at IS NOT NEW.committed_at OR OLD.rowid IS NOT NEW.rowid
BEGIN
    INSERT INTO learned_checkpoint_latest_bundles
    SELECT app_id, commit_id, bundle_id FROM bundle_commits
    WHERE app_id=OLD.app_id AND state='COMMITTED'
    ORDER BY committed_at DESC, rowid DESC LIMIT 1
    ON CONFLICT(app_id) DO UPDATE SET commit_id=excluded.commit_id, bundle_id=excluded.bundle_id
    WHERE commit_id IS NOT excluded.commit_id OR bundle_id IS NOT excluded.bundle_id;
    INSERT INTO learned_checkpoint_latest_bundles
    SELECT app_id, commit_id, bundle_id FROM bundle_commits
    WHERE app_id=NEW.app_id AND state='COMMITTED'
    ORDER BY committed_at DESC, rowid DESC LIMIT 1
    ON CONFLICT(app_id) DO UPDATE SET commit_id=excluded.commit_id, bundle_id=excluded.bundle_id
    WHERE commit_id IS NOT excluded.commit_id OR bundle_id IS NOT excluded.bundle_id;
    DELETE FROM learned_checkpoint_latest_bundles WHERE app_id IN (OLD.app_id, NEW.app_id)
        AND NOT EXISTS (SELECT 1 FROM bundle_commits
            WHERE app_id=learned_checkpoint_latest_bundles.app_id AND state='COMMITTED');
END;
