//! Two-dimensional persistence + semantic classification.

use std::path::Path;

use noland_baseline::{matches_baseline, package_owner};
use noland_state_core::*;
use noland_state_db::StateDb;

pub struct Classifier<'a> {
    pub db: &'a StateDb,
    pub image_id: String,
}

impl<'a> Classifier<'a> {
    pub fn new(db: &'a StateDb, image_id: impl Into<String>) -> Self {
        Self {
            db,
            image_id: image_id.into(),
        }
    }

    pub fn classify_path(
        &self,
        record: &PathRecord,
        assoc: &PathAssociation,
    ) -> Result<(PersistenceClass, SemanticRole)> {
        let path = Path::new(&record.canonical_path);
        if is_noland_internal(path)
            || is_hard_volatile_root(path)
            || looks_like_lock_or_socket(path)
        {
            return Ok((PersistenceClass::Ephemeral, SemanticRole::Temp));
        }
        if looks_like_cache(path) {
            return Ok((PersistenceClass::Ephemeral, SemanticRole::Cache));
        }
        if looks_like_secret(path) {
            return Ok((PersistenceClass::PersistentState, SemanticRole::Secret));
        }

        let baseline_hit =
            matches_baseline(self.db, &self.image_id, path, record.size.map(|s| s as u64))?;
        let pkg = package_owner(self.db, &self.image_id, path)?;

        let others = self.db.associations_for_path(record.path_id)?;
        let shared = others
            .iter()
            .filter(|a| a.confidence >= OWNERSHIP_CANDIDATE_MIN)
            .count()
            > 1;

        let class = if baseline_hit && !assoc.evidence.iter().any(|e| e.kind.is_mutation()) {
            PersistenceClass::BaseImage
        } else if baseline_hit && assoc.evidence.iter().any(|e| e.kind.is_mutation()) {
            PersistenceClass::PersistentState
        } else if pkg.is_some() && !assoc.evidence.iter().any(|e| e.kind.is_mutation()) {
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

    pub fn reclassify_app(&self, app_id: &AppId) -> Result<usize> {
        let rows = self.db.associations_for_app(app_id)?;
        let mut n = 0;
        for (record, mut assoc) in rows {
            let (class, role) = self.classify_path(&record, &assoc)?;
            if class != assoc.persistence_class || role != assoc.semantic_role {
                assoc.persistence_class = class;
                assoc.semantic_role = role;
                self.db.upsert_association(&assoc)?;
                n += 1;
            }
        }
        Ok(n)
    }

    pub fn decide(
        &self,
        record: &PathRecord,
        assoc: &PathAssociation,
        mode: BackupMode,
    ) -> Result<BackupDecision> {
        let path = Path::new(&record.canonical_path);
        let (class, role) = self.classify_path(record, assoc)?;
        let mut assoc = assoc.clone();
        assoc.persistence_class = class;
        assoc.semantic_role = role;
        let policy = self
            .db
            .path_policy(&record.canonical_path, Some(&assoc.app_id))?
            .as_deref()
            .and_then(parse_policy);
        let ctx = PathDecisionContext {
            path,
            association: &assoc,
            mode,
            matches_image_baseline: matches_baseline(
                self.db,
                &self.image_id,
                path,
                record.size.map(|s| s as u64),
            )?,
            reliable_reconstruction: package_owner(self.db, &self.image_id, path)?.is_some()
                || record.canonical_path.contains("/steamapps/common/"),
            policy_override: policy,
        };
        Ok(decide_path(ctx))
    }
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
}
