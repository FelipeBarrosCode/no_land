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
    // A validated saved selection above is immutable even when live discovery
    // or registration state has changed. Only NEW selection consults live state;
    // each unprepared child still performs its own full fresh preflight.
    let mut metrics = operation
        .as_ref()
        .map(operation_metrics)
        .unwrap_or_default();
    fresh_preflight(agent, operation.as_mut(), &mut metrics)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    const PID: i32 = i32::MAX - 40;

    struct Fixture {
        root: PathBuf,
        agent: StateAgent,
        parent: Uuid,
        app: AppId,
        executable: PathBuf,
        data: PathBuf,
    }

    fn desktop(agent: &StateAgent, id: &str) -> (AppId, PathBuf, PathBuf) {
        let install = agent.config.home.join("Applications").join(id);
        let executable = install.join("bin/game");
        let data = install.join("custom-data/save.dat");
        let entries = agent.config.home.join(".local/share/applications");
        std::fs::create_dir_all(&entries).unwrap();
        for path in [&executable, &data] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture bytes").unwrap();
        }
        std::fs::write(
            entries.join(format!("{id}.desktop")),
            format!(
                "[Desktop Entry]\nName={id}\nExec=\"{}\"\nType=Application\n",
                executable.display()
            ),
        )
        .unwrap();
        (AppId::desktop(id), executable, data)
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = crate::index_test_support::root(label);
            let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
            let (app, executable, data) = desktop(&agent, "plan-first");
            desktop(&agent, "plan-second");
            let parent = Uuid::new_v4();
            let metrics = OperationMetrics {
                discovery_duration_ms: 19,
                ..OperationMetrics::default()
            };
            agent
                .db
                .upsert_operation(&OperationRecord {
                    operation_id: parent,
                    kind: "backup".into(),
                    app_id: None,
                    state: BackupState::Queued.as_str().into(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    last_error: None,
                    detail_json: serde_json::json!({
                        "app_id": "*",
                        "mode": "complete_application",
                        "metrics": metrics,
                    }),
                })
                .unwrap();
            agent.db.set_operation_metrics(parent, &metrics).unwrap();
            Self {
                root,
                agent,
                parent,
                app,
                executable,
                data,
            }
        }

        fn queue_watermark(&self) {
            for index in 0..crate::collector::BACKGROUND_EVENT_BATCH {
                let mut event =
                    noland_observer::process_exec(i32::MAX - 1000 - index as i32, 1, "/opt/unused");
                event.kind = ProcessEventKind::Exit;
                self.agent.hub.inject_process(event);
            }
            self.agent
                .hub
                .inject_process(noland_observer::process_exec(PID, 1, &self.executable));
            self.agent.hub.inject_fs(noland_observer::fs_event(
                FsEventKind::Write,
                PID,
                &self.data,
            ));
        }

        fn invalidate_live_registration(&self) {
            let registry = self
                .agent
                .config
                .home
                .join(".local/share/noland-runtime/apps");
            std::fs::create_dir_all(&registry).unwrap();
            std::fs::write(registry.join("malformed.json"), b"{invalid").unwrap();
            self.agent.hub.report_filesystem_gap(PID);
        }

        fn finish(self) {
            drop(self.agent);
            std::fs::remove_dir_all(self.root).unwrap();
        }
    }

    #[test]
    fn fresh_plan_discovers_and_flushes_the_full_watermark_before_child_selection() {
        let fixture = Fixture::new("group-fresh-discovery");
        assert!(fixture.agent.db.get_app(&fixture.app).unwrap().is_none());
        fixture.queue_watermark();
        let children = plan(
            &fixture.agent,
            Some(fixture.parent),
            RegenerableCachePolicy::Include,
        )
        .unwrap();
        // Only the write behind the background limit marks an app dirty. A
        // missing/partial flush would incorrectly select every discovered app.
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].app_id, fixture.app);
        assert_ne!(children[0].operation_id, fixture.parent);
        assert!(fixture
            .agent
            .db
            .get_app(&AppId::desktop("plan-second"))
            .unwrap()
            .is_some());
        assert!(!fixture.agent.hub.queue.has_pending());
        let record = fixture
            .agent
            .db
            .get_operation(fixture.parent)
            .unwrap()
            .unwrap();
        let stored = operation_metrics(&record);
        let runtime = fixture
            .agent
            .db
            .get_operation_metrics(fixture.parent)
            .unwrap()
            .unwrap();
        assert!(stored.discovery_duration_ms > 19);
        assert_eq!(stored.discovery_duration_ms, runtime.discovery_duration_ms);
        assert_eq!(
            record.detail_json["backup_children_v1"],
            serde_json::to_value(&children).unwrap()
        );
        fixture.finish();
    }

    #[test]
    fn saved_plan_preserves_children_without_discovery_or_a_failing_live_flush() {
        let fixture = Fixture::new("group-saved-invalid-live");
        fixture.queue_watermark();
        let selected = plan(
            &fixture.agent,
            Some(fixture.parent),
            RegenerableCachePolicy::Include,
        )
        .unwrap();
        let saved = serde_json::to_value(&selected).unwrap();
        let before = fixture
            .agent
            .db
            .get_operation_metrics(fixture.parent)
            .unwrap()
            .unwrap();
        let (later, _, _) = desktop(&fixture.agent, "plan-later");
        fixture.invalidate_live_registration();
        let replay = plan(
            &fixture.agent,
            Some(fixture.parent),
            RegenerableCachePolicy::Include,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(replay).unwrap(), saved);
        assert!(fixture.agent.db.get_app(&later).unwrap().is_none());
        assert!(fixture.agent.hub.queue.has_queued_events());
        assert!(fixture.agent.hub.queue.loss_snapshot().is_none());
        assert_eq!(
            fixture
                .agent
                .db
                .get_operation_metrics(fixture.parent)
                .unwrap()
                .unwrap()
                .discovery_duration_ms,
            before.discovery_duration_ms
        );
        // The queued gap really would fail; successful plan replay must not be
        // implemented by ignoring that error or accepting current live paths.
        assert!(fixture.agent.process_events().is_err());
        fixture.finish();
    }

    #[test]
    fn corrupt_saved_selection_does_not_fall_back_to_fresh_discovery() {
        let fixture = Fixture::new("group-corrupt-plan");
        let mut record = fixture
            .agent
            .db
            .get_operation(fixture.parent)
            .unwrap()
            .unwrap();
        record.detail_json["backup_children_v1"] = serde_json::json!([{
            "app_id": fixture.app,
            "operation_id": fixture.parent,
        }]);
        fixture.agent.db.upsert_operation(&record).unwrap();
        fixture.agent.hub.report_filesystem_gap(PID);
        assert!(matches!(
            plan(
                &fixture.agent,
                Some(fixture.parent),
                RegenerableCachePolicy::Include,
            ),
            Err(StateError::Integrity(_))
        ));
        assert!(fixture.agent.db.get_app(&fixture.app).unwrap().is_none());
        assert!(fixture.agent.hub.queue.has_queued_events());
        assert_eq!(
            fixture
                .agent
                .db
                .get_operation_metrics(fixture.parent)
                .unwrap()
                .unwrap()
                .discovery_duration_ms,
            19
        );
        fixture.finish();
    }

    #[tokio::test]
    async fn saved_unprepared_child_still_requires_its_own_full_fresh_preflight() {
        let fixture = Fixture::new("group-unprepared-child");
        fixture.queue_watermark();
        let selected = plan(
            &fixture.agent,
            Some(fixture.parent),
            RegenerableCachePolicy::Include,
        )
        .unwrap();
        let child = selected[0].operation_id;
        let saved = serde_json::to_value(&selected).unwrap();
        fixture.invalidate_live_registration();
        let storage = LocalStorage::new(fixture.root.join("remote"));
        assert!(run_backup_all(
            &fixture.agent,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Fast,
            &storage,
            &MasterKey::generate(),
            Some(fixture.parent),
        )
        .await
        .is_err());
        assert_eq!(
            fixture
                .agent
                .db
                .get_operation(fixture.parent)
                .unwrap()
                .unwrap()
                .detail_json["backup_children_v1"],
            saved
        );
        assert_eq!(
            fixture
                .agent
                .db
                .get_operation(child)
                .unwrap()
                .unwrap()
                .app_id,
            Some(fixture.app.clone())
        );
        assert!(fixture
            .agent
            .db
            .latest_commit(&fixture.app)
            .unwrap()
            .is_none());
        assert!(fixture.agent.hub.queue.loss_snapshot().is_some());
        assert!(fixture.agent.hub.queue.has_queued_events());
        fixture.finish();
    }
}
