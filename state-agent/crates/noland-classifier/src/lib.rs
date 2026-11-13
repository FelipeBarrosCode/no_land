//! Two-dimensional persistence + semantic classification.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use noland_state_core::*;
use noland_state_db::StateDb;

pub struct Classifier<'a> {
    pub db: &'a StateDb,
    pub image_id: String,
    agent_paths: Option<AgentPaths>,
    target_home: Option<std::path::PathBuf>,
}

type PathInputs = (Option<(Option<i64>, Option<String>)>, usize, Option<String>);

/// An explicit, operation-local snapshot. Drop it after planning; the next pass
/// observes new roots, baseline entries, ownership and policies. No cache lives
/// on Classifier, so a long-lived classifier cannot silently retain stale state.
pub struct ClassifierPass<'classifier, 'db> {
    classifier: &'classifier Classifier<'db>,
    app_id: AppId,
    roots: HashMap<PathBuf, String>,
    inputs: HashMap<i64, PathInputs>,
}

impl<'a> Classifier<'a> {
    pub fn new(db: &'a StateDb, image_id: impl Into<String>) -> Self {
        Self {
            db,
            image_id: image_id.into(),
            agent_paths: None,
            target_home: None,
        }
    }

    pub fn with_exclusion_context(
        mut self,
        agent_paths: AgentPaths,
        target_home: impl Into<std::path::PathBuf>,
    ) -> Self {
        self.agent_paths = Some(agent_paths);
        self.target_home = Some(target_home.into());
        self
    }

    pub fn begin_pass(&self, app_id: &AppId, path_ids: &[i64]) -> Result<ClassifierPass<'_, 'a>> {
        let mut roots = HashMap::new();
        for (_, kind, root) in self.db.known_roots(Some(app_id))? {
            let root = PathBuf::from(root);
            if !is_valid_index_path(&root) {
                continue;
            }
            // Explicit state roots win ties over install roots, regardless of
            // SQLite row order. A more specific nested root always wins.
            let existing = roots.entry(root).or_insert_with(|| kind.clone());
            if matches!(kind.as_str(), "state" | "user-state" | "config" | "save") {
                *existing = kind;
            }
        }
        Ok(ClassifierPass {
            classifier: self,
            app_id: app_id.clone(),
            roots,
            inputs: self
                .db
                .classification_inputs(app_id, &self.image_id, path_ids)?,
        })
    }

    fn excluded_from_tracking(&self, path: &Path, in_known_app_root: bool) -> bool {
        self.agent_paths
            .as_ref()
            .is_some_and(|agent_paths| agent_paths.is_internal(path))
            || is_tracking_excluded(path, in_known_app_root, self.target_home.as_deref())
    }

    pub fn classify_path(
        &self,
        record: &PathRecord,
        assoc: &PathAssociation,
    ) -> Result<(PersistenceClass, SemanticRole)> {
        self.begin_pass(&assoc.app_id, &[record.path_id])?
            .classify_path(record, assoc)
    }

    pub fn reclassify_app(&self, app_id: &AppId) -> Result<usize> {
        let rows = self.db.associations_for_app(app_id)?;
        let ids = rows
            .iter()
            .map(|(record, _)| record.path_id)
            .collect::<Vec<_>>();
        let pass = self.begin_pass(app_id, &ids)?;
        let mut updates = Vec::with_capacity(512);
        let mut changed = 0;
        for (record, assoc) in rows {
            let (class, role) = pass.classify_path(&record, &assoc)?;
            if class != assoc.persistence_class || role != assoc.semantic_role {
                updates.push((assoc, class, role));
                if updates.len() == 512 {
                    changed += self.db.update_classifications(&updates)?;
                    updates.clear();
                }
            }
        }
        changed += self.db.update_classifications(&updates)?;
        Ok(changed)
    }

    pub fn decide(
        &self,
        record: &PathRecord,
        assoc: &PathAssociation,
        mode: BackupMode,
    ) -> Result<BackupDecision> {
        self.begin_pass(&assoc.app_id, &[record.path_id])?
            .decide(record, assoc, mode)
    }
}

impl ClassifierPass<'_, '_> {
    fn known_root(&self, path: &Path) -> Option<(&Path, &str)> {
        path.ancestors().find_map(|ancestor| {
            self.roots
                .get_key_value(ancestor)
                .map(|(root, kind)| (root.as_path(), kind.as_str()))
        })
    }

    fn inputs(&self, record: &PathRecord, assoc: &PathAssociation) -> Result<&PathInputs> {
        if assoc.app_id != self.app_id || record.path_id != assoc.path_id {
            return Err(StateError::Invalid(
                "classification pass app/path mismatch".into(),
            ));
        }
        self.inputs.get(&record.path_id).ok_or_else(|| {
            StateError::Invalid(format!(
                "path {} was not loaded into this classification pass",
                record.path_id
            ))
        })
    }

    pub fn classify_path(
        &self,
        record: &PathRecord,
        assoc: &PathAssociation,
    ) -> Result<(PersistenceClass, SemanticRole)> {
        let (baseline, owner_count, _) = self.inputs(record, assoc)?;
        let path = Path::new(&record.canonical_path);
        let known_root = self.known_root(path);
        let in_known_app_root = known_root.is_some();
        if !is_valid_index_path(path) {
            return Err(StateError::UnsafePath(record.canonical_path.clone()));
        }
        if self
            .classifier
            .agent_paths
            .as_ref()
            .is_some_and(|agent_paths| agent_paths.is_internal(path))
            || is_noland_internal_for_home(path, self.classifier.target_home.as_deref())
            || is_hard_volatile_root(path)
            || looks_like_lock_or_socket(path)
        {
            return Ok((PersistenceClass::Ephemeral, SemanticRole::Temp));
        }
        if is_base_system_path(path) && !in_known_app_root {
            return Ok((PersistenceClass::BaseImage, SemanticRole::Os));
        }
        if looks_like_cache(path) {
            return Ok((
                if in_known_app_root
                    || assoc.evidence.iter().any(|e| {
                        matches!(
                            e.kind,
                            EvidenceKind::KnownAppRoot | EvidenceKind::ExplicitUserBinding
                        )
                    })
                {
                    PersistenceClass::ReconstructableApp
                } else {
                    PersistenceClass::Ephemeral
                },
                SemanticRole::Cache,
            ));
        }
        if looks_like_secret(path) {
            return Ok((PersistenceClass::PersistentState, SemanticRole::Secret));
        }

        let baseline_hit = baseline_matches(baseline, record.size);
        let packaged = baseline.as_ref().is_some_and(|(_, owner)| owner.is_some());
        let shared = *owner_count > 1;
        let has_mutation = assoc.evidence.iter().any(|e| e.kind.is_mutation());
        let explicit_state = assoc.evidence.iter().any(|e| {
            e.kind == EvidenceKind::KnownUserStateRoot
                || (e.kind == EvidenceKind::ExplicitUserBinding && !is_restored_app_content(assoc))
        });
        let install_content = known_root.is_some_and(|(root, kind)| {
            kind == "install" && !looks_like_user_state_in_install(path, root)
        });
        let explicit_root_overrides_image =
            in_known_app_root && (baseline_hit || is_base_system_path(path));
        let class = if explicit_state {
            PersistenceClass::PersistentState
        } else if install_content {
            // Installation and reconciliation observations are not save-game
            // writes. Assets remain app content even in an XDG runtime tree.
            PersistenceClass::ReconstructableApp
        } else if known_root.is_some_and(|(root, kind)| {
            matches!(kind, "state" | "user-state" | "config" | "save")
                || (kind == "install" && looks_like_user_state_in_install(path, root))
        }) {
            PersistenceClass::PersistentState
        } else if explicit_root_overrides_image
            && known_root.is_some_and(|(_, kind)| kind == "proton")
            && !has_mutation
            && !looks_like_user_state(path)
        {
            PersistenceClass::ReconstructableApp
        } else if explicit_root_overrides_image {
            PersistenceClass::PersistentState
        } else if baseline_hit && !has_mutation {
            PersistenceClass::BaseImage
        } else if baseline_hit && has_mutation {
            PersistenceClass::PersistentState
        } else if packaged && !has_mutation {
            PersistenceClass::ReconstructableApp
        } else if shared {
            PersistenceClass::SharedState
        } else if looks_like_user_state(path)
            || assoc.confidence >= OWNERSHIP_STRONG
            || assoc.evidence.iter().any(|e| e.kind.is_mutation())
        {
            PersistenceClass::PersistentState
        } else if looks_like_os_or_lib(path) {
            PersistenceClass::BaseImage
        } else {
            PersistenceClass::Unknown
        };

        let role = infer_semantic_role(path, class);
        Ok((class, role))
    }

    pub fn decide(
        &self,
        record: &PathRecord,
        assoc: &PathAssociation,
        mode: BackupMode,
    ) -> Result<BackupDecision> {
        self.classify_and_decide(record, &mut assoc.clone(), mode)
    }

    /// Planning already owns its association. Classify once and borrow its
    /// evidence for the policy decision instead of cloning it for every file.
    pub fn classify_and_decide(
        &self,
        record: &PathRecord,
        assoc: &mut PathAssociation,
        mode: BackupMode,
    ) -> Result<BackupDecision> {
        let path = Path::new(&record.canonical_path);
        if !is_valid_index_path(path) {
            return Err(StateError::UnsafePath(record.canonical_path.clone()));
        }
        let (baseline, _, policy) = self.inputs(record, assoc)?;
        let in_known_app_root = self.known_root(path).is_some();
        (assoc.persistence_class, assoc.semantic_role) = self.classify_path(record, assoc)?;
        if self
            .classifier
            .excluded_from_tracking(path, in_known_app_root)
        {
            return Ok(BackupDecision::Exclude);
        }
        let temporary_root = in_known_app_root
            && !assoc
                .evidence
                .iter()
                .any(|evidence| evidence.kind == EvidenceKind::KnownAppRoot);
        if temporary_root {
            assoc
                .evidence
                .push(Evidence::new(EvidenceKind::KnownAppRoot));
        }
        let ctx = PathDecisionContext {
            path,
            association: assoc,
            mode,
            matches_image_baseline: baseline_matches(baseline, record.size),
            reliable_reconstruction: baseline.as_ref().is_some_and(|(_, owner)| owner.is_some())
                || record.canonical_path.contains("/steamapps/common/"),
            policy_override: policy.as_deref().and_then(parse_policy),
        };
        let decision = decide_path(ctx);
        if temporary_root {
            assoc.evidence.pop();
        }
        Ok(decision)
    }
}

fn baseline_matches(baseline: &Option<(Option<i64>, Option<String>)>, size: Option<i64>) -> bool {
    baseline
        .as_ref()
        .is_some_and(|(expected, _)| match (*expected, size) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => true,
        })
}

fn parse_policy(raw: &str) -> Option<PathPolicy> {
    Some(match raw {
        "include" => PathPolicy::Include,
        "exclude" => PathPolicy::Exclude,
        "cache" => PathPolicy::Cache,
        "secret" => PathPolicy::Secret,
        "shared" => PathPolicy::Shared,
        "force-persistent" => PathPolicy::ForcePersistent,
        "force-reconstructable" => PathPolicy::ForceReconstructable,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn input(db: &StateDb, app_id: &AppId, path: &str) -> (PathRecord, PathAssociation) {
        let path_id = db.upsert_path(path).unwrap();
        let assoc = PathAssociation {
            app_id: app_id.clone(),
            path_id,
            confidence: CONF_DEPENDENCY,
            evidence: vec![Evidence::new(EvidenceKind::ReadOnlyDependency)],
            persistence_class: PersistenceClass::Unknown,
            semantic_role: SemanticRole::Unknown,
            first_seen_at: Utc::now(),
            last_seen_at: Utc::now(),
        };
        db.upsert_association(&assoc).unwrap();
        (db.get_path_by_id(path_id).unwrap().unwrap(), assoc)
    }

    #[test]
    fn deeply_nested_runtime_install_assets_remain_content_after_reclassification() {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppIdentity::new(AppId::desktop("hookshot"), "Hookshot");
        db.upsert_app(&app).unwrap();
        let root = "/home/user/.local/share/noland-runtime/app-data/123/Games/Hookshot";
        db.add_known_root(&app.app_id, "install", root).unwrap();
        let classifier = Classifier::new(&db, "img");
        let mut fixtures = Vec::new();
        for (relative, class, role) in [
            (
                "romfs/a/b/c/d/e/f/g/asset.bin",
                PersistenceClass::ReconstructableApp,
                SemanticRole::AppContent,
            ),
            (
                "romfs/Rock_LonLon_SecretRoomArch_A_Alb.bntx.zs",
                PersistenceClass::ReconstructableApp,
                SemanticRole::AppContent,
            ),
            (
                "user/save/slot_00/progress.sav",
                PersistenceClass::PersistentState,
                SemanticRole::UserState,
            ),
            (
                "config/settings.json",
                PersistenceClass::PersistentState,
                SemanticRole::UserState,
            ),
            (
                "mods/custom.bin",
                PersistenceClass::PersistentState,
                SemanticRole::UserState,
            ),
            (
                "hookshot_options.ini",
                PersistenceClass::PersistentState,
                SemanticRole::UserState,
            ),
            (
                "hookshot_programs.bin",
                PersistenceClass::ReconstructableApp,
                SemanticRole::Cache,
            ),
            (
                "credentials.json",
                PersistenceClass::PersistentState,
                SemanticRole::Secret,
            ),
        ] {
            let (record, mut assoc) = input(&db, &app.app_id, &format!("{root}/{relative}"));
            assoc.confidence = CONF_EXPLICIT;
            assoc
                .evidence
                .push(Evidence::new(EvidenceKind::ReconciliationDelta));
            assoc
                .evidence
                .push(Evidence::new(EvidenceKind::InstallerTransaction));
            db.upsert_association(&assoc).unwrap();
            fixtures.push((record, assoc, class, role));
        }
        let ids = fixtures
            .iter()
            .map(|(record, ..)| record.path_id)
            .collect::<Vec<_>>();
        let pass = classifier.begin_pass(&app.app_id, &ids).unwrap();
        for (record, assoc, class, role) in &fixtures {
            assert_eq!(
                pass.classify_path(record, assoc).unwrap(),
                (*class, *role),
                "{}",
                record.canonical_path
            );
            let personal = pass
                .decide(record, assoc, BackupMode::PersonalState)
                .unwrap();
            let complete = pass
                .decide(record, assoc, BackupMode::CompleteApplication)
                .unwrap();
            if *class == PersistenceClass::ReconstructableApp {
                assert_eq!(personal, BackupDecision::MetadataOnly);
                assert_eq!(complete, BackupDecision::IncludeAsBaseOrOverlay);
            } else {
                assert!(matches!(
                    personal,
                    BackupDecision::Include | BackupDecision::EncryptedInclude
                ));
            }
        }
        assert!(classifier.reclassify_app(&app.app_id).unwrap() > 0);
        assert_eq!(classifier.reclassify_app(&app.app_id).unwrap(), 0);
    }

    #[test]
    fn explicit_bindings_and_nested_state_roots_override_install_content() {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppIdentity::new(AppId::desktop("game"), "Game");
        db.upsert_app(&app).unwrap();
        db.add_known_root(&app.app_id, "install", "/srv/Game")
            .unwrap();
        db.add_known_root(&app.app_id, "state", "/srv/Game/custom-data")
            .unwrap();
        let (record, assoc) = input(&db, &app.app_id, "/srv/Game/custom-data/arbitrary.bin");
        let classifier = Classifier::new(&db, "img");
        assert_eq!(
            classifier.classify_path(&record, &assoc).unwrap(),
            (PersistenceClass::PersistentState, SemanticRole::UserState)
        );
        let (record, mut assoc) = input(&db, &app.app_id, "/srv/Game/data.bin");
        assoc
            .evidence
            .push(Evidence::new(EvidenceKind::ExplicitUserBinding));
        assert_eq!(
            classifier.classify_path(&record, &assoc).unwrap(),
            (PersistenceClass::PersistentState, SemanticRole::UserState)
        );
    }

    #[test]
    fn restored_install_content_keeps_content_labels_and_scope_evidence() {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppIdentity::new(AppId::desktop("restored"), "Restored");
        db.upsert_app(&app).unwrap();
        db.add_known_root(&app.app_id, "install", "/srv/Hookshot")
            .unwrap();
        let classifier = Classifier::new(&db, "img");
        for (name, role) in [
            ("asset.bin", SemanticRole::AppContent),
            ("hookshot_programs.bin", SemanticRole::Cache),
        ] {
            let (record, mut assoc) = input(&db, &app.app_id, &format!("/srv/Hookshot/{name}"));
            assoc.evidence.extend([
                Evidence::new(EvidenceKind::RestoredFromCommittedBundle),
                Evidence::new(EvidenceKind::ExplicitUserBinding),
            ]);
            assoc.persistence_class = PersistenceClass::ReconstructableApp;
            assoc.semantic_role = role;
            assert_eq!(
                classifier.classify_path(&record, &assoc).unwrap(),
                (PersistenceClass::ReconstructableApp, role)
            );
            assert_eq!(
                classifier
                    .decide(&record, &assoc, BackupMode::PersonalState)
                    .unwrap(),
                BackupDecision::MetadataOnly
            );
        }
    }

    #[test]
    fn fresh_passes_observe_roots_baselines_policies_and_shared_ownership() {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppIdentity::new(AppId::desktop("game"), "Game");
        db.upsert_app(&app).unwrap();
        let classifier = Classifier::new(&db, "img");
        let (record, assoc) = input(&db, &app.app_id, "/srv/Game/data.bin");
        let before_root = classifier
            .begin_pass(&app.app_id, &[record.path_id])
            .unwrap();
        db.add_known_root(&app.app_id, "install", "/srv/Game")
            .unwrap();
        assert_eq!(
            before_root.classify_path(&record, &assoc).unwrap().0,
            PersistenceClass::Unknown
        );
        assert_eq!(
            classifier.classify_path(&record, &assoc).unwrap().0,
            PersistenceClass::ReconstructableApp
        );
        let before_policy = classifier
            .begin_pass(&app.app_id, &[record.path_id])
            .unwrap();
        db.set_path_policy(&record.canonical_path, Some(&app.app_id), "exclude")
            .unwrap();
        assert_eq!(
            before_policy
                .decide(&record, &assoc, BackupMode::CompleteApplication)
                .unwrap(),
            BackupDecision::IncludeAsBaseOrOverlay
        );
        assert_eq!(
            classifier
                .decide(&record, &assoc, BackupMode::CompleteApplication)
                .unwrap(),
            BackupDecision::Exclude
        );

        let (record, assoc) = input(&db, &app.app_id, "/srv/baseline.bin");
        let before_baseline = classifier
            .begin_pass(&app.app_id, &[record.path_id])
            .unwrap();
        db.insert_baseline(
            "img",
            &record.canonical_path,
            Some("file"),
            Some(100),
            None,
            Some("package"),
            None,
        )
        .unwrap();
        assert_eq!(
            before_baseline.classify_path(&record, &assoc).unwrap().0,
            PersistenceClass::Unknown
        );
        assert_eq!(
            classifier.classify_path(&record, &assoc).unwrap().0,
            PersistenceClass::BaseImage
        );

        let (record, mut assoc) = input(&db, &app.app_id, "/srv/shared.bin");
        assoc.confidence = CONF_EXPLICIT;
        db.upsert_association(&assoc).unwrap();
        let before_shared = classifier
            .begin_pass(&app.app_id, &[record.path_id])
            .unwrap();
        let other = AppIdentity::new(AppId::desktop("other"), "Other");
        db.upsert_app(&other).unwrap();
        let mut other_assoc = assoc.clone();
        other_assoc.app_id = other.app_id;
        db.upsert_association(&other_assoc).unwrap();
        assert_eq!(
            before_shared.classify_path(&record, &assoc).unwrap().0,
            PersistenceClass::PersistentState
        );
        assert_eq!(
            classifier.classify_path(&record, &assoc).unwrap().0,
            PersistenceClass::SharedState
        );
        assert!(before_shared.classify_path(&record, &other_assoc).is_err());
    }

    #[test]
    fn malformed_classification_paths_cannot_escape_install_policy() {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppIdentity::new(AppId::desktop("game"), "Game");
        db.upsert_app(&app).unwrap();
        db.add_known_root(&app.app_id, "install", "/srv/Game")
            .unwrap();
        let classifier = Classifier::new(&db, "img");
        for path in ["/srv/Game/../other/data.bin", "relative/data.bin"] {
            let (record, assoc) = input(&db, &app.app_id, path);
            assert!(classifier.classify_path(&record, &assoc).is_err());
            assert!(classifier
                .decide(&record, &assoc, BackupMode::CompleteApplication)
                .is_err());
        }
    }

    #[test]
    fn dated_access_logs_are_not_persistent_state_even_with_write_evidence() {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppIdentity::new(AppId::desktop("server"), "Server");
        db.upsert_app(&app).unwrap();
        db.add_known_root(&app.app_id, "install", "/opt/apache-tomcat-9.0.112")
            .unwrap();
        let classifier = Classifier::new(&db, "img");
        for (path, excluded) in [
            (
                "/opt/apache-tomcat-9.0.112/logs/localhost_access_log.2026-11-09.txt",
                true,
            ),
            ("/opt/apache-tomcat-9.0.112/logs/catalina.out", true),
            ("/opt/apache-tomcat-9.0.112/logs/important-save.dat", false),
            (
                "/home/gamer/.local/share/Hookshot/user/save/slot_00/progress.sav",
                false,
            ),
        ] {
            let path_id = db.upsert_path(path).unwrap();
            let record = db.get_path_by_id(path_id).unwrap().unwrap();
            let association = PathAssociation {
                app_id: app.app_id.clone(),
                path_id,
                confidence: CONF_EXPLICIT,
                evidence: vec![Evidence::new(EvidenceKind::DirectCgroupWrite)],
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
            };
            for mode in [BackupMode::PersonalState, BackupMode::CompleteApplication] {
                let decision = classifier.decide(&record, &association, mode).unwrap();
                assert_eq!(
                    decision == BackupDecision::Exclude,
                    excluded && mode == BackupMode::PersonalState,
                    "{path}: {mode:?}"
                );
            }
        }
    }

    #[test]
    fn excludes_unchanged_os_and_includes_saves() {
        let db = StateDb::open_in_memory().unwrap();
        db.insert_baseline(
            "img",
            "/usr/lib/libc.so.6",
            Some("file"),
            Some(100),
            None,
            Some("libc6"),
            None,
        )
        .unwrap();
        let app = AppIdentity::new(AppId::desktop("game"), "Game");
        db.upsert_app(&app).unwrap();
        let libc_id = db.upsert_path("/usr/lib/libc.so.6").unwrap();
        let save_id = db
            .upsert_path("/home/gamer/.local/share/game/save.dat")
            .unwrap();
        let now = Utc::now();
        let libc_assoc = PathAssociation {
            app_id: app.app_id.clone(),
            path_id: libc_id,
            confidence: CONF_DEPENDENCY,
            evidence: vec![Evidence::new(EvidenceKind::ReadOnlyDependency)],
            persistence_class: PersistenceClass::Unknown,
            semantic_role: SemanticRole::Unknown,
            first_seen_at: now,
            last_seen_at: now,
        };
        let save_assoc = PathAssociation {
            app_id: app.app_id.clone(),
            path_id: save_id,
            confidence: CONF_DIRECT_OUTSIDE_ROOT,
            evidence: vec![
                Evidence::new(EvidenceKind::DirectCgroupWrite),
                Evidence::new(EvidenceKind::KnownUserStateRoot),
            ],
            persistence_class: PersistenceClass::Unknown,
            semantic_role: SemanticRole::Unknown,
            first_seen_at: now,
            last_seen_at: now,
        };
        db.upsert_association(&libc_assoc).unwrap();
        db.upsert_association(&save_assoc).unwrap();
        let clf = Classifier::new(&db, "img");
        let libc_rec = db
            .get_path_by_canonical("/usr/lib/libc.so.6")
            .unwrap()
            .unwrap();
        let save_rec = db
            .get_path_by_canonical("/home/gamer/.local/share/game/save.dat")
            .unwrap()
            .unwrap();
        let (c1, _) = clf.classify_path(&libc_rec, &libc_assoc).unwrap();
        let (c2, r2) = clf.classify_path(&save_rec, &save_assoc).unwrap();
        assert_eq!(c1, PersistenceClass::BaseImage);
        assert_eq!(c2, PersistenceClass::PersistentState);
        assert_eq!(r2, SemanticRole::UserState);
        assert_eq!(
            clf.decide(&libc_rec, &libc_assoc, BackupMode::PersonalState)
                .unwrap(),
            BackupDecision::Exclude
        );
        assert_eq!(
            clf.decide(&save_rec, &save_assoc, BackupMode::PersonalState)
                .unwrap(),
            BackupDecision::Include
        );
    }

    #[test]
    fn explicit_app_root_overrides_base_system_but_not_noland_exclusions() {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppIdentity::new(AppId::desktop("game"), "Game");
        db.upsert_app(&app).unwrap();
        db.add_known_root(&app.app_id, "install", "/usr/local/share/game")
            .unwrap();
        db.add_known_root(&app.app_id, "install", "/opt/noland/game")
            .unwrap();
        let now = Utc::now();
        let association_for = |path_id| PathAssociation {
            app_id: app.app_id.clone(),
            path_id,
            confidence: CONF_DEPENDENCY,
            evidence: vec![Evidence::new(EvidenceKind::ReadOnlyDependency)],
            persistence_class: PersistenceClass::Unknown,
            semantic_role: SemanticRole::Unknown,
            first_seen_at: now,
            last_seen_at: now,
        };

        let known_id = db.upsert_path("/usr/local/share/game/content.pak").unwrap();
        let known_record = db.get_path_by_id(known_id).unwrap().unwrap();
        let known_assoc = association_for(known_id);
        let internal_id = db.upsert_path("/opt/noland/game/content.pak").unwrap();
        let internal_record = db.get_path_by_id(internal_id).unwrap().unwrap();
        let internal_assoc = association_for(internal_id);
        let classifier = Classifier::new(&db, "img");

        assert_eq!(
            classifier
                .classify_path(&known_record, &known_assoc)
                .unwrap()
                .0,
            PersistenceClass::ReconstructableApp
        );
        assert_ne!(
            classifier
                .decide(&known_record, &known_assoc, BackupMode::CompleteApplication)
                .unwrap(),
            BackupDecision::Exclude
        );
        assert_eq!(
            classifier
                .decide(
                    &internal_record,
                    &internal_assoc,
                    BackupMode::CompleteApplication
                )
                .unwrap(),
            BackupDecision::Exclude
        );
    }
}
