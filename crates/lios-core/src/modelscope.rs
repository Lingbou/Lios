use std::collections::HashSet;
use std::path::Path;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{multipart, Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::storage::{StorageAdapter, StorageObject};
use crate::{LiosError, Result};

const DATASET_SEGMENT: &str = "datasets";
const DEFAULT_REVISION: &str = "master";
const PRIVATE_VISIBILITY: &str = "1";
const LIST_REPOS_PAGE_SIZE: u32 = 50;
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
        let repo_id = Self::repo_id(namespace, dataset);
        let response = self
            .auth(
                self.client
                    .get(self.api(&format!("{DATASET_SEGMENT}/{repo_id}/repo")))
                    .query(&[
                        ("Revision", self.revision.as_str()),
                        ("FilePath", remote_path),
                    ]),
            )
            .send()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let bytes = response
                .bytes()
                .await
                .map_err(|err| LiosError::Storage(err.to_string()))?;
            return Err(LiosError::Storage(
                String::from_utf8_lossy(&bytes).to_string(),
            ));
        }

        let temp_path = local_path.with_extension("download");
        let mut output = tokio::fs::File::create(&temp_path).await?;
        let mut written = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| LiosError::Storage(err.to_string()))?;
            output.write_all(&chunk).await?;
            written += chunk.len() as u64;
            on_progress(written);
        }
        output.flush().await?;
        drop(output);
        tokio::fs::rename(temp_path, local_path).await?;
        Ok(())
    }

    fn api(&self, path: &str) -> String {
        format!("{}/api/v1/{}", self.endpoint, path.trim_start_matches('/'))
    }

    fn repo_id(namespace: &str, dataset: &str) -> String {
        format!("{namespace}/{dataset}")
    }

    fn auth(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .bearer_auth(&self.token)
            .header("Cookie", format!("m_session_id={}", self.token))
            .header("X-Request-ID", Uuid::new_v4().simple().to_string())
    }

    async fn json_body(response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
        if !status.is_success() {
            return Err(LiosError::Storage(
                String::from_utf8_lossy(&bytes).to_string(),
            ));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        let body: Value = serde_json::from_slice(&bytes)?;
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

    async fn create_commit(
        &self,
        namespace: &str,
        dataset: &str,
        commit_message: String,
        actions: Vec<Value>,
    ) -> Result<()> {
        let repo_id = Self::repo_id(namespace, dataset);
        let response = self
            .auth(self.client.post(self.api(&format!(
                "repos/{DATASET_SEGMENT}/{repo_id}/commit/{}",
                self.revision
            ))))
            .json(&json!({
                "commit_message": commit_message,
                "actions": actions,
            }))
            .send()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
        Self::json_data(response).await?;
        Ok(())
    }

    async fn validate_blob(
        &self,
        namespace: &str,
        dataset: &str,
        oid: &str,
        size: u64,
    ) -> Result<Option<String>> {
        let repo_id = Self::repo_id(namespace, dataset);
        let response = self
            .auth(self.client.post(self.api(&format!(
                "repos/{DATASET_SEGMENT}/{repo_id}/info/lfs/objects/batch"
            ))))
            .json(&json!({
                "operation": "upload",
                "objects": [{ "oid": oid, "size": size }],
            }))
            .send()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
        let data = Self::json_data(response).await?;
        let objects = data
            .get("objects")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let href = objects
            .iter()
            .find(|object| object.get("oid").and_then(Value::as_str) == Some(oid))
            .and_then(|object| object.pointer("/actions/upload/href"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Ok(href)
    }

    async fn put_blob(&self, upload_url: &str, bytes: Vec<u8>) -> Result<()> {
        let response = self
            .client
            .put(upload_url)
            .bearer_auth(&self.token)
            .header("Cookie", format!("m_session_id={}", self.token))
            .header("X-Request-ID", Uuid::new_v4().simple().to_string())
            .header("Content-Length", bytes.len().to_string())
            .body(bytes)
            .send()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
        Self::json_data(response).await?;
        Ok(())
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
            .auth(self.client.post(self.api("login")))
            .json(&json!({ "AccessToken": self.token }))
            .send()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
        let data = Self::json_data(response).await?;
        let username = Self::value_string(&data, &["Username", "username", "Name", "name"])
            .ok_or_else(|| {
                LiosError::Storage("ModelScope username was not returned".to_string())
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
                .auth(self.client.get(self.api(DATASET_SEGMENT)).query(&params))
                .send()
                .await
                .map_err(|err| LiosError::Storage(err.to_string()))?;
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
            .auth(self.client.post(self.api(DATASET_SEGMENT)).multipart(form))
            .send()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
        let status = response.status();
        if status.is_success()
            || status == StatusCode::CONFLICT
            || self.repo_exists(namespace, dataset).await.unwrap_or(false)
        {
            Ok(())
        } else {
            Err(LiosError::Storage(
                response.text().await.unwrap_or_default(),
            ))
        }
    }

    async fn repo_exists(&self, namespace: &str, dataset: &str) -> Result<bool> {
        let repo_id = Self::repo_id(namespace, dataset);
        let response = self
            .auth(
                self.client
                    .get(self.api(&format!("{DATASET_SEGMENT}/{repo_id}"))),
            )
            .send()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
        match response.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            _ => Err(LiosError::Storage(
                response.text().await.unwrap_or_default(),
            )),
        }
    }

    async fn list_objects(
        &self,
        namespace: &str,
        dataset: &str,
        prefix: &str,
    ) -> Result<Vec<StorageObject>> {
        let repo_id = Self::repo_id(namespace, dataset);
        let response = self
            .auth(
                self.client
                    .get(self.api(&format!("{DATASET_SEGMENT}/{repo_id}/repo/tree")))
                    .query(&[
                        ("Revision", self.revision.as_str()),
                        ("Recursive", "true"),
                        ("Root", prefix),
                    ]),
            )
            .send()
            .await
            .map_err(|err| LiosError::Storage(err.to_string()))?;
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

    async fn upload_object(
        &self,
        namespace: &str,
        dataset: &str,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<()> {
        let bytes = tokio::fs::read(local_path).await?;
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let size = bytes.len() as u64;
        if let Some(upload_url) = self
            .validate_blob(namespace, dataset, &sha256, size)
            .await?
        {
            self.put_blob(&upload_url, bytes).await?;
        }
        self.create_commit(
            namespace,
            dataset,
            format!("Upload {remote_path}"),
            vec![json!({
                "action": "create",
                "path": remote_path,
                "type": "lfs",
                "size": size,
                "sha256": sha256,
                "content": "",
                "encoding": "",
            })],
        )
        .await
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

    async fn delete_objects(
        &self,
        namespace: &str,
        dataset: &str,
        remote_paths: &[String],
    ) -> Result<()> {
        let mut paths = remote_paths.to_vec();
        paths.sort();
        paths.dedup();
        let actions = paths
            .iter()
            .map(|path| {
                json!({
                    "action": "delete",
                    "path": path,
                    "type": "normal",
                    "size": 0,
                    "sha256": "",
                    "content": "",
                    "encoding": "",
                })
            })
            .collect::<Vec<_>>();
        if actions.is_empty() {
            return Ok(());
        }
        self.create_commit(
            namespace,
            dataset,
            "Delete stale snapshot objects".to_string(),
            actions,
        )
        .await
    }

    async fn delete_prefix(&self, namespace: &str, dataset: &str, prefix: &str) -> Result<()> {
        let objects = self.list_objects(namespace, dataset, prefix).await?;
        let actions = objects
            .into_iter()
            .filter(|object| object.path.starts_with(prefix))
            .map(|object| {
                json!({
                    "action": "delete",
                    "path": object.path,
                    "type": "normal",
                    "size": 0,
                    "sha256": "",
                    "content": "",
                    "encoding": "",
                })
            })
            .collect::<Vec<_>>();
        if actions.is_empty() {
            return Ok(());
        }
        self.create_commit(namespace, dataset, format!("Delete {prefix}"), actions)
            .await
    }
}
