//! Stable all-app selection and child captures survive parent retries/restarts.
use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Child {
    pub app_id: AppId,
    pub operation_id: Uuid,
}

pub(super) fn plan(
    agent: &StateAgent,
    parent: Option<Uuid>,
    policy: RegenerableCachePolicy,
) -> Result<Vec<Child>> {
    let mut operation = parent
        .map(|id| agent.db.get_operation(id))
        .transpose()?
        .flatten();
    if let Some(saved) = operation
        .as_ref()
        .and_then(|op| op.detail_json.get("backup_children_v1"))
    {
        let children: Vec<Child> = serde_json::from_value(saved.clone())?;
        let mut apps = BTreeSet::new();
        let mut ids = BTreeSet::new();
        if children.iter().any(|child| {
            Some(child.operation_id) == parent
                || !ids.insert(child.operation_id)
                || !apps.insert(child.app_id.as_str())
        }) {
            return Err(StateError::Integrity(
                "invalid all-app child selection".into(),
            ));
        }
        return Ok(children);
    }
    let portable = noland_discovery::filter_backup_candidates(agent.db.list_apps()?)
        .into_iter()
        .map(|app| app.app_id)
        .collect::<BTreeSet<_>>();
    let mut selected = agent
        .db
        .list_dirty_apps()?
        .into_iter()
        .map(|dirty| dirty.app_id)
        .filter(|id| portable.contains(id))
        .collect::<BTreeSet<_>>();
    if selected.is_empty() {
        selected = portable;
    }
    let children = selected
        .into_iter()
        .map(|app_id| Child {
            app_id,
            operation_id: Uuid::new_v4(),
        })
        .collect::<Vec<_>>();
    if let Some(operation) = operation.as_mut() {
        operation.detail_json["backup_children_v1"] = serde_json::to_value(&children)?;
        operation.detail_json["regenerable_caches"] = serde_json::json!(policy.as_str());
        agent.db.upsert_operation(operation)?;
    }
    Ok(children)
}

pub(super) async fn completed(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation: Option<Uuid>,
    app: &AppId,
    mode: BackupMode,
    policy: RegenerableCachePolicy,
) -> Result<Option<BundleManifest>> {
    let Some(record) = operation
        .map(|id| agent.db.get_operation(id))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    if record.kind != "backup" || record.app_id.as_ref() != Some(app) {
        return Err(StateError::Integrity(
            "all-app child identity changed".into(),
        ));
    }
    if record.state != "COMPLETED" {
        return Ok(None);
    }
    let bundle = record
        .detail_json
        .get("bundle_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(|| StateError::Integrity("completed child has no bundle".into()))?;
    let binding = repository_cache_binding(provider.storage_identity(), master);
    let cached: Option<BundleManifest> = binding.as_ref().and_then(|binding| {
        cache::read_json(
            &parent_cache_dir(agent, app, bundle)
                .join(binding)
                .join("manifest.json"),
            master,
            &format!("parent:{binding}:{app}:{bundle}:manifest"),
        )
    });
    let manifest = match cached {
        Some(manifest) => manifest,
        None => read_committed_manifest(provider, master, app, bundle).await?,
    };
    if manifest.app.app_id != *app
        || manifest.bundle_id != bundle
        || manifest.mode != mode
        || !policy.matches_parent(&manifest)
    {
        return Err(StateError::Integrity(
            "completed child differs from its original selection".into(),
        ));
    }
    Ok(Some(manifest))
}
