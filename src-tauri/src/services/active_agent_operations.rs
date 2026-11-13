use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveAgentOperation {
    pub instance_id: u64,
    pub operation_id: String,
    pub kind: String,
}

/// Retained monitoring handles, including operations whose connection is paused.
/// Completing one operation must never discard another instance's cancellation
/// handle, or a newer operation on the same instance.
#[derive(Default)]
pub(super) struct ActiveAgentOperations {
    instances: HashMap<u64, Vec<ActiveAgentOperation>>,
}

impl ActiveAgentOperations {
    pub(super) fn get_by_id(
        &self,
        instance_id: u64,
        operation_id: &str,
    ) -> Option<ActiveAgentOperation> {
        self.instances
            .get(&instance_id)?
            .iter()
            .find(|operation| operation.operation_id == operation_id)
            .cloned()
    }

    pub(super) fn get(&self, instance_id: u64, kind: Option<&str>) -> Option<ActiveAgentOperation> {
        self.instances
            .get(&instance_id)?
            .iter()
            .rev()
            .find(|operation| kind.is_none_or(|kind| operation.kind == kind))
            .cloned()
    }

    pub(super) fn retain(&mut self, operation: ActiveAgentOperation) {
        let operations = self.instances.entry(operation.instance_id).or_default();
        operations.retain(|existing| existing.operation_id != operation.operation_id);
        operations.push(operation);
    }

    pub(super) fn clear(&mut self, instance_id: u64, operation_id: &str) {
        if let Some(operations) = self.instances.get_mut(&instance_id) {
            operations.retain(|operation| operation.operation_id != operation_id);
            if operations.is_empty() {
                self.instances.remove(&instance_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(instance_id: u64, id: &str, kind: &str) -> ActiveAgentOperation {
        ActiveAgentOperation {
            instance_id,
            operation_id: id.into(),
            kind: kind.into(),
        }
    }

    #[test]
    fn independent_instances_keep_their_own_handles() {
        let mut handles = ActiveAgentOperations::default();
        handles.retain(operation(1, "backup", "backup"));
        handles.retain(operation(2, "restore", "restore"));
        handles.clear(2, "restore");
        assert_eq!(handles.get(1, None), Some(operation(1, "backup", "backup")));
        assert!(handles.get(2, None).is_none());
    }

    #[test]
    fn late_completion_cannot_clear_a_newer_or_different_kind_handle() {
        let mut handles = ActiveAgentOperations::default();
        handles.retain(operation(1, "old", "backup"));
        handles.retain(operation(1, "new", "backup"));
        handles.retain(operation(1, "seal", "seal"));
        handles.clear(1, "old");
        assert_eq!(
            handles.get(1, Some("backup")),
            Some(operation(1, "new", "backup"))
        );
        assert_eq!(handles.get(1, None), Some(operation(1, "seal", "seal")));
        handles.clear(2, "new");
        assert!(handles.get(1, Some("backup")).is_some());
    }

    #[test]
    fn reattachment_replaces_only_its_own_handle() {
        let mut handles = ActiveAgentOperations::default();
        handles.retain(operation(1, "paused", "restore"));
        handles.retain(operation(1, "other", "backup"));
        handles.retain(operation(1, "paused", "restore"));
        assert_eq!(handles.instances[&1].len(), 2);
        assert_eq!(
            handles.get(1, None),
            Some(operation(1, "paused", "restore"))
        );
        handles.clear(1, "paused");
        assert_eq!(handles.get(1, None), Some(operation(1, "other", "backup")));
    }

    #[test]
    fn exact_cancellation_does_not_select_a_newer_operation() {
        let mut handles = ActiveAgentOperations::default();
        handles.retain(operation(1, "paused", "restore"));
        handles.retain(operation(1, "new", "seal"));
        assert_eq!(
            handles.get_by_id(1, "paused"),
            Some(operation(1, "paused", "restore"))
        );
        assert!(handles.get_by_id(2, "paused").is_none());
        handles.clear(1, "paused");
        assert!(handles.get_by_id(1, "paused").is_none());
        assert_eq!(handles.get(1, None), Some(operation(1, "new", "seal")));
    }
}
