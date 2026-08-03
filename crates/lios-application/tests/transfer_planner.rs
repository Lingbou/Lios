use lios_application::transfer_planner::{
    EntryKind, PlanActionKind, PlanOptions, TransferPlanner, TreeEntry,
};
use lios_application::CommandErrorCode;

fn file(path: &str, sha: &str) -> TreeEntry {
    TreeEntry::file(path, sha, 10)
}

fn dir(path: &str) -> TreeEntry {
    TreeEntry::directory(path)
}

#[test]
fn source_wins_identical_files_skip_and_destination_only_content_is_kept() {
    let source = vec![
        dir("docs"),
        file("docs/same.txt", "aaa"),
        file("new.txt", "bbb"),
    ];
    let destination = vec![
        dir("docs"),
        file("docs/same.txt", "aaa"),
        file("new.txt", "old"),
        file("keep.txt", "ccc"),
    ];

    let plan = TransferPlanner::plan(&source, &destination, &PlanOptions::default()).unwrap();

    assert_eq!(plan.action("docs").unwrap().kind, PlanActionKind::Skip);
    assert_eq!(
        plan.action("docs/same.txt").unwrap().kind,
        PlanActionKind::Skip
    );
    assert_eq!(plan.action("new.txt").unwrap().kind, PlanActionKind::Update);
    assert!(plan.action("keep.txt").is_none());
}

#[test]
fn delete_is_scoped_and_excluded_subtrees_are_protected() {
    let source = vec![dir("docs")];
    let destination = vec![
        dir("docs"),
        file("docs/old.txt", "aaa"),
        dir("cache"),
        file("cache/blob.bin", "bbb"),
    ];
    let options = PlanOptions {
        delete: true,
        exclude: vec!["cache/**".to_string()],
        ..PlanOptions::default()
    };

    let plan = TransferPlanner::plan(&source, &destination, &options).unwrap();

    assert_eq!(
        plan.action("docs/old.txt").unwrap().kind,
        PlanActionKind::Delete
    );
    assert!(plan.action("cache").is_none());
    assert!(plan.action("cache/blob.bin").is_none());
    assert_eq!(plan.actions.last().unwrap().kind, PlanActionKind::Delete);
}

#[test]
fn type_changes_require_explicit_authorization() {
    let source = vec![file("report", "aaa")];
    let destination = vec![TreeEntry {
        path: "report".to_string(),
        kind: EntryKind::Directory,
        sha256: None,
        size: 0,
    }];

    let error = TransferPlanner::plan(&source, &destination, &PlanOptions::default()).unwrap_err();
    assert_eq!(error.code, CommandErrorCode::InvalidInput);

    let plan = TransferPlanner::plan(
        &source,
        &destination,
        &PlanOptions {
            replace_type: true,
            yes: true,
            ..PlanOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        plan.action("report").unwrap().kind,
        PlanActionKind::ReplaceType
    );
}

#[test]
fn source_rejects_case_folded_name_collisions() {
    let error = TransferPlanner::plan(
        &[file("Docs/A.txt", "aaa"), file("docs/a.txt", "bbb")],
        &[],
        &PlanOptions::default(),
    )
    .unwrap_err();

    assert_eq!(error.code, CommandErrorCode::InvalidInput);
    assert!(error.message.contains("case"));
}

#[test]
fn trailing_slash_mapping_matches_rsync() {
    assert_eq!(
        TransferPlanner::map_source_path("dir", false, "nested/file.txt"),
        "dir/nested/file.txt"
    );
    assert_eq!(
        TransferPlanner::map_source_path("dir", true, "nested/file.txt"),
        "nested/file.txt"
    );
}
