macro_rules! with_registered_commands {
    ($consumer:ident) => {
        $consumer! {
            current_setup,
            setup_token,
            create_dataset_repo,
            connect_dataset_repo,
            list_dataset_repos,
            select_dataset_repo,
            initialize_space,
            load_space_catalog,
            preview_upload_conflicts,
            enqueue_upload_to_folder,
            enqueue_download,
            enqueue_delete_nodes,
            enqueue_verify_space,
            preview_rebuild_catalog,
            enqueue_rebuild_catalog,
            rename_node,
            create_folder,
            search_catalog,
            export_recovery_key,
            verify_recovery_key,
            import_recovery_key,
            list_tasks,
            cleanup_local_cache,
            pause_task,
            resume_task,
            retry_task,
            cancel_task,
            clear_task,
        }
    };
}

pub(crate) use with_registered_commands;

macro_rules! define_registered_commands {
    ($($command:ident),* $(,)?) => {
        pub const REGISTERED_COMMANDS: &[&str] = &[$(stringify!($command)),*];
    };
}

with_registered_commands!(define_registered_commands);

#[cfg(test)]
mod tests {
    use super::REGISTERED_COMMANDS;

    #[test]
    fn registered_names_are_generated_from_handler_tokens() {
        macro_rules! collect_names {
            ($($command:ident),* $(,)?) => {
                &[$(stringify!($command)),*]
            };
        }

        let generated: &[&str] = super::with_registered_commands!(collect_names);

        assert_eq!(REGISTERED_COMMANDS, generated);
    }

    #[test]
    fn excludes_legacy_dangerous_commands() {
        for legacy in [
            "load_remote_catalog",
            "enqueue_upload",
            "enqueue_replace",
            "enqueue_delete",
            "enqueue_restore",
        ] {
            assert!(!REGISTERED_COMMANDS.contains(&legacy), "{legacy}");
        }
    }

    #[test]
    fn retains_node_scoped_commands() {
        for command in [
            "enqueue_upload_to_folder",
            "enqueue_delete_nodes",
            "enqueue_download",
            "enqueue_verify_space",
            "preview_rebuild_catalog",
            "enqueue_rebuild_catalog",
            "retry_task",
        ] {
            assert!(REGISTERED_COMMANDS.contains(&command), "{command}");
        }
    }

    #[test]
    fn registers_recovery_key_workflow_and_removes_obsolete_key_commands() {
        for command in [
            "export_recovery_key",
            "verify_recovery_key",
            "import_recovery_key",
        ] {
            assert!(REGISTERED_COMMANDS.contains(&command), "{command}");
        }
        for obsolete in ["generate_key_file", "import_key_file"] {
            assert!(!REGISTERED_COMMANDS.contains(&obsolete), "{obsolete}");
        }
    }
}
