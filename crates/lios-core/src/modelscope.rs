use std::collections::{HashMap, HashSet};
use std::io::SeekFrom;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{multipart, Body, Client, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::storage::{
    validate_blob_oid, validate_remote_actions, BlobCheckpoint, BlobSpec, BlobValidation,
    RemoteAction, RepoRevision, StorageAdapter, StorageObject, StorageTransactionError,
    ValidatedBlobUpload, MODELSCOPE_COMMIT_ACTION_LIMIT, MODELSCOPE_LFS_BATCH_SIZE,
};
use crate::{LiosError, RemoteError, RemoteErrorKind, Result};

const DATASET_SEGMENT: &str = "datasets";
const DEFAULT_REVISION: &str = "master";
const PRIVATE_VISIBILITY: &str = "1";
const LIST_REPOS_PAGE_SIZE: u32 = 50;
const BLOB_STREAM_BUFFER_SIZE: usize = 1024 * 1024;
const USER_AGENT: &str = concat!(
    "lios/",
    env!("CARGO_PKG_VERSION"),
    "; rust-reqwest; modelscope_hub-compatible"
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetRepoSummary {
    pub namespace: String,
    pub dataset: String,
    pub endpoint: String,
    pub visibility: Option<String>,
    pub updated_at: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelScopeUserSummary {
    pub username: String,
    pub email: Option<String>,
}

#[derive(Clone)]
pub struct ModelScopeAdapter {
    endpoint: String,
    token: String,
    client: Client,
    revision: String,
}

#[derive(Clone, Default)]
struct BlobStreamProgress {
    bytes: u64,
    hasher: Sha256,
    integrity_error: bool,
}

struct BlobStreamState {
    file: tokio::fs::File,
    expected_oid: String,
    expected_size: u64,
    progress: Arc<Mutex<BlobStreamProgress>>,
}

impl ModelScopeAdapter {
    pub fn new(endpoint: impl Into<String>, token: impl Into<String>) -> Self {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client configuration should be valid");
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token: token.into(),
            client,
            revision: DEFAULT_REVISION.to_string(),
        }
    }

    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = revision.into();
        self
    }

    pub async fn download_object_with_progress(
        &self,
        namespace: &str,
        dataset: &str,
        remote_path: &str,
        local_path: &Path,
        mut on_progress: impl FnMut(u64),
    ) -> Result<()> {
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let response = self
            .auth(
                self.client
                    .get(self.api_segments(&[DATASET_SEGMENT, namespace, dataset, "repo"]))
                    .query(&[
                        ("Revision", self.revision.as_str()),
                        ("FilePath", remote_path),
                    ]),
            )
            .send()
            .await
            .map_err(Self::network_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(Self::response_error(response).await);
        }

        let temp_path = local_path.with_extension("download");
        let mut output = tokio::fs::File::create(&temp_path).await?;
        let mut written = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(Self::network_error)?;
            output.write_all(&chunk).await?;
            written += chunk.len() as u64;
            on_progress(written);
        }
        output.flush().await?;
        drop(output);
        tokio::fs::rename(temp_path, local_path).await?;
        Ok(())
    }

    fn api_segments(&self, segments: &[&str]) -> Url {
        let mut url = Url::parse(&self.endpoint).expect("ModelScope endpoint must be a valid URL");
        url.set_query(None);
        url.set_fragment(None);
        let mut path = url
            .path_segments_mut()
            .expect("ModelScope endpoint must support path segments");
        path.clear().push("api").push("v1");
        path.extend(segments.iter().copied());
        drop(path);
        url
    }

    fn auth(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .bearer_auth(&self.token)
            .header("Cookie", format!("m_session_id={}", self.token))
            .header("X-Request-ID", Uuid::new_v4().simple().to_string())
    }

    fn parse_upload_target(&self, href: &str, status: StatusCode) -> Result<Url> {
        let upload_url = Url::parse(href).map_err(|_error| Self::invalid_response(status))?;
        if !matches!(upload_url.scheme(), "http" | "https") {
            return Err(Self::invalid_response(status));
        }
        let endpoint = Url::parse(&self.endpoint).expect("ModelScope endpoint must be a valid URL");
        if endpoint.scheme() == "https" && upload_url.scheme() != "https" {
            return Err(Self::invalid_response(status));
        }
        Ok(upload_url)
    }

    fn upload_target_receives_credentials(&self, upload_url: &Url) -> bool {
        let endpoint = Url::parse(&self.endpoint).expect("ModelScope endpoint must be a valid URL");
        if same_origin(&endpoint, upload_url) {
            return true;
        }
        is_production_modelscope_endpoint(&endpoint)
            && upload_url.scheme() == "https"
            && upload_url.port_or_known_default() == Some(443)
            && upload_url.host_str().is_some_and(is_modelscope_host)
    }

    fn network_error(_error: reqwest::Error) -> LiosError {
        RemoteError::new(RemoteErrorKind::Network, None).into()
    }

    async fn response_error(response: reqwest::Response) -> LiosError {
        let status = response.status();
        RemoteError::from_status(status.as_u16()).into()
    }

    async fn json_body(response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        if !status.is_success() {
            return Err(Self::response_error(response).await);
        }
        let bytes = response.bytes().await.map_err(Self::network_error)?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|_error| {
            RemoteError::new(RemoteErrorKind::InvalidResponse, Some(status.as_u16()))
        })?;
        Ok(body)
    }

    async fn json_data(response: reqwest::Response) -> Result<Value> {
        let body = Self::json_body(response).await?;
        Ok(body
            .get("Data")
            .or_else(|| body.get("data"))
            .cloned()
            .unwrap_or(body))
    }

    fn invalid_response(status: StatusCode) -> LiosError {
        RemoteError::new(RemoteErrorKind::InvalidResponse, Some(status.as_u16())).into()
    }

    async fn open_verified_blob_source(blob: &BlobSpec) -> Result<tokio::fs::File> {
        let mut file = tokio::fs::File::open(&blob.local_path).await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() || metadata.len() != blob.size {
            return Err(Self::blob_source_changed(&blob.local_path));
        }

        let mut hasher = Sha256::new();
        let mut hashed_bytes = 0u64;
        let mut buffer = vec![0u8; BLOB_STREAM_BUFFER_SIZE];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            hashed_bytes += read as u64;
            hasher.update(&buffer[..read]);
        }
        let final_metadata = file.metadata().await?;
        let actual_oid = hex::encode(hasher.finalize());
        if hashed_bytes != blob.size || final_metadata.len() != blob.size || actual_oid != blob.oid
        {
            return Err(Self::blob_source_changed(&blob.local_path));
        }
        file.seek(SeekFrom::Start(0)).await?;
        Ok(file)
    }

    fn blob_source_changed(path: &Path) -> LiosError {
        LiosError::DataCorruption(format!(
            "blob source changed before or during upload: {}",
            path.display()
        ))
    }

    fn parse_storage_object(value: &Value) -> Option<StorageObject> {
        let path = value
            .get("Path")
            .or_else(|| value.get("path"))
            .or_else(|| value.get("Name"))
            .or_else(|| value.get("name"))?
            .as_str()?
            .to_string();
        let size = value
            .get("Size")
            .or_else(|| value.get("size"))
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let sha256 = value
            .get("Sha256")
            .or_else(|| value.get("sha256"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Some(StorageObject { path, size, sha256 })
    }

    fn value_string(value: &Value, keys: &[&str]) -> Option<String> {
        keys.iter()
            .filter_map(|key| value.get(*key))
            .find_map(|item| match item {
                Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            })
    }

    fn value_usize(value: &Value, keys: &[&str]) -> Option<usize> {
        keys.iter()
            .filter_map(|key| value.get(*key))
            .find_map(|item| match item {
                Value::Number(number) => number.as_u64().map(|number| number as usize),
                Value::String(text) => text.parse::<usize>().ok(),
                _ => None,
            })
    }

    fn visibility_label(value: &Value) -> Option<String> {
        if let Some(private) = value.get("private").and_then(Value::as_bool) {
            return Some(if private { "private" } else { "public" }.to_string());
        }
        if let Some(gated) = value.get("gated").and_then(Value::as_bool) {
            if gated {
                return Some("private".to_string());
            }
        }
        value
            .get("Visibility")
            .or_else(|| value.get("visibility"))
            .and_then(|visibility| match visibility {
                Value::Number(number) => match number.as_i64() {
                    Some(1) => Some("private".to_string()),
                    Some(3) => Some("internal".to_string()),
                    Some(5) => Some("public".to_string()),
                    Some(other) => Some(other.to_string()),
                    None => None,
                },
                Value::String(text) if !text.trim().is_empty() => {
                    Some(text.trim().to_ascii_lowercase())
                }
                _ => None,
            })
    }

    fn paged_repo_items(data: &Value) -> (Vec<Value>, Option<usize>) {
        if let Some(items) = data.as_array() {
            return (items.clone(), Some(items.len()));
        }
        let Some(object) = data.as_object() else {
            return (Vec::new(), None);
        };
        let item_keys = [
            "Data", "data", "datasets", "Datasets", "items", "Items", "list", "List", "results",
            "Results",
        ];
        let items = item_keys
            .iter()
            .filter_map(|key| object.get(*key))
            .find_map(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let total = Self::value_usize(
            data,
            &["total_count", "TotalCount", "total", "Total", "totalCount"],
        );
        (items, total)
    }

    fn parse_dataset_repo(&self, value: &Value) -> Option<DatasetRepoSummary> {
        let path = Self::value_string(value, &["Path", "path"]);
        let id = Self::value_string(value, &["id", "Id", "repo_id", "repoId"])
            .or_else(|| path.clone().filter(|path| path.contains('/')));
        let (id_namespace, id_dataset) = id
            .as_deref()
            .and_then(|repo_id| repo_id.split_once('/'))
            .map(|(namespace, dataset)| (Some(namespace.to_string()), Some(dataset.to_string())))
            .unwrap_or((None, None));
        let namespace = Self::value_string(value, &["owner", "Owner", "namespace", "Namespace"])
            .or_else(|| path.filter(|path| !path.contains('/')))
            .or(id_namespace)?;
        let dataset =
            Self::value_string(value, &["name", "Name", "dataset", "repo_name", "repoName"])
                .or(id_dataset)?;
        Some(DatasetRepoSummary {
            namespace,
            dataset,
            endpoint: self.endpoint.clone(),
            visibility: Self::visibility_label(value),
            updated_at: Self::value_string(
                value,
                &["last_modified", "updated_at", "UpdatedAt", "LastModified"],
            ),
            description: Self::value_string(value, &["description", "Description"]),
        })
    }

    pub async fn whoami(&self) -> Result<ModelScopeUserSummary> {
        let response = self
            .auth(self.client.post(self.api_segments(&["login"])))
            .json(&json!({ "AccessToken": self.token }))
            .send()
            .await
            .map_err(Self::network_error)?;
        let data = Self::json_data(response).await?;
        let username = Self::value_string(&data, &["Username", "username", "Name", "name"])
            .ok_or_else(|| {
                RemoteError::new(
                    RemoteErrorKind::InvalidResponse,
                    Some(StatusCode::OK.as_u16()),
                )
            })?;
        let email = Self::value_string(&data, &["Email", "email"]);
        Ok(ModelScopeUserSummary { username, email })
    }

    pub async fn list_dataset_repos_for_owner(
        &self,
        owner: Option<&str>,
    ) -> Result<Vec<DatasetRepoSummary>> {
        let mut page_number = 1;
        let mut repos = Vec::new();
        let mut seen = HashSet::new();
        loop {
            let mut params = vec![
                ("PageNumber", page_number.to_string()),
                ("PageSize", LIST_REPOS_PAGE_SIZE.to_string()),
            ];
            if let Some(owner) = owner.filter(|owner| !owner.trim().is_empty()) {
                params.push(("owner", owner.trim().to_string()));
            }
            let response = self
                .auth(
                    self.client
                        .get(self.api_segments(&[DATASET_SEGMENT]))
                        .query(&params),
                )
                .send()
                .await
                .map_err(Self::network_error)?;
            let body = Self::json_body(response).await?;
            let (items, total) = Self::paged_repo_items(&body);
            let item_count = items.len();
            for item in items {
                if let Some(repo) = self.parse_dataset_repo(&item) {
                    if seen.insert((repo.namespace.clone(), repo.dataset.clone())) {
                        repos.push(repo);
                    }
                }
            }
            let reached_total = total.is_some_and(|total| repos.len() >= total);
            if item_count < LIST_REPOS_PAGE_SIZE as usize || reached_total {
                break;
            }
            page_number += 1;
        }
        Ok(repos)
    }

    pub async fn list_dataset_repos(&self) -> Result<Vec<DatasetRepoSummary>> {
        let user = self.whoami().await?;
        self.list_dataset_repos_for_owner(Some(&user.username))
            .await
    }
}

#[async_trait]
impl StorageAdapter for ModelScopeAdapter {
    async fn create_repo(&self, namespace: &str, dataset: &str) -> Result<()> {
        let form = multipart::Form::new()
            .text("Owner", namespace.to_string())
            .text("Name", dataset.to_string())
            .text("Visibility", PRIVATE_VISIBILITY.to_string())
            .text("License", "Apache-2.0".to_string());
        let response = self
            .auth(
                self.client
                    .post(self.api_segments(&[DATASET_SEGMENT]))
                    .multipart(form),
            )
            .send()
            .await
            .map_err(Self::network_error)?;
        let status = response.status();
        if status.is_success() || status == StatusCode::CONFLICT {
            return Ok(());
        }
        if self.repo_exists(namespace, dataset).await? {
            Ok(())
        } else {
            Err(Self::response_error(response).await)
        }
    }

    async fn repo_exists(&self, namespace: &str, dataset: &str) -> Result<bool> {
        let response = self
            .auth(
                self.client
                    .get(self.api_segments(&[DATASET_SEGMENT, namespace, dataset])),
            )
            .send()
            .await
            .map_err(Self::network_error)?;
        match response.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            _ => Err(Self::response_error(response).await),
        }
    }

    async fn head_revision(&self, namespace: &str, dataset: &str) -> Result<RepoRevision> {
        let response = self
            .auth(self.client.get(self.api_segments(&[
                DATASET_SEGMENT,
                namespace,
                dataset,
                "revisions",
            ])))
            .send()
            .await
            .map_err(Self::network_error)?;
        let status = response.status();
        let data = Self::json_data(response).await?;
        let revision_map = data
            .get("RevisionMap")
            .or_else(|| data.get("revision_map"))
            .ok_or_else(|| Self::invalid_response(status))?;
        let branches = revision_map
            .get("Branches")
            .or_else(|| revision_map.get("branches"))
            .and_then(Value::as_array)
            .ok_or_else(|| Self::invalid_response(status))?;
        let branch = branches
            .iter()
            .find_map(|branch| {
                let name = Self::value_string(branch, &["Revision", "revision", "Name", "name"])?;
                (name == self.revision).then_some((name, branch))
            })
            .ok_or_else(|| Self::invalid_response(status))?;
        let commit_id =
            Self::value_string(branch.1, &["CommitId", "commit_id", "commitId", "CommitID"]);
        Ok(RepoRevision {
            branch: branch.0,
            commit_id,
        })
    }

    async fn list_objects(
        &self,
        namespace: &str,
        dataset: &str,
        prefix: &str,
    ) -> Result<Vec<StorageObject>> {
        let response = self
            .auth(
                self.client
                    .get(self.api_segments(&[DATASET_SEGMENT, namespace, dataset, "repo", "tree"]))
                    .query(&[
                        ("Revision", self.revision.as_str()),
                        ("Recursive", "true"),
                        ("Root", prefix),
                    ]),
            )
            .send()
            .await
            .map_err(Self::network_error)?;
        let data = Self::json_data(response).await?;
        let files = if let Some(files) = data.as_array() {
            files.clone()
        } else {
            data.get("Files")
                .or_else(|| data.get("files"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        Ok(files
            .iter()
            .filter_map(Self::parse_storage_object)
            .collect())
    }

    async fn validate_blobs(
        &self,
        namespace: &str,
        dataset: &str,
        blobs: &[BlobSpec],
    ) -> Result<Vec<BlobValidation>> {
        for blob in blobs {
            validate_blob_oid(&blob.oid)?;
        }
        let mut seen = HashSet::with_capacity(blobs.len());
        if blobs.iter().any(|blob| !seen.insert(blob.oid.as_str())) {
            return Err(RemoteError::new(RemoteErrorKind::InvalidRequest, None).into());
        }

        let mut results = Vec::with_capacity(blobs.len());
        for batch in blobs.chunks(MODELSCOPE_LFS_BATCH_SIZE) {
            let objects = batch
                .iter()
                .map(|blob| json!({ "oid": blob.oid, "size": blob.size }))
                .collect::<Vec<_>>();
            let response = self
                .auth(self.client.post(self.api_segments(&[
                    "repos",
                    DATASET_SEGMENT,
                    namespace,
                    dataset,
                    "info",
                    "lfs",
                    "objects",
                    "batch",
                ])))
                .json(&json!({
                    "operation": "upload",
                    "objects": objects,
                }))
                .send()
                .await
                .map_err(Self::network_error)?;
            let status = response.status();
            let data = Self::json_data(response).await?;
            let response_objects = data
                .get("objects")
                .and_then(Value::as_array)
                .ok_or_else(|| Self::invalid_response(status))?;
            let requested = batch
                .iter()
                .map(|blob| (blob.oid.as_str(), blob))
                .collect::<HashMap<_, _>>();
            let mut parsed = HashMap::with_capacity(response_objects.len());

            for object in response_objects {
                if object.get("error").is_some() {
                    return Err(Self::invalid_response(status));
                }
                let oid = object
                    .get("oid")
                    .and_then(Value::as_str)
                    .filter(|oid| requested.contains_key(*oid))
                    .ok_or_else(|| Self::invalid_response(status))?;
                let blob = requested[oid];
                if let Some(returned_size) = object.get("size") {
                    if returned_size.as_u64() != Some(blob.size) {
                        return Err(Self::invalid_response(status));
                    }
                }
                let checkpoint = BlobCheckpoint::new(oid, blob.size);
                let validation = match object.get("actions") {
                    None => BlobValidation::Reusable(checkpoint),
                    Some(Value::Object(actions)) if actions.is_empty() => {
                        BlobValidation::Reusable(checkpoint)
                    }
                    Some(Value::Object(actions)) => {
                        let upload = actions
                            .get("upload")
                            .and_then(Value::as_object)
                            .ok_or_else(|| Self::invalid_response(status))?;
                        let href = upload
                            .get("href")
                            .and_then(Value::as_str)
                            .filter(|href| !href.trim().is_empty())
                            .ok_or_else(|| Self::invalid_response(status))?;
                        let upload_url = self.parse_upload_target(href, status)?;
                        BlobValidation::UploadRequired(ValidatedBlobUpload::new(
                            checkpoint,
                            upload_url.into(),
                        ))
                    }
                    Some(_) => return Err(Self::invalid_response(status)),
                };
                if parsed.insert(oid.to_string(), validation).is_some() {
                    return Err(Self::invalid_response(status));
                }
            }

            for blob in batch {
                results.push(
                    parsed
                        .remove(&blob.oid)
                        .ok_or_else(|| Self::invalid_response(status))?,
                );
            }
        }
        Ok(results)
    }

    async fn upload_blob(
        &self,
        blob: &BlobSpec,
        validated: ValidatedBlobUpload,
    ) -> Result<BlobCheckpoint> {
        let (checkpoint, upload_url) = validated.into_parts();
        validate_blob_oid(&blob.oid)?;
        validate_blob_oid(&checkpoint.oid)?;
        if checkpoint.oid != blob.oid || checkpoint.size != blob.size {
            return Err(LiosError::DataCorruption(
                "validated blob does not match the local blob specification".to_string(),
            ));
        }
        let file = Self::open_verified_blob_source(blob).await?;
        let progress = Arc::new(Mutex::new(BlobStreamProgress::default()));
        let stream = futures_util::stream::try_unfold(
            BlobStreamState {
                file,
                expected_oid: blob.oid.clone(),
                expected_size: blob.size,
                progress: Arc::clone(&progress),
            },
            |mut state| async move {
                let mut buffer = vec![0u8; BLOB_STREAM_BUFFER_SIZE];
                let read = state.file.read(&mut buffer).await?;
                if read == 0 {
                    let mut progress = state.progress.lock().map_err(|_error| {
                        std::io::Error::other("blob stream progress is unavailable")
                    })?;
                    let digest = hex::encode(progress.hasher.clone().finalize());
                    let mismatch =
                        progress.bytes != state.expected_size || digest != state.expected_oid;
                    progress.integrity_error = mismatch;
                    if mismatch {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "blob source changed during upload",
                        ));
                    }
                    return Ok(None);
                }

                {
                    let mut progress = state.progress.lock().map_err(|_error| {
                        std::io::Error::other("blob stream progress is unavailable")
                    })?;
                    progress.bytes += read as u64;
                    progress.hasher.update(&buffer[..read]);
                    if progress.bytes > state.expected_size {
                        progress.integrity_error = true;
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "blob source changed during upload",
                        ));
                    }
                }
                buffer.truncate(read);
                Ok(Some((buffer, state)))
            },
        );
        let upload_url = self.parse_upload_target(&upload_url, StatusCode::OK)?;
        let attach_credentials = self.upload_target_receives_credentials(&upload_url);
        let mut request = self
            .client
            .put(upload_url)
            .header("X-Request-ID", Uuid::new_v4().simple().to_string())
            .header("Content-Length", blob.size.to_string())
            .body(Body::wrap_stream(stream));
        if attach_credentials {
            request = request
                .bearer_auth(&self.token)
                .header("Cookie", format!("m_session_id={}", self.token));
        }
        let response = request.send().await;
        let stream_progress = progress
            .lock()
            .map_err(|_error| {
                LiosError::Storage("blob stream progress is unavailable".to_string())
            })?
            .clone();
        if stream_progress.integrity_error {
            return Err(Self::blob_source_changed(&blob.local_path));
        }
        let response = response.map_err(Self::network_error)?;
        let streamed_digest = hex::encode(stream_progress.hasher.finalize());
        if stream_progress.bytes != blob.size || streamed_digest != blob.oid {
            return Err(Self::blob_source_changed(&blob.local_path));
        }
        if !response.status().is_success() {
            return Err(Self::response_error(response).await);
        }
        Ok(checkpoint)
    }

    async fn commit_actions(
        &self,
        namespace: &str,
        dataset: &str,
        commit_message: &str,
        actions: &[RemoteAction],
    ) -> Result<()> {
        if actions.len() > MODELSCOPE_COMMIT_ACTION_LIMIT {
            return Err(StorageTransactionError::CommitBatchTooLarge {
                actions: actions.len(),
                limit: MODELSCOPE_COMMIT_ACTION_LIMIT,
            }
            .into());
        }
        validate_remote_actions(actions)?;
        if actions.is_empty() {
            return Ok(());
        }
        let response = self
            .auth(self.client.post(self.api_segments(&[
                "repos",
                DATASET_SEGMENT,
                namespace,
                dataset,
                "commit",
                &self.revision,
            ])))
            .json(&json!({
                "commit_message": commit_message,
                "actions": actions,
            }))
            .send()
            .await
            .map_err(Self::network_error)?;
        Self::json_data(response).await?;
        Ok(())
    }

    async fn download_object(
        &self,
        namespace: &str,
        dataset: &str,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<()> {
        self.download_object_with_progress(namespace, dataset, remote_path, local_path, |_| {})
            .await
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_production_modelscope_endpoint(endpoint: &Url) -> bool {
    endpoint.scheme() == "https"
        && endpoint.port_or_known_default() == Some(443)
        && matches!(
            endpoint.host_str(),
            Some("modelscope.cn" | "www.modelscope.cn")
        )
}

fn is_modelscope_host(host: &str) -> bool {
    host == "modelscope.cn" || host.ends_with(".modelscope.cn")
}

#[cfg(test)]
mod tests {
    use super::ModelScopeAdapter;
    use crate::{LiosError, RemoteErrorKind};
    use reqwest::Url;

    #[test]
    fn repository_identifiers_cannot_change_api_route() {
        let adapter = ModelScopeAdapter::new(
            "https://modelscope.cn/base?token=secret#fragment",
            "request-token",
        );

        let url = adapter.api_segments(&[
            "datasets",
            "team/../../admin?x=1#frag",
            "数据/../catalog",
            "repo",
        ]);

        assert_eq!(
            url.path(),
            "/api/v1/datasets/team%2F..%2F..%2Fadmin%3Fx=1%23frag/%E6%95%B0%E6%8D%AE%2F..%2Fcatalog/repo"
        );
        assert_eq!(url.query(), None);
        assert_eq!(url.fragment(), None);
    }

    #[test]
    fn https_endpoint_rejects_insecure_upload_target() {
        let adapter = ModelScopeAdapter::new("https://modelscope.cn", "request-token");

        let error = adapter
            .parse_upload_target("http://uploads.example.test/blob", reqwest::StatusCode::OK)
            .unwrap_err();

        let LiosError::Remote(error) = error else {
            panic!("expected typed remote error");
        };
        assert_eq!(error.kind, RemoteErrorKind::InvalidResponse);
        assert_eq!(error.status, Some(200));
    }

    #[test]
    fn cross_host_modelscope_credentials_require_an_official_endpoint() {
        let upload_url = Url::parse("https://uploads.modelscope.cn/blob").unwrap();

        assert!(
            ModelScopeAdapter::new("https://modelscope.cn", "request-token")
                .upload_target_receives_credentials(&upload_url)
        );
        assert!(
            ModelScopeAdapter::new("https://www.modelscope.cn", "request-token")
                .upload_target_receives_credentials(&upload_url)
        );
        assert!(
            !ModelScopeAdapter::new("https://staging.modelscope.cn", "request-token")
                .upload_target_receives_credentials(&upload_url)
        );
    }
}
