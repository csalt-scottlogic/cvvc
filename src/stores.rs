use anyhow::anyhow;
use std::{fmt::Display, str::FromStr};

use crate::objects::{GitObject, RawObject};

pub mod branch_file_store;
pub mod file_store;
pub mod pack_store;

pub trait ObjectStore {
    fn create(&self) -> Result<(), anyhow::Error>;
    fn _is_writeable(&self) -> bool;
    fn search_objects(&self, partial_object_id: &str) -> Result<Vec<String>, anyhow::Error>;
    fn has_object(&self, object_id: &str) -> Result<bool, anyhow::Error>;
    fn read_object(&self, object_id: &str) -> Result<Option<RawObject>, anyhow::Error>;
    fn write_raw_object(&self, obj: &RawObject) -> Result<String, anyhow::Error>;

    fn write_object(&self, obj: &impl GitObject) -> Result<String, anyhow::Error> {
        self.write_raw_object(&RawObject::from_git_object(obj))
    }
}

pub trait BranchStore {
    fn create(&self) -> Result<(), anyhow::Error>;
    fn is_valid(&self, branch: &BranchSpec) -> Result<bool, anyhow::Error>;
    fn local_branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error>;
    fn resolve_branch_target(&self, branch: &BranchSpec) -> Result<Option<String>, anyhow::Error>;
    fn search_remotes_for_branch(&self, name: &str) -> Result<Vec<BranchSpec>, anyhow::Error>;
    fn update_branch(&self, branch: &BranchSpec, commit_id: &str) -> Result<(), anyhow::Error>;
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum BranchKind {
    Local,
    Remote(String)
}

impl Display for BranchKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refs/{}", match self {
            BranchKind::Local => "heads".to_string(),
            BranchKind::Remote(r) => format!("remotes/{r}"),
        })
    }
}

impl FromStr for BranchKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("refs/heads") {
            Ok(BranchKind::Local)
        } else if s.starts_with("refs/remotes/") {
            const START_LEN: usize = 13;
            let lim = s[START_LEN..].find("/").unwrap_or_else(|| s.len() - START_LEN) + START_LEN;
            if lim == 0 {
                return Err(anyhow!("unrecognised branch format (no remote name)"));
            }
            Ok(BranchKind::Remote(s[START_LEN..].to_string()))
        } else {
            Err(anyhow!("unrecognised branch format"))
        }
    }
}


#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct BranchSpec {
    // Correct behaviour of the branch-list command depends on the 
    // ordering of members of this struct, so that the derived
    // Ord and PartialOrd implementations give the expected result

    pub kind: BranchKind,
    pub name: String,
}

impl BranchSpec {
    pub fn new(name: &str, kind: BranchKind) -> Self {
        Self {
            name: name.to_string(),
            kind
        }
    }
}

impl Display for BranchSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.kind, self.name)
    }
}

impl FromStr for BranchSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let kind: BranchKind = s.parse()?;
        let start_idx = match &kind {
            BranchKind::Local => 11,
            BranchKind::Remote(r) => 14 + r.len()
        };
        Ok(Self {
            name: s[start_idx..].to_string(),
            kind
        })
    }
}
