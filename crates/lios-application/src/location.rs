//! Parsing for rsync-style local and explicit-Space operands.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::space_registry::validate_space_name;
use crate::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Location {
    Local(LocalLocation),
    Remote(RemoteLocation),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalLocation {
    pub path: PathBuf,
    pub trailing_slash: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteLocation {
    pub space_name: String,
    pub catalog_path: String,
    pub trailing_slash: bool,
}

impl Location {
    pub fn local(path: PathBuf, trailing_slash: bool) -> Self {
        Self::Local(LocalLocation {
            path,
            trailing_slash,
        })
    }

    pub fn remote(
        space_name: impl Into<String>,
        catalog_path: impl Into<String>,
        trailing_slash: bool,
    ) -> Self {
        Self::Remote(RemoteLocation {
            space_name: space_name.into(),
            catalog_path: catalog_path.into(),
            trailing_slash,
        })
    }
}

#[derive(Debug, Clone)]
pub struct LocationParser {
    registered_spaces: BTreeSet<String>,
}

impl LocationParser {
    pub fn new<I, S>(space_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            registered_spaces: space_names
                .into_iter()
                .map(|name| name.as_ref().to_string())
                .collect(),
        }
    }

    pub fn parse(&self, operand: &str) -> CommandResult<Location> {
        if operand.is_empty() {
            return Err(CommandError::invalid_input("path operand cannot be empty"));
        }
        if is_windows_drive_path(operand) || is_explicit_local_path(operand) {
            return Ok(parse_local(operand));
        }

        if let Some((name, path)) = operand.split_once(':') {
            if validate_space_name(name).is_ok() {
                if !self.registered_spaces.contains(name) {
                    return Err(CommandError::invalid_input(format!(
                        "space `{name}` is not registered"
                    )));
                }
                if !path.is_empty() && !path.starts_with('/') {
                    return Err(CommandError::invalid_input(
                        "remote Catalog paths must be absolute",
                    ));
                }
                if path.contains('\\') {
                    return Err(CommandError::invalid_input(
                        "remote Catalog paths must use forward slashes",
                    ));
                }
                let trailing_slash = path.ends_with('/');
                let catalog_path = if path.is_empty() {
                    "/"
                } else if trailing_slash && path.len() > 1 {
                    &path[..path.len() - 1]
                } else {
                    path
                };
                return Ok(Location::remote(name, catalog_path, trailing_slash));
            }
        }

        Ok(parse_local(operand))
    }
}

fn parse_local(operand: &str) -> Location {
    let trailing_slash = operand.ends_with('/') || operand.ends_with('\\');
    let path = if trailing_slash && operand.len() > 1 {
        PathBuf::from(&operand[..operand.len() - 1])
    } else {
        PathBuf::from(operand)
    };
    Location::local(path, trailing_slash)
}

fn is_windows_drive_path(operand: &str) -> bool {
    let bytes = operand.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_explicit_local_path(operand: &str) -> bool {
    operand.starts_with("./")
        || operand.starts_with("../")
        || operand.starts_with('/')
        || operand.starts_with(".\\")
        || operand.starts_with("..\\")
        || operand.starts_with("\\\\")
}
