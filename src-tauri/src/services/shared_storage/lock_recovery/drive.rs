//! Google Drive metadata-only recovery transport. Credentials stay inside the
//! application; there is no local rclone requirement or remote-agent update.
use async_trait::async_trait;
use noland_crypto::{derive_keys, MasterKey};
use noland_state_core::{Result, StateError};
use noland_storage::{
    open_metadata_with_limit,
    repository_recovery::{
        RecoveryCheckpoint, RecoveryMetadataStore, RecoveryPhase, RecoveryReceipt,
    },
    seal_metadata, RepositoryActivity,
};
use reqwest::{Client, Response};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Duration;
use uuid::Uuid;

const API: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const FOLDER: &str = "application/vnd.google-apps.folder";
const FIELDS: &str = "id,name,mimeType,size,parents,trashed,version";
const MAX_METADATA: usize = 16 * 1024;
// Ordinary admission permits 1,024 claims. Targeted recovery must still observe
// that entire inventory after adding its own single execution-unique fence.
const MAX_LISTING: usize = 1025;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFile {
    id: String,
    name: String,
    mime_type: String,
    size: Option<String>,
    #[serde(default)]
    parents: Vec<String>,
    #[serde(default)]
    trashed: bool,
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
    incomplete_search: bool,
}

pub(super) struct DriveMetadata {
    http: Client,
    access_token: String,
    master: MasterKey,
    pub root_id: String,
    maintenance_id: Option<String>,
    activity_id: Option<String>,
}

fn invalid(message: &str) -> StateError {
    StateError::Integrity(message.into())
}

fn escaped_query(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn valid_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 256
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid("invalid Drive object identity"));
    }
    Ok(())
}

fn matches_parent(requested: &str, observed: &[String]) -> bool {
    // Drive's "root" is a query alias. With drive.file authorization its
    // metadata is not readable, but an app-created child's parents contain
    // the actual root ID. The server's exact "'root' in parents" query proves
    // membership; every subsequent traversal uses the concrete folder ID.
    if requested == "root" {
        observed.len() == 1 && valid_id(&observed[0]).is_ok()
    } else {
        observed == [requested]
    }
}

async fn bounded(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        // Provider error bodies can echo credential-bearing requests. Never
        // expose them in diagnostics, native errors, MCP output or app logs.
        return Err(StateError::Storage(format!(
            "Google Drive lock metadata request failed (HTTP {})",
            response.status().as_u16()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(invalid("Drive metadata response exceeds its limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        StateError::Storage("Drive metadata response interrupted; outcome may be uncertain".into())
    })? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(invalid("Drive metadata response exceeds its limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

impl DriveMetadata {
    pub async fn connect(access_token: String, master: MasterKey, path: &str) -> Result<Self> {
        if path.len() > 4096 || path.split('/').count() > 32 {
            return Err(invalid(
                "repository metadata path exceeds recovery traversal limits",
            ));
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| StateError::Storage("could not create metadata client".into()))?;
        let mut this = Self {
            http,
            access_token,
            master,
            root_id: String::new(),
            maintenance_id: None,
            activity_id: None,
        };
        // Do not GET files/root: drive.file intentionally grants access only
        // to app-created/selected files, not to root's own metadata.
        let mut parent = "root".to_string();
        for component in path.split('/').filter(|part| !part.is_empty()) {
            if matches!(component, "." | "..") || component.contains('\0') {
                return Err(invalid("invalid repository folder path"));
            }
            parent = this
                .folder(&parent, component, false)
                .await?
                .ok_or_else(|| {
                    StateError::Storage("configured repository folder does not exist".into())
                })?;
        }
        this.root_id = parent;
        this.maintenance_id = this.folder(&this.root_id, "maintenance", false).await?;
        if let Some(maintenance) = &this.maintenance_id {
            this.activity_id = this.folder(maintenance, "activity", false).await?;
        }
        Ok(this)
    }

    async fn response(&self, request: reqwest::RequestBuilder) -> Result<Response> {
        request
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|_| {
                StateError::Storage(
                    "Drive metadata request interrupted; inspect before any new recovery attempt"
                        .into(),
                )
            })
    }

    async fn get_file(&self, id: &str) -> Result<DriveFile> {
        valid_id(id)?;
        let response = self
            .response(
                self.http
                    .get(format!("{API}/{id}"))
                    .query(&[("fields", FIELDS), ("supportsAllDrives", "true")]),
            )
            .await?;
        let file: DriveFile = serde_json::from_slice(&bounded(response, MAX_METADATA).await?)?;
        valid_id(&file.id)?;
        if id != "root" && file.id != id {
            return Err(invalid("Drive returned metadata for a different object"));
        }
        Ok(file)
    }

    async fn list(&self, parent: &str, name: Option<&str>) -> Result<Vec<DriveFile>> {
        valid_id(parent)?;
        let mut query = format!("'{}' in parents and trashed = false", escaped_query(parent));
        if let Some(name) = name {
            query.push_str(&format!(" and name = '{}'", escaped_query(name)));
        }
        let mut files = Vec::new();
        let mut page_token = String::new();
        let mut tokens = BTreeSet::new();
        let mut ids = BTreeSet::new();
        loop {
            let mut request = self.http.get(API).query(&[
                ("q", query.as_str()),
                ("fields", "nextPageToken,incompleteSearch,files(id,name,mimeType,size,parents,trashed,version)"),
                ("pageSize", "1000"),
                ("supportsAllDrives", "true"),
                ("includeItemsFromAllDrives", "true"),
            ]);
            if !page_token.is_empty() {
                request = request.query(&[("pageToken", &page_token)]);
            }
            let response = self.response(request).await?;
            let page: Page = serde_json::from_slice(&bounded(response, 1024 * 1024).await?)?;
            if page.incomplete_search {
                return Err(invalid("Drive returned an incomplete repository listing"));
            }
            for file in page.files {
                valid_id(&file.id)?;
                if file.trashed
                    || !matches_parent(parent, &file.parents)
                    || name.is_some_and(|name| file.name != name)
                    || !ids.insert(file.id.clone())
                    || files.len() == MAX_LISTING
                {
                    return Err(invalid("ambiguous or invalid repository metadata listing"));
                }
                files.push(file);
            }
            match page.next_page_token.filter(|token| !token.is_empty()) {
                None => return Ok(files),
                Some(next) if tokens.len() < 16 && tokens.insert(next.clone()) => page_token = next,
                _ => return Err(invalid("invalid repository listing pagination")),
            }
        }
    }

    async fn folder(&self, parent: &str, name: &str, create: bool) -> Result<Option<String>> {
        let files = self.list(parent, Some(name)).await?;
        match files.as_slice() {
            [] if !create => Ok(None),
            [] => {
                let response = self
                    .response(
                        self.http
                            .post(API)
                            .query(&[("fields", FIELDS)])
                            .json(&json!({"name":name,"mimeType":FOLDER,"parents":[parent]})),
                    )
                    .await?;
                let file: DriveFile =
                    serde_json::from_slice(&bounded(response, MAX_METADATA).await?)?;
                if file.name != name || file.mime_type != FOLDER || file.parents != [parent] {
                    return Err(invalid("created metadata folder identity mismatch"));
                }
                valid_id(&file.id)?;
                let observed = self.list(parent, Some(name)).await?;
                if observed.len() != 1 || observed[0].id != file.id {
                    return Err(invalid("metadata folder creation was ambiguous"));
                }
                Ok(Some(file.id))
            }
            [file] if file.mime_type == FOLDER => Ok(Some(file.id.clone())),
            _ => Err(invalid(
                "repository folder name is ambiguous or not a folder",
            )),
        }
    }

    async fn read_activity(&self, file: &DriveFile) -> Result<RepositoryActivity> {
        let expected = file
            .name
            .strip_suffix(".enc")
            .and_then(|stem| Uuid::parse_str(stem).ok())
            .ok_or_else(|| invalid("invalid activity object name"))?;
        let activity: RepositoryActivity =
            self.read_document(file, b"repository-activity-v1").await?;
        if activity.claim_id != expected || file.name != format!("{expected}.enc") {
            return Err(invalid(
                "authenticated activity identity does not match its object",
            ));
        }
        Ok(activity)
    }

    async fn read_document<T: DeserializeOwned>(&self, file: &DriveFile, aad: &[u8]) -> Result<T> {
        let size = file
            .size
            .as_deref()
            .and_then(|size| size.parse::<usize>().ok())
            .filter(|size| *size > 0 && *size <= MAX_METADATA)
            .ok_or_else(|| invalid("invalid activity object length"))?;
        if file.mime_type == FOLDER || file.version.as_deref().is_none_or(str::is_empty) {
            return Err(invalid("activity is not a versioned metadata object"));
        }
        let response = self
            .response(
                self.http
                    .get(format!("{API}/{}", file.id))
                    .query(&[("alt", "media")]),
            )
            .await?;
        let bytes = bounded(response, MAX_METADATA).await?;
        if bytes.len() != size {
            return Err(invalid("activity download length changed"));
        }
        let document = open_metadata_with_limit(
            &derive_keys(&self.master).catalog,
            aad,
            bytes,
            MAX_METADATA as u64,
        )?;
        let current = self.get_file(&file.id).await?;
        if current.name != file.name
            || current.parents != file.parents
            || current.version != file.version
            || current.trashed
        {
            return Err(invalid("activity changed while authenticating"));
        }
        Ok(document)
    }

    async fn put_new<T: serde::Serialize + DeserializeOwned + PartialEq>(
        &self,
        parent: &str,
        name: &str,
        aad: &[u8],
        value: &T,
    ) -> Result<()> {
        if !self.list(parent, Some(name)).await?.is_empty() {
            return Err(invalid(
                "recovery metadata already exists; do not retry this execution",
            ));
        }
        let envelope = seal_metadata(&derive_keys(&self.master).catalog, aad, value)?.envelope;
        if envelope.len() > MAX_METADATA {
            return Err(invalid("recovery metadata exceeds its limit"));
        }
        let boundary = format!("noland-{}", Uuid::new_v4());
        let metadata = serde_json::to_vec(&json!({
            "name":name,"parents":[parent],"mimeType":"application/octet-stream"
        }))?;
        let mut body =
            format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n")
                .into_bytes();
        body.extend(metadata);
        body.extend(
            format!("\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(&envelope);
        body.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
        let response = self
            .response(
                self.http
                    .post(UPLOAD)
                    .query(&[
                        ("uploadType", "multipart"),
                        ("fields", FIELDS),
                        ("supportsAllDrives", "true"),
                    ])
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        format!("multipart/related; boundary={boundary}"),
                    )
                    .body(body),
            )
            .await?;
        let file: DriveFile = serde_json::from_slice(&bounded(response, MAX_METADATA).await?)?;
        valid_id(&file.id)?;
        let observed = self.list(parent, Some(name)).await?;
        if file.name != name
            || file.parents != [parent]
            || observed.len() != 1
            || observed[0].id != file.id
        {
            return Err(invalid(
                "recovery metadata publication could not be uniquely confirmed",
            ));
        }
        // A newly uploaded Drive file can gain a metadata-only version after
        // upload acknowledgement. Retry ONLY bounded reads of this exact ID,
        // requiring the exact uploaded ciphertext and value every time. Never
        // re-upload, adopt changed bytes, or relax existing-claim verification.
        for _ in 0..3 {
            let before = self.get_file(&file.id).await?;
            if before.name != name
                || before.parents != [parent]
                || before.trashed
                || before.size.as_deref() != Some(envelope.len().to_string().as_str())
                || before.version.as_deref().is_none_or(str::is_empty)
            {
                return Err(invalid("new recovery metadata identity changed"));
            }
            let response = self
                .response(
                    self.http
                        .get(format!("{API}/{}", file.id))
                        .query(&[("alt", "media")]),
                )
                .await?;
            let bytes = bounded(response, MAX_METADATA).await?;
            if bytes != envelope {
                return Err(invalid("new recovery metadata ciphertext changed"));
            }
            let authenticated: T = open_metadata_with_limit(
                &derive_keys(&self.master).catalog,
                aad,
                bytes,
                MAX_METADATA as u64,
            )?;
            if authenticated != *value {
                return Err(invalid("new recovery metadata value changed"));
            }
            let after = self.get_file(&file.id).await?;
            if after.name != before.name
                || after.parents != before.parents
                || after.size != before.size
                || after.trashed
            {
                return Err(invalid("new recovery metadata identity changed"));
            }
            if after.version == before.version {
                return Ok(());
            }
        }
        Err(invalid("new recovery metadata did not stabilize"))
    }

    pub(super) async fn checkpoint(&self, id: Uuid) -> Result<RecoveryCheckpoint> {
        let maintenance = self
            .maintenance_id
            .as_ref()
            .ok_or_else(|| invalid("maintenance folder missing"))?;
        let parent = self
            .folder(maintenance, "lock-recoveries", false)
            .await?
            .ok_or_else(|| invalid("original recovery receipts are unavailable"))?;
        let mut receipts = Vec::new();
        for phase in ["prepared", "complete"] {
            let files = self
                .list(&parent, Some(&format!("{id}.{phase}.enc")))
                .await?;
            let receipt = match files.as_slice() {
                [] => None,
                [file] => Some(
                    self.read_document::<RecoveryReceipt>(file, b"repository-lock-recovery-v1")
                        .await?,
                ),
                _ => return Err(invalid("ambiguous original recovery receipt")),
            };
            if receipt.as_ref().is_some_and(|receipt| {
                receipt.recovery_id != id
                    || match receipt.phase {
                        RecoveryPhase::Prepared => "prepared",
                        RecoveryPhase::Complete => "complete",
                    } != phase
            }) {
                return Err(invalid("original recovery receipt identity mismatch"));
            }
            receipts.push(receipt);
        }
        Ok(RecoveryCheckpoint {
            prepared: receipts[0]
                .take()
                .ok_or_else(|| invalid("authenticated prepared recovery receipt missing"))?,
            complete: receipts[1].take(),
        })
    }
}

#[async_trait]
impl RecoveryMetadataStore for DriveMetadata {
    async fn activities(&self) -> Result<Vec<RepositoryActivity>> {
        let Some(parent) = &self.activity_id else {
            return Ok(Vec::new());
        };
        let mut activities = Vec::new();
        let mut ids = BTreeSet::new();
        for file in self.list(parent, None).await? {
            let activity = self.read_activity(&file).await?;
            if !ids.insert(activity.claim_id) {
                return Err(invalid("duplicate repository activity identity"));
            }
            activities.push(activity);
        }
        activities.sort_by_key(|activity| activity.claim_id);
        Ok(activities)
    }

    async fn has_pending_deletion(&self) -> Result<bool> {
        let Some(maintenance) = &self.maintenance_id else {
            return Ok(false);
        };
        let Some(pending) = self.folder(maintenance, "pending", false).await? else {
            return Ok(false);
        };
        // Conservative: even a completed-but-unretired pending marker requires
        // its original deletion's recovery, not blind lock removal.
        Ok(!self.list(&pending, None).await?.is_empty())
    }

    async fn create_activity(&self, activity: &RepositoryActivity) -> Result<()> {
        let parent = self
            .activity_id
            .as_ref()
            .ok_or_else(|| invalid("activity folder disappeared"))?;
        self.put_new(
            parent,
            &format!("{}.enc", activity.claim_id),
            b"repository-activity-v1",
            activity,
        )
        .await
    }

    async fn remove_activity(&self, expected: &RepositoryActivity) -> Result<()> {
        let parent = self
            .activity_id
            .as_ref()
            .ok_or_else(|| invalid("activity folder disappeared"))?;
        let files = self
            .list(parent, Some(&format!("{}.enc", expected.claim_id)))
            .await?;
        let [file] = files.as_slice() else {
            return Err(invalid(
                "target activity is absent or ambiguous; inspect before continuing",
            ));
        };
        if self.read_activity(file).await? != *expected {
            return Err(invalid("authenticated activity changed before removal"));
        }
        // Managed claim IDs are immutable and execution-unique. The recovery
        // activity fences all other managed writers while this exact Drive ID
        // is removed; external/unfenced writers must be stopped by the human.
        let response = self
            .response(
                self.http
                    .delete(format!("{API}/{}", file.id))
                    .query(&[("supportsAllDrives", "true")]),
            )
            .await?;
        bounded(response, MAX_METADATA).await?;
        if !self.list(parent, Some(&file.name)).await?.is_empty() {
            return Err(invalid("activity absence could not be confirmed"));
        }
        Ok(())
    }

    async fn write_receipt(&self, receipt: &RecoveryReceipt) -> Result<()> {
        let maintenance = self
            .maintenance_id
            .as_ref()
            .ok_or_else(|| invalid("maintenance folder disappeared"))?;
        let parent = self
            .folder(maintenance, "lock-recoveries", true)
            .await?
            .ok_or_else(|| invalid("recovery receipt folder unavailable"))?;
        let phase = match receipt.phase {
            RecoveryPhase::Prepared => "prepared",
            RecoveryPhase::Complete => "complete",
        };
        self.put_new(
            &parent,
            &format!("{}.{phase}.enc", receipt.recovery_id),
            b"repository-lock-recovery-v1",
            receipt,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queries_and_object_ids_cannot_escape_the_metadata_namespace() {
        assert_eq!(escaped_query("a'b\\c"), "a\\'b\\\\c");
        for id in ["", "../packs", "id/other", "x?alt=media", "x\n"] {
            assert!(valid_id(id).is_err());
        }
        valid_id("safe_123-ID").unwrap();
    }
    #[test]
    fn missing_or_partial_listing_is_not_evidence_of_absence() {
        for bytes in [b"{}".as_slice(), b"{\"files\":[]}", b"{\"files\":"] {
            assert!(serde_json::from_slice::<Page>(bytes).is_err());
        }
        let page: Page =
            serde_json::from_slice(br#"{"files":[],"incompleteSearch":false}"#).unwrap();
        assert!(page.files.is_empty());
        assert!(!page.incomplete_search);
    }

    #[test]
    fn scoped_root_queries_accept_the_real_parent_id_without_weakening_concrete_traversal() {
        assert!(matches_parent("root", &["real-root-id".into()]));
        assert!(matches_parent("folder-id", &["folder-id".into()]));
        assert!(!matches_parent("folder-id", &["different-folder".into()]));
        assert!(!matches_parent("root", &[]));
        assert!(!matches_parent("root", &["one".into(), "two".into()]));
        assert!(!matches_parent("root", &["../untrusted".into()]));
    }
}
