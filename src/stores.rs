use anyhow::anyhow;
use std::{fmt::Display, str::FromStr};

use crate::objects::{GitObject, RawObject, RawObjectData};

/// The store that records branch details using the filesystem.
pub mod branch_file_store;

/// The loose object store.
pub mod file_store;

/// The store which reads objects from packfiles.
pub mod pack_store;

/// The stores that store objects.
pub trait ObjectStore {
    /// Create an object store, if necessary.
    ///
    /// This function should create the store's permanent data structures, such as filesystem directories
    /// or database tables.  Implementations should provide their own `new()` function with appropriate
    /// parameters.
    fn create(&self) -> Result<(), anyhow::Error>;

    /// Indicate if this store is writeable or is read-only.
    fn _is_writeable(&self) -> bool;

    /// Search for objects using a whole or partial object ID, and return all those that match.
    ///
    /// This function should return `OK(vec![])` if no objects with the matching partial ID are found,
    /// rather than erroring.
    fn search_objects(&self, partial_object_id: &str) -> Result<Vec<String>, anyhow::Error>;

    /// Determine if a store holds a copy of a given object.
    fn has_object(&self, object_id: &str) -> Result<bool, anyhow::Error>;

    /// Read an object from the store.
    ///
    /// This function should return `Ok(None)` if the store does not contain the object,
    /// rather than erroring.
    fn read_raw_object(&self, object_id: &str) -> Result<Option<RawObjectData>, anyhow::Error>;

    /// Write a raw object to the store.
    fn write_raw_object(&self, obj: &RawObject) -> Result<String, anyhow::Error>;

    /// Write an object to the store.
    ///
    /// The default implementation is a convenience method which calls [`RawObject::from_git_object`]
    /// followed by [`Self::write_raw_object`].
    fn write_object(&self, obj: &impl GitObject) -> Result<String, anyhow::Error> {
        self.write_raw_object(&RawObject::from_git_object(obj))
    }
}

/// The stores that store branches.
pub trait BranchStore {
    /// Create a branch store, if necessary.
    ///
    /// This function should create the store's permanent data structures, such as filesystem directories.
    /// Implementations should provide their own `new()` function with appropriate parameters.
    fn create(&self) -> Result<(), anyhow::Error>;

    /// Determine whether a given branch exists in the store.
    fn is_valid(&self, branch: &BranchSpec) -> Result<bool, anyhow::Error>;

    /// List all of the local branches in the store.
    fn local_branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error>;

    /// Return the commit ID of the tip of the branch.
    ///
    /// This function should return `Ok(None)` if the branch does not exist rather than erroring.
    fn resolve_branch_target(&self, branch: &BranchSpec) -> Result<Option<String>, anyhow::Error>;

    /// Return all of the remote branches with the matching name.
    ///
    /// This function should return `Ok(vec![])` if no branches with the given name exist, rather than erroring.
    fn search_remotes_for_branch(&self, name: &str) -> Result<Vec<BranchSpec>, anyhow::Error>;

    /// Update the given branch to point to the given commit.
    ///
    /// This function is not required to confirm the commit is valid.
    fn update_branch(&self, branch: &BranchSpec, commit_id: &str) -> Result<(), anyhow::Error>;
}

/// Specifies if a branch is local, or if it is remote, which remote it belongs to.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum BranchKind {
    /// A local branch
    Local,

    /// A remote branch on a named remote.
    Remote(String),
}

impl Display for BranchKind {
    /// Format a `BranchKind` as a string.
    ///
    /// The output format matches the Unix path to the files which store branches of this kind,
    /// relative to the `.git` directory, as follows:
    /// - Local branches convert to `refs/heads`
    /// - Remote branches convert to `refs/remotes/<remote-name>`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refs/{}",
            match self {
                BranchKind::Local => "heads".to_string(),
                BranchKind::Remote(r) => format!("remotes/{r}"),
            }
        )
    }
}

impl FromStr for BranchKind {
    type Err = anyhow::Error;

    /// Convert a string to a `BranchKind`
    ///
    /// The inverse of [`BranchKind::fmt`], this function carries out the following conversion:
    /// - if the string starts `refs/heads/` it returns [`BranchKind::Local`]
    /// - if the string starts with `refs/remotes/` and contains at least one following character,
    ///   it returns [`BranchKind::Remote`], taking the string after the second `/` and up to (but not
    ///   including) the third `/` as the remote name.
    ///
    /// # Errors
    ///
    /// An error is returned in the following situations:
    /// - if the string doesn't start with `refs/heads/` or `refs/remotes/`
    /// - if the string starts with `refs/remotes//`
    /// - if the string consists solely of `refs/remotes/`
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("refs/heads/") {
            Ok(BranchKind::Local)
        } else if s.starts_with("refs/remotes/") {
            const START_LEN: usize = 13;
            let lim = s[START_LEN..]
                .find("/")
                .unwrap_or_else(|| s.len() - START_LEN)
                + START_LEN;
            if lim <= START_LEN + 1 {
                return Err(anyhow!("unrecognised branch format (no remote name)"));
            }
            Ok(BranchKind::Remote(s[START_LEN..lim].to_string()))
        } else {
            Err(anyhow!("unrecognised branch format"))
        }
    }
}

/// The definition of a branch.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct BranchSpec {
    // Correct behaviour of the branch-list command depends on the
    // ordering of members of this struct, so that the derived
    // Ord and PartialOrd implementations give the expected result
    /// Whether the branch is local or remote (and if remote, which remote it is on).
    pub kind: BranchKind,

    /// The branch name.
    pub name: String,
}

impl BranchSpec {
    /// Create a new [`BranchSpec`] object.
    pub fn new(name: &str, kind: BranchKind) -> Self {
        Self {
            name: name.to_string(),
            kind,
        }
    }
}

impl Display for BranchSpec {
    /// Format a [`BranchSpec`] as a string.
    ///
    /// The output format matches the Unix path to the files which store branches of this kind,
    /// relative to the `.git` directory, as follows:
    /// - for local branches, `refs/heads/<branch-name>`
    /// - for remote branches, `refs/remotes/<remote-name>/<branch-name>`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.kind, self.name)
    }
}

impl FromStr for BranchSpec {
    type Err = anyhow::Error;

    /// Convert a string to a [`BranchSpec`]
    ///
    /// This function is the inverse of [`BranchSpec::fmt`]. Acceptable input formats are:
    /// - for local branches, `refs/heads/<branch-name>`
    /// - for remote branches, `refs/remotes/<remote-name>/<branch-name>`
    ///
    /// Any other input will return an error.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let kind: BranchKind = s.parse()?;
        let start_idx = match &kind {
            BranchKind::Local => 11,
            BranchKind::Remote(r) => 14 + r.len(),
        };
        if s.len() <= start_idx {
            Err(anyhow!("No branch name given"))
        } else {
            Ok(Self {
                name: s[start_idx..].to_string(),
                kind,
            })
        }
    }
}
