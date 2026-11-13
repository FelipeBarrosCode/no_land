//! Explicit, preview-bound deletion. Never part of upload/download completion.
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_snapshot::{SourceMetadata, SourcePath};
use noland_state_core::*;
use noland_storage::SharedStorageProvider;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::StateAgent;

pub(crate) struct LocalLeaf {
    path: PathBuf,
    path_id: i64,
    metadata: SourceMetadata,
    parent: (u64, u64),
}

pub(crate) struct DeletionPlan {
    pub id: Uuid,
    pub target: String,
    pub created: Instant,
    pub preview: Value,
    pub binding: Option<String>,
    apps: BTreeSet<AppId>,
    local: Vec<LocalLeaf>,
    cloud: Option<noland_storage::FullDeletionPlan>,
}

struct DeletionJournal<'a> {
    db: &'a noland_state_db::StateDb,
    id: Uuid,
    preview: &'a Value,
    flushed: std::sync::Mutex<Option<(String, Instant)>>,
}

impl noland_storage::DeletionObserver for DeletionJournal<'_> {
    fn progress(&self, sample: &noland_storage::DeletionProgress) -> Result<()> {
        let mut flushed = self
            .flushed
            .lock()
            .map_err(|_| StateError::Storage("deletion progress lock poisoned".into()))?;
        if flushed.as_ref().is_some_and(|(phase, at)| {
            phase == &sample.phase && at.elapsed() < Duration::from_millis(500)
        }) {
            return Ok(());
        }
        let mut progress = OperationProgress::new(&sample.phase, sample.completed_objects);
        progress.total_units = Some(sample.total_objects);
        progress.unit = Some("objects".into());
        progress.detail_json = self.preview.clone();
        progress.detail_json["deletion"] = serde_json::to_value(sample)?;
        self.db.set_operation_progress(self.id, Some(&progress))?;
        *flushed = Some((sample.phase.clone(), Instant::now()));
        Ok(())
    }
}

pub(crate) fn ensure_idle(agent: &StateAgent) -> Result<()> {
    if !agent.operations.running_snapshots().is_empty()
        || agent.db.unfinished_operation_count()? != 0
    {
        return Err(StateError::Conflict(
            "finish or resolve active storage operations before deletion".into(),
        ));
    }
    Ok(())
}

pub(crate) fn binding(session: &EphemeralRcloneSession, master: &MasterKey) -> Result<String> {
    Ok(
        blake3::keyed_hash(&master.0, &serde_json::to_vec(&session.storage_identity())?)
            .to_hex()
            .to_string(),
    )
}

fn selections(params: &Value) -> Result<Vec<String>> {
    let paths: Vec<String> =
        serde_json::from_value(params.get("selected_paths").cloned().unwrap_or(Value::Null))?;
    if paths.is_empty() || paths.len() > 1000 {
        return Err(StateError::Invalid("select 1–1000 explicit items".into()));
    }
    Ok(paths
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn allowed_local(path: &Path, home: &Path) -> bool {
    path.is_absolute()
        && path.starts_with(home)
        && path != home
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && !is_base_system_path(path)
        && !is_hard_volatile_root(path)
        && !is_noland_internal_for_home(path, Some(home))
        && path
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("nup"))
        && !path.components().any(|c| {
            let part = c.as_os_str().to_string_lossy().to_ascii_lowercase();
            part.contains("pre-sync")
                || part.contains("presync")
                || part.starts_with(".noland-delete-")
                || matches!(part.as_str(), ".ssh" | ".gnupg")
        })
}

fn owned_root(root: &Path, home: &Path) -> bool {
    let Ok(relative) = root.strip_prefix(home) else {
        return false;
    };
    !relative.as_os_str().is_empty()
        && !matches!(
            relative.to_str(),
            Some(
                "Games"
                    | "Applications"
                    | "Downloads"
                    | "Documents"
                    | "Desktop"
                    | ".config"
                    | ".cache"
                    | ".local"
                    | ".local/share"
                    | ".local/state"
                    | ".steam"
            )
        )
        && allowed_local(root, home)
}

fn ensure_apps_stopped(agent: &StateAgent, apps: &BTreeSet<AppId>) -> Result<()> {
    agent.reconcile_live_processes()?;
    if agent
        .db
        .open_sessions()?
        .iter()
        .any(|s| apps.contains(&s.app_id))
    {
        return Err(StateError::Conflict(
            "close the selected applications before deleting their files".into(),
        ));
    }
    Ok(())
}

pub(crate) fn prepare_local(agent: &StateAgent, params: &Value) -> Result<DeletionPlan> {
    agent.discover()?;
    let available = noland_discovery::filter_backup_candidates(agent.db.list_apps()?);
    let mut apps = BTreeSet::new();
    let mut names = Vec::new();
    for path in selections(params)? {
        let id = path
            .strip_prefix("/apps/")
            .filter(|id| !id.is_empty() && !id.contains(['/', '*', '\\']))
            .ok_or_else(|| {
                StateError::Invalid("select individual applications for deletion".into())
            })?;
        let app = available
            .iter()
            .find(|app| app.app_id.as_str() == id)
            .ok_or_else(|| StateError::NotFound(id.into()))?;
        apps.insert(app.app_id.clone());
        names.push(app.display_name.clone());
    }
    ensure_apps_stopped(agent, &apps)?;
    let home = std::fs::canonicalize(&agent.config.home)?;
    let mut local = Vec::new();
    let mut seen = BTreeSet::new();
    let mut skipped = 0usize;
    for app in &apps {
        crate::reconcile::reconcile_app_for_complete_backup(agent, app)?;
        let scope = crate::reconcile::SteamBackupScope::load(agent, app)?;
        let owned = agent
            .db
            .known_roots(Some(app))?
            .into_iter()
            .map(|(_, _, p)| PathBuf::from(p))
            .filter(|p| owned_root(p, &home))
            .collect::<Vec<_>>();
        for (record, association) in agent.db.associations_for_app(app)? {
            let path = PathBuf::from(&record.canonical_path);
            if !seen.insert(path.clone()) {
                continue;
            }
            let explicit = association.evidence.iter().any(|e| {
                matches!(
                    e.kind,
                    EvidenceKind::ExplicitUserBinding | EvidenceKind::RestoredFromCommittedBundle
                )
            });
            let is_owned = scope.as_ref().map_or_else(
                || owned.iter().any(|r| path.starts_with(r)),
                |s| s.contains(&path),
            );
            if !allowed_local(&path, &home)
                || !(explicit || is_owned)
                || scope.as_ref().is_some_and(|s| !s.contains(&path))
                || agent
                    .db
                    .associations_for_path(record.path_id)?
                    .iter()
                    .any(|a| !apps.contains(&a.app_id))
            {
                skipped += 1;
                continue;
            }
            let source = match SourcePath::new(&path) {
                Ok(source) => source,
                Err(StateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let metadata = match source.metadata() {
                Ok(metadata) => metadata,
                Err(StateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if !(metadata.is_file() || metadata.is_symlink()) {
                continue;
            }
            local.push(LocalLeaf {
                path,
                path_id: record.path_id,
                metadata,
                parent: source.parent_identity()?,
            });
            if local.len() > 200_000 {
                return Err(StateError::Invalid(
                    "deletion selection is too large; select fewer applications".into(),
                ));
            }
        }
    }
    if local.is_empty() {
        return Err(StateError::Invalid(
            "no exclusively owned application files are eligible for deletion".into(),
        ));
    }
    let id = Uuid::new_v4();
    let preview = json!({"planId": id, "target":"remote_pc", "names": names,
        "items":local.len(), "bytes":local.iter().map(|l| l.metadata.len()).sum::<u64>(),
        "skipped": skipped, "retainsSharedPacks":false,
        "samplePaths":local.iter().take(20).map(|l| l.path.to_string_lossy()).collect::<Vec<_>>()});
    Ok(DeletionPlan {
        id,
        target: "remote_pc".into(),
        created: Instant::now(),
        preview,
        binding: None,
        apps,
        local,
        cloud: None,
    })
}

pub(crate) async fn prepare_cloud(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    params: &Value,
    binding: String,
    id: Uuid,
    owner: &str,
) -> Result<DeletionPlan> {
    let mut selected = Vec::new();
    for path in selections(params)? {
        let parts = path
            .strip_prefix("/catalog/")
            .or_else(|| path.strip_prefix("/retained-backups/"))
            .unwrap_or("")
            .split('/')
            .collect::<Vec<_>>();
        if parts.len() != 2 {
            return Err(StateError::Invalid(
                "select individual cloud backups for deletion".into(),
            ));
        }
        let id = Uuid::parse_str(parts[1])
            .map_err(|_| StateError::Invalid("invalid backup ID".into()))?;
        selected.push((AppId(parts[0].into()), id));
    }
    let full =
        noland_storage::prepare_full_deletion_selected(provider, master, &selected, id, owner)
            .await?;
    Ok(DeletionPlan::from_full(full, binding))
}

impl DeletionPlan {
    pub(crate) fn from_full(full: noland_storage::FullDeletionPlan, binding: String) -> Self {
        let id = full.plan_id;
        let preview = json!({"planId":id,"target":"cloud",
            "names":full.targets.iter().map(|t|t.name.clone()).collect::<Vec<_>>(),
            "items":full.targets.len(),"bytes":full.logical_bytes(),"physicalBytes":full.physical_bytes(),
            "packCount":full.packs.len(),"dependentBundles":full.targets.len().saturating_sub(full.requested_count),
            "skipped":0,"retainsSharedPacks":false,"samplePaths":[],"fullDeletion":true});
        Self {
            id,
            target: "cloud".into(),
            created: Instant::now(),
            preview,
            binding: Some(binding),
            apps: BTreeSet::new(),
            local: Vec::new(),
            cloud: Some(full),
        }
    }
    pub fn validate(&self, target: &str, binding: Option<&str>) -> Result<()> {
        if self.target != target
            || self.binding.as_deref() != binding
            || self.created.elapsed() > Duration::from_secs(600)
        {
            return Err(StateError::Conflict(
                "deletion preview expired or destination changed; preview again".into(),
            ));
        }
        Ok(())
    }

    pub async fn run(
        self,
        agent: &StateAgent,
        provider: Option<&dyn SharedStorageProvider>,
        master: Option<&MasterKey>,
    ) -> Result<()> {
        if !self.local.is_empty() {
            ensure_apps_stopped(agent, &self.apps)?;
            // Revalidate the entire confirmed set before the first mutation.
            for leaf in &self.local {
                self.check_leaf(agent, leaf)?;
            }
        }
        let total = self.local.len() + self.cloud.as_ref().map_or(0, |p| p.targets.len());
        let mut progress = OperationProgress::new("deleting", 0);
        progress.total_units = Some(total as u64);
        progress.unit = Some("items".into());
        progress.detail_json = self.preview.clone();
        agent.db.set_operation_progress(self.id, Some(&progress))?;
        for leaf in &self.local {
            if progress.completed_units % 128 == 0 {
                ensure_apps_stopped(agent, &self.apps)?;
            }
            let source = self.check_leaf(agent, leaf)?;
            source.delete_leaf(&leaf.metadata)?;
            progress.completed_units += 1;
            agent.db.set_operation_progress(self.id, Some(&progress))?;
            tokio::task::yield_now().await;
        }
        if let Some(full) = &self.cloud {
            let provider =
                provider.ok_or_else(|| StateError::Invalid("missing cloud session".into()))?;
            let master = master.ok_or_else(|| StateError::Invalid("missing vault key".into()))?;
            let guard =
                crate::repository::acquire(agent, provider, master, full.operation_id).await?;
            let observer = DeletionJournal {
                db: &agent.db,
                id: self.id,
                preview: &self.preview,
                flushed: std::sync::Mutex::new(None),
            };
            let result = noland_storage::execute_full_deletion_observed(
                provider,
                master,
                full,
                Some(&observer),
            )
            .await;
            guard.release().await?;
            result?;
        }
        for app in &self.apps {
            agent.db.mark_dirty(app, None, true)?;
        }
        Ok(())
    }

    fn check_leaf(&self, agent: &StateAgent, leaf: &LocalLeaf) -> Result<SourcePath> {
        let source = SourcePath::new(&leaf.path)?;
        if source.parent_identity()? != leaf.parent
            || source.metadata()? != leaf.metadata
            || agent
                .db
                .associations_for_path(leaf.path_id)?
                .iter()
                .any(|a| !self.apps.contains(&a.app_id))
        {
            return Err(StateError::Conflict(format!(
                "{} changed since deletion preview",
                leaf.path.display()
            )));
        }
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rpc_handler::AgentRpc, AgentConfig};
    use noland_rpc::{RpcHandler, RpcRequest};
    use std::sync::Arc;

    fn fixture() -> (PathBuf, Arc<StateAgent>, AppId, PathBuf) {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/delete-agent-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
        let app = AppId::desktop("deletion-game");
        let install = agent.config.home.join("Games/DeletionGame");
        std::fs::create_dir_all(&install).unwrap();
        let path = install.join("game.bin");
        std::fs::write(&path, b"fixture game bytes").unwrap();
        let mut identity = AppIdentity::new(app.clone(), "Deletion Game");
        identity.canonical_executable = Some(path.clone());
        agent.db.upsert_app(&identity).unwrap();
        agent
            .db
            .add_known_root(&app, "install", &install.to_string_lossy())
            .unwrap();
        (root, agent, app, path)
    }

    fn params(app: &AppId) -> Value {
        json!({"target":"remote_pc","selected_paths":[format!("/apps/{app}")]})
    }

    #[tokio::test]
    async fn cloud_progress_is_durable_throttled_and_available_in_read_only_projection() {
        use noland_storage::DeletionObserver;
        let (root, agent, _, _) = fixture();
        let id = Uuid::new_v4();
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id: id,
                kind: "delete".into(),
                app_id: None,
                state: "RUNNING".into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                last_error: None,
                detail_json: json!({"target":"cloud"}),
            })
            .unwrap();
        let preview = json!({"planId":id,"fullDeletion":true});
        let observer = DeletionJournal {
            db: &agent.db,
            id,
            preview: &preview,
            flushed: std::sync::Mutex::new(None),
        };
        let mut sample = noland_storage::DeletionProgress {
            phase: "validating".into(),
            validated_packs: 0,
            completed_objects: 0,
            total_objects: 4,
            completed_packs: 0,
            total_packs: 2,
            verified_absent_bytes: 0,
            total_bytes: 30,
            last_success_at: None,
            elapsed_ms: 0,
            concurrency: 4,
            storage_metrics: Default::default(),
        };
        observer.progress(&sample).unwrap();
        sample.validated_packs = 1;
        observer.progress(&sample).unwrap();
        assert_eq!(
            agent
                .db
                .get_operation_progress(id)
                .unwrap()
                .unwrap()
                .detail_json["deletion"]["validated_packs"],
            0,
            "unchanged-phase SQLite flushes are throttled"
        );
        sample.phase = "deleting_packs".into();
        sample.completed_objects = 1;
        sample.completed_packs = 1;
        sample.verified_absent_bytes = 10;
        sample.last_success_at = Some(chrono::Utc::now());
        observer.progress(&sample).unwrap();
        let status = rpc(
            &agent,
            "GetOperationStatus",
            json!({"operation_id":id,"projection":"progress"}),
        )
        .await
        .unwrap();
        assert_eq!(
            status["progress"]["detail_json"]["deletion"]["completed_packs"],
            1
        );
        assert_eq!(status["progress"]["completed_units"], 1);
        sample.phase = "finalizing".into();
        sample.completed_objects = 4;
        sample.completed_packs = 2;
        sample.verified_absent_bytes = 30;
        observer.progress(&sample).unwrap();
        assert_eq!(
            agent
                .db
                .get_operation_progress(id)
                .unwrap()
                .unwrap()
                .detail_json["deletion"]["verified_absent_bytes"],
            30,
            "phase changes force a final snapshot even inside the throttle window"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn rpc(agent: &Arc<StateAgent>, method: &str, params: Value) -> Result<Value> {
        AgentRpc(agent.clone())
            .handle(&RpcRequest {
                id: "delete-test".into(),
                method: method.into(),
                params,
            })
            .await
    }

    #[tokio::test]
    async fn preview_confirmation_and_replay_delete_only_the_frozen_files() {
        let (root, agent, app, path) = fixture();
        let package = path.with_extension("nup");
        std::fs::write(&package, b"preserved package").unwrap();
        let old = path.parent().unwrap().join("pre-sync-keep/save");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"preserved save").unwrap();
        let preview = rpc(&agent, "PrepareDeletion", params(&app)).await.unwrap();
        assert!(path.exists(), "preview is read-only");
        assert_eq!(preview["items"], 1);
        assert!(rpc(
            &agent,
            "StartDeletion",
            json!({"plan_id":preview["planId"],"target":"remote_pc"})
        )
        .await
        .is_err());
        let confirm = json!({"plan_id":preview["planId"],"target":"remote_pc","confirmed":true});
        let accepted = rpc(&agent, "StartDeletion", confirm.clone()).await.unwrap();
        for _ in 0..1000 {
            if agent.operations.running_snapshots().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let replay = rpc(&agent, "StartDeletion", confirm).await.unwrap();
        assert_eq!(replay["state"], "COMPLETED", "{replay}");
        assert_eq!(replay["operation_id"], accepted["operation_id"]);
        assert!(!path.exists());
        assert!(package.exists());
        assert!(old.exists());
        // A recreated file is not part of an already accepted deletion.
        std::fs::write(&path, b"new generation").unwrap();
        rpc(
            &agent,
            "StartDeletion",
            json!({"plan_id":preview["planId"],"target":"remote_pc","confirmed":true}),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new generation");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preview_rejects_wildcards_stale_sources_and_new_shared_ownership() {
        let (root, agent, app, path) = fixture();
        assert!(prepare_local(&agent, &json!({"selected_paths":["/apps/*"]})).is_err());
        let mut plan = prepare_local(&agent, &params(&app)).unwrap();
        assert!(plan.validate("cloud", None).is_err());
        plan.created = Instant::now() - Duration::from_secs(601);
        assert!(plan.validate("remote_pc", None).is_err());
        std::fs::write(&path, b"changed").unwrap();
        assert!(plan.check_leaf(&agent, &plan.local[0]).is_err());
        let plan = prepare_local(&agent, &params(&app)).unwrap();
        let other = AppId::desktop("other-game");
        agent
            .db
            .upsert_app(&AppIdentity::new(other.clone(), "Other Game"))
            .unwrap();
        let mut association = agent
            .db
            .associations_for_path(plan.local[0].path_id)
            .unwrap()
            .remove(0);
        association.app_id = other;
        agent.db.upsert_association(&association).unwrap();
        assert!(plan.check_leaf(&agent, &plan.local[0]).is_err());
        assert!(path.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preview_does_not_follow_replaced_parent_or_leaf() {
        use std::os::unix::fs::symlink;
        let (root, agent, app, path) = fixture();
        let plan = prepare_local(&agent, &params(&app)).unwrap();
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("game.bin"), b"outside data").unwrap();
        let parent = path.parent().unwrap();
        std::fs::rename(parent, parent.with_extension("moved")).unwrap();
        symlink(&outside, parent).unwrap();
        assert!(plan.check_leaf(&agent, &plan.local[0]).is_err());
        assert_eq!(
            std::fs::read(outside.join("game.bin")).unwrap(),
            b"outside data"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
