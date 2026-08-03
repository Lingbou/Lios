//! Pure one-way transfer planning shared by dry runs and durable tasks.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{CommandError, CommandResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub kind: EntryKind,
    pub sha256: Option<String>,
    pub size: u64,
}

impl TreeEntry {
    pub fn file(path: impl Into<String>, sha256: impl Into<String>, size: u64) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::File,
            sha256: Some(sha256.into()),
            size,
        }
    }

    pub fn directory(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::Directory,
            sha256: None,
            size: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlanOptions {
    pub delete: bool,
    pub exclude: Vec<String>,
    pub replace_type: bool,
    pub yes: bool,
    pub no_clobber: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanActionKind {
    Create,
    Update,
    Skip,
    Delete,
    ReplaceType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAction {
    pub path: String,
    pub destination_path: String,
    pub kind: PlanActionKind,
    pub entry_kind: EntryKind,
    pub source_sha256: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPlan {
    pub actions: Vec<PlanAction>,
}

impl TransferPlan {
    pub fn action(&self, path: &str) -> Option<&PlanAction> {
        self.actions.iter().find(|action| action.path == path)
    }
}

pub struct TransferPlanner;

impl TransferPlanner {
    pub fn plan(
        source: &[TreeEntry],
        destination: &[TreeEntry],
        options: &PlanOptions,
    ) -> CommandResult<TransferPlan> {
        validate_entries(source, "source")?;
        validate_entries(destination, "destination")?;

        let destination_by_folded = destination
            .iter()
            .map(|entry| (fold_path(&entry.path), entry))
            .collect::<BTreeMap<_, _>>();
        let source_folded = source
            .iter()
            .map(|entry| fold_path(&entry.path))
            .collect::<BTreeSet<_>>();
        let mut actions = Vec::new();

        let mut ordered_source = source.iter().collect::<Vec<_>>();
        ordered_source.sort_by_key(|entry| (path_depth(&entry.path), entry.path.clone()));
        for entry in ordered_source {
            if is_excluded(&entry.path, &options.exclude) {
                continue;
            }
            let existing = destination_by_folded.get(&fold_path(&entry.path)).copied();
            let (kind, destination_path) = match existing {
                None => (PlanActionKind::Create, entry.path.clone()),
                Some(existing) if existing.kind != entry.kind => {
                    if !options.replace_type || !options.yes {
                        return Err(CommandError::invalid_input(format!(
                            "file/directory type conflict at `{}` requires --replace-type --yes",
                            entry.path
                        )));
                    }
                    (PlanActionKind::ReplaceType, existing.path.clone())
                }
                Some(existing)
                    if entry.kind == EntryKind::Directory || entry.sha256 == existing.sha256 =>
                {
                    (PlanActionKind::Skip, existing.path.clone())
                }
                Some(existing) if options.no_clobber => {
                    (PlanActionKind::Skip, existing.path.clone())
                }
                Some(existing) => (PlanActionKind::Update, existing.path.clone()),
            };
            actions.push(PlanAction {
                path: entry.path.clone(),
                destination_path,
                kind,
                entry_kind: entry.kind,
                source_sha256: entry.sha256.clone(),
                size: entry.size,
            });
        }

        if options.delete {
            let mut deletions = destination
                .iter()
                .filter(|entry| !source_folded.contains(&fold_path(&entry.path)))
                .filter(|entry| !delete_is_protected(&entry.path, destination, &options.exclude))
                .collect::<Vec<_>>();
            deletions.sort_by_key(|entry| {
                (
                    std::cmp::Reverse(path_depth(&entry.path)),
                    entry.path.clone(),
                )
            });
            actions.extend(deletions.into_iter().map(|entry| PlanAction {
                path: entry.path.clone(),
                destination_path: entry.path.clone(),
                kind: PlanActionKind::Delete,
                entry_kind: entry.kind,
                source_sha256: None,
                size: entry.size,
            }));
        }

        Ok(TransferPlan { actions })
    }

    pub fn map_source_path(root_name: &str, trailing_slash: bool, relative: &str) -> String {
        let relative = relative.trim_start_matches('/');
        if trailing_slash {
            relative.to_string()
        } else if relative.is_empty() {
            root_name.to_string()
        } else {
            format!("{root_name}/{relative}")
        }
    }
}

fn validate_entries(entries: &[TreeEntry], label: &str) -> CommandResult<()> {
    let mut seen = BTreeMap::<String, &str>::new();
    for entry in entries {
        validate_relative_catalog_path(&entry.path)?;
        if entry.kind == EntryKind::File && entry.sha256.as_deref().is_none_or(str::is_empty) {
            return Err(CommandError::invalid_input(format!(
                "{label} file `{}` has no SHA-256",
                entry.path
            )));
        }
        let folded = fold_path(&entry.path);
        if let Some(previous) = seen.insert(folded, &entry.path) {
            return Err(CommandError::invalid_input(format!(
                "{label} contains a case-insensitive path conflict between `{previous}` and `{}`",
                entry.path
            )));
        }
    }
    Ok(())
}

fn validate_relative_catalog_path(path: &str) -> CommandResult<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.contains('\\')
    {
        return Err(CommandError::invalid_input(format!(
            "invalid relative transfer path `{path}`"
        )));
    }
    Ok(())
}

fn fold_path(path: &str) -> String {
    path.to_ascii_lowercase()
}

fn path_depth(path: &str) -> usize {
    path.bytes().filter(|byte| *byte == b'/').count()
}

fn delete_is_protected(path: &str, destination: &[TreeEntry], patterns: &[String]) -> bool {
    is_excluded(path, patterns)
        || destination.iter().any(|entry| {
            entry.path.starts_with(&format!("{path}/")) && is_excluded(&entry.path, patterns)
        })
}

fn is_excluded(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| glob_matches(pattern, path))
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    wildcard_matches(pattern.as_bytes(), path.as_bytes())
}

fn wildcard_matches(pattern: &[u8], value: &[u8]) -> bool {
    match pattern.split_first() {
        None => value.is_empty(),
        Some((&b'*', rest)) => {
            wildcard_matches(rest, value)
                || (!value.is_empty() && wildcard_matches(pattern, &value[1..]))
        }
        Some((&b'?', rest)) => !value.is_empty() && wildcard_matches(rest, &value[1..]),
        Some((&expected, rest)) => value
            .split_first()
            .is_some_and(|(&actual, tail)| expected == actual && wildcard_matches(rest, tail)),
    }
}
