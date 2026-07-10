use std::fs;

use httpmock::Method::{GET, POST, PUT};
use httpmock::MockServer;
use lios_core::modelscope::ModelScopeAdapter;
use lios_core::storage::StorageAdapter;
use lios_core::{LiosError, RemoteErrorKind};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn assert_remote_error(error: LiosError, kind: RemoteErrorKind, status: Option<u16>) {
    let LiosError::Remote(error) = error else {
        panic!("expected remote error, got {error:?}");
    };
    assert_eq!(error.kind, kind);
    assert_eq!(error.status, status);
}

#[tokio::test]
async fn catalog_download_preserves_not_found_status() {
    let server = MockServer::start();
    let tmp = tempdir().unwrap();
    let download = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/datasets/novix/cold/repo")
            .query_param("Revision", "master")
            .query_param("FilePath", "catalog.enc");
        then.status(404).body("catalog missing");
    });

    let adapter = ModelScopeAdapter::new(server.base_url(), "token");
    let error = adapter
        .download_object(
            "novix",
            "cold",
            "catalog.enc",
            &tmp.path().join("catalog.enc"),
        )
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::NotFound, Some(404));
    download.assert();
}

#[tokio::test]
async fn authentication_status_is_typed() {
    let server = MockServer::start();
    let login = server.mock(|when, then| {
        when.method(POST).path("/api/v1/login");
        then.status(401)
            .json_body(json!({ "Message": "invalid token" }));
    });

    let error = ModelScopeAdapter::new(server.base_url(), "secret-token")
        .whoami()
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::Authentication, Some(401));
    login.assert();
}

#[tokio::test]
async fn rate_limit_status_is_typed() {
    let server = MockServer::start();
    let list = server.mock(|when, then| {
        when.method(GET).path("/api/v1/datasets");
        then.status(429).body("slow down");
    });

    let error = ModelScopeAdapter::new(server.base_url(), "token")
        .list_dataset_repos_for_owner(Some("novix"))
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::RateLimited, Some(429));
    list.assert();
}

#[tokio::test]
async fn server_status_is_typed() {
    let server = MockServer::start();
    let exists = server.mock(|when, then| {
        when.method(GET).path("/api/v1/datasets/novix/cold");
        then.status(503).body("unavailable");
    });

    let error = ModelScopeAdapter::new(server.base_url(), "token")
        .repo_exists("novix", "cold")
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::Server, Some(503));
    exists.assert();
}

#[tokio::test]
async fn transport_failure_is_typed_as_network() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    let error = ModelScopeAdapter::new(endpoint, "token")
        .repo_exists("novix", "cold")
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::Network, None);
}

#[tokio::test]
async fn remote_response_body_secrets_are_never_retained() {
    let server = MockServer::start();
    let secret_fragments = [
        "ms-token-super-secret",
        "Authorization: Bearer",
        "Cookie: m_session_id",
        "X-Amz-Credential=signed-secret",
    ];
    server.mock(|when, then| {
        when.method(POST).path("/api/v1/login");
        then.status(401).body(
            "Authorization: Bearer ms-token-super-secret; Cookie: m_session_id=ms-token-super-secret; https://example.test/file?X-Amz-Credential=signed-secret",
        );
    });

    let error = ModelScopeAdapter::new(server.base_url(), "request-token")
        .whoami()
        .await
        .unwrap_err();
    let rendered = format!("{error:?} {error}");

    for secret in secret_fragments {
        assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
    }
}

#[tokio::test]
async fn transport_error_urls_are_never_retained() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let endpoint =
        format!("http://{address}/?token=ms-token-super-secret&X-Amz-Credential=signed-secret");

    let error = ModelScopeAdapter::new(endpoint, "request-token")
        .repo_exists("novix", "cold")
        .await
        .unwrap_err();
    let rendered = format!("{error:?} {error}");

    assert!(!rendered.contains("ms-token-super-secret"), "{rendered}");
    assert!(!rendered.contains("X-Amz-Credential"), "{rendered}");
    assert!(!rendered.contains("token="), "{rendered}");
}

#[tokio::test]
async fn create_repo_does_not_swallow_repo_exists_failure() {
    let server = MockServer::start();
    let create = server.mock(|when, then| {
        when.method(POST).path("/api/v1/datasets");
        then.status(400).body("create failed");
    });
    let exists = server.mock(|when, then| {
        when.method(GET).path("/api/v1/datasets/novix/cold");
        then.status(401).body("invalid token");
    });

    let error = ModelScopeAdapter::new(server.base_url(), "token")
        .create_repo("novix", "cold")
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::Authentication, Some(401));
    create.assert();
    exists.assert();
}

#[tokio::test]
async fn create_repo_is_idempotent_when_dataset_already_exists() {
    let server = MockServer::start();
    let create = server.mock(|when, then| {
        when.method(POST).path("/api/v1/datasets");
        then.status(400).json_body(json!({
            "Code": 10020101001_i64,
            "Message": "dataset already exists"
        }));
    });
    let exists = server.mock(|when, then| {
        when.method(GET).path("/api/v1/datasets/novix/cold");
        then.status(200)
            .json_body(json!({ "Data": { "Name": "cold" } }));
    });

    let adapter = ModelScopeAdapter::new(server.base_url(), "token");
    adapter.create_repo("novix", "cold").await.unwrap();

    create.assert();
    exists.assert();
}

#[tokio::test]
async fn whoami_uses_access_token_login() {
    let server = MockServer::start();
    let login = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/login")
            .header("authorization", "Bearer token")
            .json_body(json!({ "AccessToken": "token" }));
        then.status(200).json_body(json!({
            "Data": {
                "Username": "novix",
                "Email": "novix@example.test"
            }
        }));
    });

    let adapter = ModelScopeAdapter::new(server.base_url(), "token");
    let user = adapter.whoami().await.unwrap();

    assert_eq!(user.username, "novix");
    assert_eq!(user.email.as_deref(), Some("novix@example.test"));
    login.assert();
}

#[tokio::test]
async fn list_dataset_repos_uses_legacy_owner_listing_with_service_page_limit() {
    let server = MockServer::start();
    let list = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/datasets")
            .header("authorization", "Bearer token")
            .query_param("owner", "novix")
            .query_param("PageNumber", "1")
            .query_param("PageSize", "50");
        then.status(200).json_body(json!({
            "Data": [
                {
                    "Path": "novix",
                    "Name": "cold",
                    "Visibility": 1,
                    "UpdatedAt": "2026-07-08T08:00:00Z"
                },
                {
                    "id": "novix/archive",
                    "private": false,
                    "last_modified": "2026-07-08T09:00:00Z"
                }
            ],
            "TotalCount": 2,
            "PageNumber": 1,
            "PageSize": 50
        }));
    });

    let adapter = ModelScopeAdapter::new(server.base_url(), "token");
    let repos = adapter
        .list_dataset_repos_for_owner(Some("novix"))
        .await
        .unwrap();

    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0].namespace, "novix");
    assert_eq!(repos[0].dataset, "cold");
    assert_eq!(repos[0].endpoint, server.base_url());
    assert_eq!(repos[0].visibility.as_deref(), Some("private"));
    assert_eq!(repos[1].namespace, "novix");
    assert_eq!(repos[1].dataset, "archive");
    assert_eq!(repos[1].visibility.as_deref(), Some("public"));
    list.assert();
}

#[tokio::test]
async fn upload_object_uses_lfs_blob_then_commit_for_dataset_repo() {
    let server = MockServer::start();
    let tmp = tempdir().unwrap();
    let local_file = tmp.path().join("chunk.lios");
    fs::write(&local_file, b"encrypted chunk").unwrap();
    let sha = hex::encode(Sha256::digest(b"encrypted chunk"));
    let upload_url = server.url("/blob/upload-url");

    let validate = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch")
            .header("authorization", "Bearer token")
            .json_body(json!({
                "operation": "upload",
                "objects": [{ "oid": sha, "size": 15 }]
            }));
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{
                    "oid": sha,
                    "actions": { "upload": { "href": upload_url } }
                }]
            }
        }));
    });
    let upload = server.mock(|when, then| {
        when.method(PUT)
            .path("/blob/upload-url")
            .header("authorization", "Bearer token")
            .body("encrypted chunk");
        then.status(200);
    });
    let commit = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/commit/master")
            .json_body(json!({
                "commit_message": "Upload objects/file-a/chunk-000000.lios",
                "actions": [{
                    "action": "create",
                    "path": "objects/file-a/chunk-000000.lios",
                    "type": "lfs",
                    "size": 15,
                    "sha256": sha,
                    "content": "",
                    "encoding": ""
                }]
            }));
        then.status(200)
            .json_body(json!({ "Data": { "commit": "abc" } }));
    });

    let adapter = ModelScopeAdapter::new(server.base_url(), "token");
    adapter
        .upload_object(
            "novix",
            "cold",
            "objects/file-a/chunk-000000.lios",
            &local_file,
        )
        .await
        .unwrap();

    validate.assert();
    upload.assert();
    commit.assert();
}

#[tokio::test]
async fn list_download_and_delete_prefix_use_dataset_tree_and_commit_delete() {
    let server = MockServer::start();
    let tmp = tempdir().unwrap();
    let download_path = tmp.path().join("restore/chunk.lios");

    let list = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/datasets/novix/cold/repo/tree")
            .query_param("Revision", "master")
            .query_param("Recursive", "true")
            .query_param("Root", "objects/file-a");
        then.status(200).json_body(json!({
            "Data": [
                { "Path": "objects/file-a/chunk-000000.lios", "Size": 15, "Sha256": "abc" },
                { "Path": "objects/file-a/manifest.enc", "Size": 8 }
            ]
        }));
    });
    let download = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/datasets/novix/cold/repo")
            .query_param("Revision", "master")
            .query_param("FilePath", "objects/file-a/chunk-000000.lios");
        then.status(200).body("encrypted chunk");
    });
    let delete_commit = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/commit/master")
            .json_body(json!({
                "commit_message": "Delete objects/file-a",
                "actions": [
                    {
                        "action": "delete",
                        "path": "objects/file-a/chunk-000000.lios",
                        "type": "normal",
                        "size": 0,
                        "sha256": "",
                        "content": "",
                        "encoding": ""
                    },
                    {
                        "action": "delete",
                        "path": "objects/file-a/manifest.enc",
                        "type": "normal",
                        "size": 0,
                        "sha256": "",
                        "content": "",
                        "encoding": ""
                    }
                ]
            }));
        then.status(200)
            .json_body(json!({ "Data": { "commit": "def" } }));
    });

    let adapter = ModelScopeAdapter::new(server.base_url(), "token");
    let objects = adapter
        .list_objects("novix", "cold", "objects/file-a")
        .await
        .unwrap();
    adapter
        .download_object(
            "novix",
            "cold",
            "objects/file-a/chunk-000000.lios",
            &download_path,
        )
        .await
        .unwrap();
    adapter
        .delete_prefix("novix", "cold", "objects/file-a")
        .await
        .unwrap();

    assert_eq!(objects.len(), 2);
    assert_eq!(objects[0].path, "objects/file-a/chunk-000000.lios");
    assert_eq!(fs::read(&download_path).unwrap(), b"encrypted chunk");
    list.assert_hits(2);
    download.assert();
    delete_commit.assert();
}

#[tokio::test]
async fn download_object_with_progress_reports_written_bytes() {
    let server = MockServer::start();
    let tmp = tempdir().unwrap();
    let download_path = tmp.path().join("restore/chunk.lios");
    let body = b"encrypted chunk with enough bytes";
    let download = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/datasets/novix/cold/repo")
            .query_param("Revision", "master")
            .query_param("FilePath", "objects/file-a/chunk-000000.lios");
        then.status(200).body(body);
    });

    let adapter = ModelScopeAdapter::new(server.base_url(), "token");
    let mut events = Vec::new();
    adapter
        .download_object_with_progress(
            "novix",
            "cold",
            "objects/file-a/chunk-000000.lios",
            &download_path,
            |written| events.push(written),
        )
        .await
        .unwrap();

    assert_eq!(fs::read(&download_path).unwrap(), body);
    assert_eq!(events.last().copied(), Some(body.len() as u64));
    assert!(events.iter().all(|written| *written <= body.len() as u64));
    download.assert();
}
