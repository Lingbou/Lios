use std::path::Path;

use lios_core::catalog::CATALOG_FILE;
use lios_core::storage::StorageAdapter;
use lios_core::{LiosError, RemoteErrorKind};
use uuid::Uuid;

use crate::command_error::{CommandError, CommandErrorCode};

pub async fn ensure_space_can_initialize<A: StorageAdapter + ?Sized>(
    adapter: &A,
    namespace: &str,
    dataset: &str,
    probe_dir: &Path,
) -> Result<(), CommandError> {
    tokio::fs::create_dir_all(probe_dir).await?;
    let probe_path = probe_dir.join(format!(".catalog-probe-{}.tmp", Uuid::new_v4()));
    let result = adapter
        .download_object(namespace, dataset, CATALOG_FILE, &probe_path)
        .await;
    for cleanup_path in [&probe_path, &probe_path.with_extension("download")] {
        match tokio::fs::remove_file(cleanup_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    match result {
        Ok(()) => Err(CommandError::already_initialized(
            "space already contains catalog.enc",
        )),
        Err(LiosError::Remote(error)) if error.kind == RemoteErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn map_catalog_load_error(error: LiosError) -> CommandError {
    match error {
        LiosError::Remote(remote) if remote.kind == RemoteErrorKind::NotFound => CommandError::new(
            CommandErrorCode::NotInitialized,
            "space is not initialized",
            false,
            Some(serde_json::json!({
                "kind": remote.kind,
                "status": remote.status,
            })),
        ),
        error => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use async_trait::async_trait;
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use lios_core::modelscope::ModelScopeAdapter;
    use lios_core::storage::{StorageAdapter, StorageObject};
    use lios_core::{LiosError, RemoteError, RemoteErrorKind};
    use tempfile::tempdir;

    use super::{ensure_space_can_initialize, map_catalog_load_error};
    use crate::command_error::CommandErrorCode;

    struct InterruptedAdapter;

    #[async_trait]
    impl StorageAdapter for InterruptedAdapter {
        async fn create_repo(&self, _namespace: &str, _dataset: &str) -> lios_core::Result<()> {
            unreachable!()
        }

        async fn repo_exists(&self, _namespace: &str, _dataset: &str) -> lios_core::Result<bool> {
            unreachable!()
        }

        async fn list_objects(
            &self,
            _namespace: &str,
            _dataset: &str,
            _prefix: &str,
        ) -> lios_core::Result<Vec<StorageObject>> {
            unreachable!()
        }

        async fn upload_object(
            &self,
            _namespace: &str,
            _dataset: &str,
            _remote_path: &str,
            _local_path: &Path,
        ) -> lios_core::Result<()> {
            unreachable!()
        }

        async fn download_object(
            &self,
            _namespace: &str,
            _dataset: &str,
            _remote_path: &str,
            local_path: &Path,
        ) -> lios_core::Result<()> {
            tokio::fs::write(local_path.with_extension("download"), b"partial")
                .await
                .unwrap();
            Err(RemoteError::new(RemoteErrorKind::Network, None).into())
        }

        async fn delete_objects(
            &self,
            _namespace: &str,
            _dataset: &str,
            _remote_paths: &[String],
        ) -> lios_core::Result<()> {
            unreachable!()
        }

        async fn delete_prefix(
            &self,
            _namespace: &str,
            _dataset: &str,
            _prefix: &str,
        ) -> lios_core::Result<()> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn existing_catalog_refuses_initialization_and_cleans_probe() {
        let server = MockServer::start();
        let catalog = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/datasets/novix/cold/repo")
                .query_param("Revision", "master")
                .query_param("FilePath", "catalog.enc");
            then.status(200).body("encrypted catalog");
        });
        let probe_dir = tempdir().unwrap();

        let error = ensure_space_can_initialize(
            &ModelScopeAdapter::new(server.base_url(), "token"),
            "novix",
            "cold",
            probe_dir.path(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, CommandErrorCode::AlreadyInitialized);
        assert_eq!(probe_dir.path().read_dir().unwrap().count(), 0);
        catalog.assert();
    }

    #[tokio::test]
    async fn only_remote_not_found_permits_initialization() {
        let server = MockServer::start();
        let catalog = server.mock(|when, then| {
            when.method(GET).path("/api/v1/datasets/novix/cold/repo");
            then.status(404).body("missing");
        });
        let probe_dir = tempdir().unwrap();

        ensure_space_can_initialize(
            &ModelScopeAdapter::new(server.base_url(), "token"),
            "novix",
            "cold",
            probe_dir.path(),
        )
        .await
        .unwrap();

        assert_eq!(probe_dir.path().read_dir().unwrap().count(), 0);
        catalog.assert();
    }

    #[tokio::test]
    async fn auth_rate_limit_and_server_failures_do_not_initialize() {
        for (status, expected) in [
            (401, CommandErrorCode::Authentication),
            (429, CommandErrorCode::RateLimited),
            (503, CommandErrorCode::RemoteServer),
        ] {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/api/v1/datasets/novix/cold/repo");
                then.status(status).body("remote failure");
            });
            let probe_dir = tempdir().unwrap();

            let error = ensure_space_can_initialize(
                &ModelScopeAdapter::new(server.base_url(), "token"),
                "novix",
                "cold",
                probe_dir.path(),
            )
            .await
            .unwrap_err();

            assert_eq!(error.code, expected);
            assert_eq!(probe_dir.path().read_dir().unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn network_failure_does_not_initialize() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let probe_dir = tempdir().unwrap();

        let error = ensure_space_can_initialize(
            &ModelScopeAdapter::new(endpoint, "token"),
            "novix",
            "cold",
            probe_dir.path(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, CommandErrorCode::Network);
        assert!(error.retryable);
        assert_eq!(probe_dir.path().read_dir().unwrap().count(), 0);
    }

    #[tokio::test]
    async fn interrupted_probe_removes_download_sidecar() {
        let probe_dir = tempdir().unwrap();

        let error =
            ensure_space_can_initialize(&InterruptedAdapter, "novix", "cold", probe_dir.path())
                .await
                .unwrap_err();

        assert_eq!(error.code, CommandErrorCode::Network);
        assert_eq!(probe_dir.path().read_dir().unwrap().count(), 0);
    }

    #[test]
    fn catalog_load_maps_only_not_found_to_not_initialized() {
        let missing = map_catalog_load_error(LiosError::Remote(RemoteError::new(
            RemoteErrorKind::NotFound,
            Some(404),
        )));
        let auth = map_catalog_load_error(LiosError::Remote(RemoteError::new(
            RemoteErrorKind::Authentication,
            Some(401),
        )));

        assert_eq!(missing.code, CommandErrorCode::NotInitialized);
        assert_eq!(
            missing.details,
            Some(serde_json::json!({ "kind": "NotFound", "status": 404 }))
        );
        assert_eq!(auth.code, CommandErrorCode::Authentication);
    }
}
