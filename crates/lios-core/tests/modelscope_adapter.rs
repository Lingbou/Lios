use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;

use httpmock::Method::{GET, POST, PUT};
use httpmock::MockServer;
use lios_core::modelscope::ModelScopeAdapter;
use lios_core::storage::{
    BlobCheckpoint, BlobSpec, BlobValidation, RemoteAction, StorageAdapter,
    StorageTransactionError, ValidatedBlobUpload, MODELSCOPE_COMMIT_ACTION_LIMIT,
};
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

fn assert_transaction_error(error: LiosError, expected: StorageTransactionError) {
    let LiosError::StorageTransaction(error) = error else {
        panic!("expected storage transaction error, got {error:?}");
    };
    assert_eq!(error, expected);
}

fn start_upload_header_capture_server() -> (String, Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (headers_tx, headers_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or_default();
            if request.len() >= header_end + content_length {
                headers_tx.send(headers.into_owned()).unwrap();
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    (format!("http://{address}/blob/upload"), headers_rx, handle)
}

trait AmbiguousIfClone<A> {
    fn marker() {}
}

impl<T: ?Sized> AmbiguousIfClone<()> for T {}
impl<T: Clone> AmbiguousIfClone<u8> for T {}

#[test]
fn validated_blob_upload_is_not_cloneable() {
    let _ = <ValidatedBlobUpload as AmbiguousIfClone<_>>::marker;
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
async fn validate_65_blobs_uses_64_plus_1_requests_and_returns_every_oid() {
    let server = MockServer::start();
    let specs = (0..65)
        .map(|index| BlobSpec {
            local_path: PathBuf::from(format!("unused/{index}")),
            oid: format!("{index:064x}"),
            size: index as u64 + 1,
        })
        .collect::<Vec<_>>();
    let first_request = specs[..64]
        .iter()
        .map(|spec| json!({ "oid": spec.oid, "size": spec.size }))
        .collect::<Vec<_>>();
    let first_response = specs[..64]
        .iter()
        .map(|spec| json!({ "oid": spec.oid }))
        .collect::<Vec<_>>();
    let last_request = vec![json!({ "oid": specs[64].oid, "size": specs[64].size })];
    let last_response = vec![json!({ "oid": specs[64].oid })];
    let first_batch = server.mock(move |when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch")
            .json_body(json!({
                "operation": "upload",
                "objects": first_request
            }));
        then.status(200).json_body(json!({
            "Data": { "objects": first_response }
        }));
    });
    let last_batch = server.mock(move |when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch")
            .json_body(json!({
                "operation": "upload",
                "objects": last_request
            }));
        then.status(200).json_body(json!({
            "Data": { "objects": last_response }
        }));
    });

    let results = ModelScopeAdapter::new(server.base_url(), "token")
        .validate_blobs("novix", "cold", &specs)
        .await
        .unwrap();

    assert_eq!(results.len(), specs.len());
    for (result, spec) in results.iter().zip(&specs) {
        assert!(matches!(
            result,
            BlobValidation::Reusable(checkpoint)
                if checkpoint.oid == spec.oid && checkpoint.size == spec.size
        ));
    }
    first_batch.assert_hits(1);
    last_batch.assert_hits(1);
}

#[tokio::test]
async fn validate_blobs_rejects_a_missing_response_entry() {
    let server = MockServer::start();
    let specs = vec![
        BlobSpec {
            local_path: PathBuf::from("unused/a"),
            oid: format!("{:064x}", 1),
            size: 1,
        },
        BlobSpec {
            local_path: PathBuf::from("unused/b"),
            oid: format!("{:064x}", 2),
            size: 2,
        },
    ];
    let validate = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{ "oid": format!("{:064x}", 1) }]
            }
        }));
    });

    let error = ModelScopeAdapter::new(server.base_url(), "token")
        .validate_blobs("novix", "cold", &specs)
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::InvalidResponse, Some(200));
    validate.assert_hits(1);
}

#[tokio::test]
async fn validate_blobs_rejects_duplicate_and_unknown_response_oids() {
    let spec = BlobSpec {
        local_path: PathBuf::from("unused/strict-response"),
        oid: "d".repeat(64),
        size: 0,
    };
    let invalid_responses = [
        vec![
            json!({ "oid": spec.oid, "size": spec.size }),
            json!({ "oid": spec.oid, "size": spec.size }),
        ],
        vec![json!({ "oid": "e".repeat(64), "size": spec.size })],
    ];

    for objects in invalid_responses {
        let server = MockServer::start();
        let validate = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
            then.status(200)
                .json_body(json!({ "Data": { "objects": objects } }));
        });

        let error = ModelScopeAdapter::new(server.base_url(), "token")
            .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
            .await
            .unwrap_err();

        assert_remote_error(error, RemoteErrorKind::InvalidResponse, Some(200));
        validate.assert_hits(1);
    }
}

#[tokio::test]
async fn validate_blobs_rejects_per_object_error_fields() {
    let server = MockServer::start();
    let spec = BlobSpec {
        local_path: PathBuf::from("unused/error"),
        oid: "a".repeat(64),
        size: 0,
    };
    let validate = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{
                    "oid": spec.oid,
                    "size": spec.size,
                    "error": { "code": 422, "message": "rejected" }
                }]
            }
        }));
    });

    let error = ModelScopeAdapter::new(server.base_url(), "token")
        .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::InvalidResponse, Some(200));
    validate.assert_hits(1);
}

#[tokio::test]
async fn validate_blobs_rejects_returned_size_mismatch() {
    let server = MockServer::start();
    let spec = BlobSpec {
        local_path: PathBuf::from("unused/size"),
        oid: "b".repeat(64),
        size: 7,
    };
    let validate = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{ "oid": spec.oid, "size": spec.size + 1 }]
            }
        }));
    });

    let error = ModelScopeAdapter::new(server.base_url(), "token")
        .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::InvalidResponse, Some(200));
    validate.assert_hits(1);
}

#[tokio::test]
async fn validate_blobs_rejects_malformed_request_oid_before_network() {
    let server = MockServer::start();
    let validate = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
        then.status(200)
            .json_body(json!({ "Data": { "objects": [] } }));
    });
    let malformed_oid = "A".repeat(64);
    let spec = BlobSpec {
        local_path: PathBuf::from("unused/oid"),
        oid: malformed_oid.clone(),
        size: 0,
    };

    let error = ModelScopeAdapter::new(server.base_url(), "token")
        .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
        .await
        .unwrap_err();

    assert_transaction_error(
        error,
        StorageTransactionError::InvalidBlobOid(malformed_oid),
    );
    validate.assert_hits(0);
}

#[tokio::test]
async fn upload_blob_streams_exact_bytes_with_auth_and_content_length() {
    let server = MockServer::start();
    let tmp = tempdir().unwrap();
    let local_file = tmp.path().join("large-object.lios");
    let body = "streamed encrypted payload\n".repeat(80_000);
    fs::write(&local_file, body.as_bytes()).unwrap();
    let oid = hex::encode(Sha256::digest(body.as_bytes()));
    let spec = BlobSpec {
        local_path: local_file,
        oid: oid.clone(),
        size: body.len() as u64,
    };
    let upload_url = server.url("/blob/upload");
    let validate = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{
                    "oid": oid,
                    "actions": { "upload": { "href": upload_url } }
                }]
            }
        }));
    });
    let expected_length = body.len().to_string();
    let expected_body = body.clone();
    let upload = server.mock(move |when, then| {
        when.method(PUT)
            .path("/blob/upload")
            .header("authorization", "Bearer token")
            .header("cookie", "m_session_id=token")
            .header("content-length", expected_length.as_str())
            .body(expected_body.as_str());
        then.status(200);
    });

    let adapter = ModelScopeAdapter::new(server.base_url(), "token");
    let validation = adapter
        .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
        .await
        .unwrap()
        .pop()
        .unwrap();
    let BlobValidation::UploadRequired(validated) = validation else {
        panic!("expected an upload-required validation");
    };
    let checkpoint = adapter.upload_blob(&spec, validated).await.unwrap();

    assert_eq!(checkpoint.oid, spec.oid);
    assert_eq!(checkpoint.size, spec.size);
    validate.assert_hits(1);
    upload.assert_hits(1);
}

#[tokio::test]
async fn cross_origin_upload_omits_authorization_and_cookie_headers() {
    let api_server = MockServer::start();
    let (upload_url, headers_rx, upload_server) = start_upload_header_capture_server();
    let tmp = tempdir().unwrap();
    let local_file = tmp.path().join("cross-origin.lios");
    fs::write(&local_file, b"encrypted").unwrap();
    let spec = BlobSpec {
        local_path: local_file,
        oid: hex::encode(Sha256::digest(b"encrypted")),
        size: 9,
    };
    let validate = api_server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch")
            .header("authorization", "Bearer request-token");
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{
                    "oid": spec.oid,
                    "actions": { "upload": { "href": upload_url } }
                }]
            }
        }));
    });
    let adapter = ModelScopeAdapter::new(api_server.base_url(), "request-token");
    let validation = adapter
        .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
        .await
        .unwrap()
        .pop()
        .unwrap();
    let BlobValidation::UploadRequired(validated) = validation else {
        panic!("expected an upload-required validation");
    };

    adapter.upload_blob(&spec, validated).await.unwrap();
    let headers = headers_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let lower_headers = headers.to_ascii_lowercase();

    assert!(!lower_headers.contains("authorization:"), "{headers}");
    assert!(!lower_headers.contains("cookie:"), "{headers}");
    assert!(lower_headers.contains("content-length: 9"), "{headers}");
    assert!(lower_headers.contains("x-request-id:"), "{headers}");
    validate.assert_hits(1);
    upload_server.join().unwrap();
}

#[tokio::test]
async fn production_https_adapter_rejects_transferred_http_upload_target_before_put() {
    let api_server = MockServer::start();
    let upload_server = MockServer::start();
    let tmp = tempdir().unwrap();
    let local_file = tmp.path().join("transferred-http-target.lios");
    fs::write(&local_file, b"encrypted").unwrap();
    let spec = BlobSpec {
        local_path: local_file,
        oid: hex::encode(Sha256::digest(b"encrypted")),
        size: 9,
    };
    let upload_url = upload_server.url("/blob/should-not-run");
    let validate = api_server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{
                    "oid": spec.oid,
                    "actions": { "upload": { "href": upload_url } }
                }]
            }
        }));
    });
    let put = upload_server.mock(|when, then| {
        when.method(PUT).path("/blob/should-not-run");
        then.status(200);
    });
    let validation = ModelScopeAdapter::new(api_server.base_url(), "request-token")
        .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
        .await
        .unwrap()
        .pop()
        .unwrap();
    let BlobValidation::UploadRequired(validated) = validation else {
        panic!("expected an upload-required validation");
    };

    let error = ModelScopeAdapter::new("https://modelscope.cn", "request-token")
        .upload_blob(&spec, validated)
        .await
        .unwrap_err();

    assert_remote_error(error, RemoteErrorKind::InvalidResponse, Some(200));
    validate.assert_hits(1);
    put.assert_hits(0);
}

#[tokio::test]
async fn upload_blob_rejects_same_size_source_hash_mutation_before_put() {
    let server = MockServer::start();
    let tmp = tempdir().unwrap();
    let local_file = tmp.path().join("mutable-object.lios");
    let original = b"trusted-original";
    let mutated = b"changed-content!";
    assert_eq!(original.len(), mutated.len());
    fs::write(&local_file, original).unwrap();
    let spec = BlobSpec {
        local_path: local_file.clone(),
        oid: hex::encode(Sha256::digest(original)),
        size: original.len() as u64,
    };
    let upload_url = server.url("/blob/should-not-run?token=upload-secret");
    let validate = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{
                    "oid": spec.oid,
                    "actions": { "upload": { "href": upload_url } }
                }]
            }
        }));
    });
    let put = server.mock(|when, then| {
        when.method(PUT).path("/blob/should-not-run");
        then.status(200);
    });
    let adapter = ModelScopeAdapter::new(server.base_url(), "request-token");
    let validation = adapter
        .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
        .await
        .unwrap()
        .pop()
        .unwrap();
    let BlobValidation::UploadRequired(validated) = validation else {
        panic!("expected an upload-required validation");
    };

    fs::write(&local_file, mutated).unwrap();
    let error = adapter.upload_blob(&spec, validated).await.unwrap_err();

    assert!(matches!(error, LiosError::DataCorruption(_)));
    validate.assert_hits(1);
    put.assert_hits(0);
}

#[tokio::test]
async fn commit_actions_rejects_more_than_256_actions_before_request() {
    let actions = (0..=MODELSCOPE_COMMIT_ACTION_LIMIT)
        .map(|index| RemoteAction::delete(format!("objects/stale/{index:03}.enc")))
        .collect::<Vec<_>>();

    let error = ModelScopeAdapter::new("http://127.0.0.1:9", "token")
        .commit_actions("novix", "cold", "too many", &actions)
        .await
        .unwrap_err();

    assert_transaction_error(
        error,
        StorageTransactionError::CommitBatchTooLarge {
            actions: 257,
            limit: MODELSCOPE_COMMIT_ACTION_LIMIT,
        },
    );
}

#[tokio::test]
async fn commit_actions_rejects_invalid_or_conflicting_actions_before_request() {
    let server = MockServer::start();
    let commit = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/commit/master");
        then.status(200)
            .json_body(json!({ "Data": { "commit": "unexpected" } }));
    });
    let valid_oid = "c".repeat(64);
    let path = "objects/files/live/manifest.enc";
    let cases = vec![
        (
            vec![RemoteAction::lfs_upsert(
                path,
                BlobCheckpoint::new("C".repeat(64), 0),
            )],
            StorageTransactionError::InvalidBlobOid("C".repeat(64)),
        ),
        (
            vec![RemoteAction::lfs_upsert(
                "README.md",
                BlobCheckpoint::new(valid_oid.clone(), 0),
            )],
            StorageTransactionError::UnmanagedUploadPath("README.md".to_string()),
        ),
        (
            vec![RemoteAction::lfs_upsert(
                "objects/../README.md",
                BlobCheckpoint::new(valid_oid.clone(), 0),
            )],
            StorageTransactionError::UnmanagedUploadPath("objects/../README.md".to_string()),
        ),
        (
            vec![RemoteAction::delete("catalog.enc")],
            StorageTransactionError::UnmanagedDeletePath("catalog.enc".to_string()),
        ),
        (
            vec![
                RemoteAction::lfs_upsert(path, BlobCheckpoint::new(valid_oid.clone(), 0)),
                RemoteAction::lfs_upsert(path, BlobCheckpoint::new(valid_oid.clone(), 0)),
            ],
            StorageTransactionError::DuplicateActionPath(path.to_string()),
        ),
        (
            vec![
                RemoteAction::lfs_upsert(path, BlobCheckpoint::new(valid_oid.clone(), 0)),
                RemoteAction::delete(path),
            ],
            StorageTransactionError::ConflictingActionPath(path.to_string()),
        ),
    ];
    let adapter = ModelScopeAdapter::new(server.base_url(), "token");

    for (actions, expected) in cases {
        let error = adapter
            .commit_actions("novix", "cold", "invalid", &actions)
            .await
            .unwrap_err();
        assert_transaction_error(error, expected);
    }
    commit.assert_hits(0);
}

#[tokio::test]
async fn head_revision_parses_master_commit_id_from_revisions_endpoint() {
    let server = MockServer::start();
    let revisions = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/datasets/novix/cold/revisions")
            .header("authorization", "Bearer token");
        then.status(200).json_body(json!({
            "Data": {
                "RevisionMap": {
                    "Branches": [
                        { "Revision": "dev", "CommitId": "dev-commit" },
                        { "Revision": "master", "CommitId": "master-commit" }
                    ],
                    "Tags": []
                }
            }
        }));
    });

    let head = ModelScopeAdapter::new(server.base_url(), "token")
        .head_revision("novix", "cold")
        .await
        .unwrap();

    assert_eq!(head.branch, "master");
    assert_eq!(head.commit_id.as_deref(), Some("master-commit"));
    revisions.assert_hits(1);
}

#[tokio::test]
async fn head_revision_targets_the_configured_revision() {
    let server = MockServer::start();
    let revisions = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/datasets/novix/cold/revisions");
        then.status(200).json_body(json!({
            "Data": {
                "RevisionMap": {
                    "Branches": [
                        { "Revision": "master", "CommitId": "master-commit" },
                        { "Revision": "dev", "CommitId": "dev-commit" }
                    ]
                }
            }
        }));
    });

    let head = ModelScopeAdapter::new(server.base_url(), "token")
        .with_revision("dev")
        .head_revision("novix", "cold")
        .await
        .unwrap();

    assert_eq!(head.branch, "dev");
    assert_eq!(head.commit_id.as_deref(), Some("dev-commit"));
    revisions.assert_hits(1);
}

#[tokio::test]
async fn blob_upload_errors_do_not_expose_token_or_signed_url() {
    let server = MockServer::start();
    let tmp = tempdir().unwrap();
    let local_file = tmp.path().join("secret-error.lios");
    fs::write(&local_file, b"encrypted").unwrap();
    let spec = BlobSpec {
        local_path: local_file,
        oid: hex::encode(Sha256::digest(b"encrypted")),
        size: 9,
    };
    let upload_url =
        server.url("/blob/fail?X-Amz-Credential=signed-secret&token=upload-url-secret");
    let validate = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v1/repos/datasets/novix/cold/info/lfs/objects/batch");
        then.status(200).json_body(json!({
            "Data": {
                "objects": [{
                    "oid": spec.oid,
                    "actions": { "upload": { "href": upload_url } }
                }]
            }
        }));
    });
    let upload = server.mock(|when, then| {
        when.method(PUT).path("/blob/fail");
        then.status(500)
            .body("request-token upload-url-secret X-Amz-Credential=signed-secret");
    });
    let adapter = ModelScopeAdapter::new(server.base_url(), "request-token");
    let validation = adapter
        .validate_blobs("novix", "cold", std::slice::from_ref(&spec))
        .await
        .unwrap()
        .pop()
        .unwrap();
    let debug_validation = format!("{validation:?}");
    assert!(!debug_validation.contains("upload-url-secret"));
    assert!(!debug_validation.contains("X-Amz-Credential"));
    let BlobValidation::UploadRequired(validated) = validation else {
        panic!("expected an upload-required validation");
    };

    let error = adapter.upload_blob(&spec, validated).await.unwrap_err();
    let rendered = format!("{error:?} {error}");

    for secret in [
        "request-token",
        "upload-url-secret",
        "X-Amz-Credential",
        "signed-secret",
    ] {
        assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
    }
    validate.assert_hits(1);
    upload.assert_hits(1);
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
