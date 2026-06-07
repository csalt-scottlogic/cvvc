use std::{fmt::Display, str::FromStr};

use crate::{
    objects::{GitObject, RawObject, RawObjectData},
    stores::errors::InvalidRefNameError,
};

// The store that records ref details using the filesystem.
mod ref_file_store;

// The store that reads ref details from a packed ref file.
mod packed_ref_store;

// The store that can read ref details from either a loose store or a packed store.
mod combined_ref_store;
pub use combined_ref_store::CombinedRefStore;

// The loose object store.
mod file_store;
pub use file_store::LooseObjectStore;

// The store which reads objects from packfiles.
mod pack_store;
pub use pack_store::PackStore;

/// Standard error types for stores.
mod errors;

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
    fn read_raw_object_data(&self, object_id: &str)
        -> Result<Option<RawObjectData>, anyhow::Error>;

    /// Write a raw object to the store.
    fn write_raw_object(&self, obj: &RawObject) -> Result<String, anyhow::Error>;

    /// Write an object to the store.
    ///
    /// The default implementation is a convenience method which calls [`RawObject::from_git_object`]
    /// followed by [`Self::write_raw_object`].
    fn write_object(&self, obj: &impl GitObject) -> Result<String, anyhow::Error> {
        self.write_raw_object(&RawObject::from(obj))
    }
}

/// The stores that store branches.
pub trait RefStore {
    /// Create a ref store, if necessary.
    ///
    /// This function should create the store's permanent data structures, such as filesystem directories.
    /// Implementations should provide their own `new()` function with appropriate parameters.
    fn create(&self) -> Result<(), anyhow::Error>;

    /// Determine whether a given branch or tag exists in the store.
    fn is_existing_ref(&self, r: &RefSpec) -> Result<bool, anyhow::Error>;

    /// List all of the local branches in the store.
    fn local_branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error>;

    /// List all of the tags in the store.
    fn tags(&self) -> Result<Vec<RefSpec>, anyhow::Error>;

    /// List all of the refs in the store.
    fn all_refs(&self) -> Result<Vec<RefSpec>, anyhow::Error>;

    /// List all of the refs in the store, and their target objects.
    ///
    /// This method should return a vector of tuples, each tuple consisting of a [`RefSpec`], and the ID of
    /// the object it points to.
    ///
    /// This method should peel symbolic refs until it gets an object ID, but does not unpeel annotated tags.
    fn all_ref_targets(&self) -> Result<Vec<TargetedRef>, anyhow::Error>;

    /// Return the ID of the ref (the tip of the branch, or the tag).
    ///
    /// In the case of a chunky tag, this will be the ID of the tag object; it is the caller's
    /// responsibiity to peel it.
    ///
    /// This function should return `Ok(None)` if the branch does not exist rather than erroring.
    fn resolve_target(&self, r: &RefSpec) -> Result<Option<RefTarget>, anyhow::Error>;

    /// Return all of the remote branches with the matching name.
    ///
    /// This function should return `Ok(vec![])` if no branches with the given name exist, rather than erroring.
    fn search_remotes_for_branch(&self, name: &str) -> Result<Vec<BranchSpec>, anyhow::Error>;

    /// Create a new ref
    fn create_ref(&self, r: &RefSpec, object_id: &str) -> Result<(), anyhow::Error>;

    /// Update the given branch to point to the given commit.
    ///
    /// This function is not required to confirm the commit is valid.
    fn update_branch(&self, branch: &BranchSpec, commit_id: &str) -> Result<(), anyhow::Error>;
}

/// Specifies if a branch or tag is local, or if it is remote, which remote it belongs to.
#[derive(Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BranchLocation {
    /// A local branch or tag
    Local,

    /// A remote branch on a named remote.
    Remote(String),
}

impl Display for BranchLocation {
    /// Format a `BranchLocation` as a string.
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
                BranchLocation::Local => "heads".to_string(),
                BranchLocation::Remote(r) => format!("remotes/{r}"),
            }
        )
    }
}

impl FromStr for BranchLocation {
    type Err = InvalidRefNameError;

    /// Convert a string to a `BranchLocation`
    ///
    /// The inverse of [`BranchLocation::fmt`], this function carries out the following conversion:
    /// - if the string starts `refs/heads/` it returns [`BranchLocation::Local`]
    /// - if the string starts with `refs/remotes/` and contains at least one following character,
    ///   it returns [`BranchLocation::Remote`], taking the string after the second `/` and up to (but not
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
            Ok(BranchLocation::Local)
        } else if s.starts_with("refs/remotes/") {
            const START_LEN: usize = 13;
            let lim = s[START_LEN..]
                .find("/")
                .unwrap_or_else(|| s.len() - START_LEN)
                + START_LEN;
            if lim <= START_LEN + 1 {
                return Err(InvalidRefNameError::new(s));
            }
            Ok(BranchLocation::Remote(s[START_LEN..lim].to_string()))
        } else {
            Err(InvalidRefNameError::new(s))
        }
    }
}

/// The specification of a ref.
#[derive(Debug, Eq, Hash, PartialEq)]
pub enum RefSpec {
    /// A branch ref, either a local branch or a remote branch.
    Branch(BranchSpec),

    /// A tag ref.
    Tag(TagSpec),

    /// A symbolic reference to HEAD
    Head,
}

impl Display for RefSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefSpec::Tag(tag_name) => tag_name.fmt(f),
            RefSpec::Branch(branch_spec) => branch_spec.fmt(f),
            RefSpec::Head => write!(f, "HEAD"),
        }
    }
}

impl FromStr for RefSpec {
    type Err = InvalidRefNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "HEAD" {
            Ok(Self::Head)
        } else if s.starts_with("refs/tags/") {
            Ok(Self::Tag(TagSpec::from_str(s)?))
        } else if s.starts_with("refs/heads/") || s.starts_with("refs/remotes/") {
            Ok(Self::Branch(BranchSpec::from_str(s)?))
        } else {
            Err(InvalidRefNameError::new(s))
        }
    }
}

/// The definition of a branch.
#[derive(Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BranchSpec {
    // Correct behaviour of the branch-list command depends on the
    // ordering of members of this struct, so that the derived
    // Ord and PartialOrd implementations give the expected result
    /// Whether the branch is local or remote (and if remote, which remote it is on).
    pub location: BranchLocation,

    /// The branch name.
    pub name: String,
}

impl BranchSpec {
    /// Create a new [`BranchSpec`] object.
    pub fn new(name: &str, location: BranchLocation) -> Self {
        Self {
            name: name.to_string(),
            location,
        }
    }

    /// Convert this branch spec into a [`RefSpec`], consuming it.
    pub fn into_ref_spec(self) -> RefSpec {
        RefSpec::Branch(self)
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
        write!(f, "{}/{}", self.location, self.name)
    }
}

impl FromStr for BranchSpec {
    type Err = InvalidRefNameError;

    /// Convert a string to a [`BranchSpec`]
    ///
    /// This function is the inverse of [`BranchSpec::fmt`]. Acceptable input formats are:
    /// - for local branches, `refs/heads/<branch-name>`
    /// - for remote branches, `refs/remotes/<remote-name>/<branch-name>`
    ///
    /// Any other input will return an error.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let location: BranchLocation = s.parse()?;
        let start_idx = match &location {
            BranchLocation::Local => 11,
            BranchLocation::Remote(r) => 14 + r.len(),
        };
        if s.len() <= start_idx {
            Err(InvalidRefNameError::new(s))
        } else {
            Ok(Self {
                name: s[start_idx..].to_string(),
                location,
            })
        }
    }
}

/// The target of a reference.  This can be either an object ID, or a symbolic reference to another reference.
///
/// For example, the `HEAD` reference is normally a symbolic reference to a branch, but can be an object ID
/// when in "detached HEAD" mode.
#[derive(Eq, Hash, PartialEq)]
pub enum RefTarget {
    /// The reference target is a specific object ID.
    Object(String),

    /// The reference target is another reference.
    SymbolicRef(RefSpec),
}

impl Display for RefTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Object(id) => id.fmt(f),
            Self::SymbolicRef(spec) => write!(f, "ref: {spec}"),
        }
    }
}

impl FromStr for RefTarget {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_prefix("ref: ") {
            Some(rs) => Ok(Self::SymbolicRef(RefSpec::from_str(rs)?)),
            None => Ok(Self::Object(s.to_string())),
        }
    }
}

/// Contains a [`RefSpec`] and its current target.
#[derive(Eq, Hash, PartialEq)]
pub struct TargetedRef {
    /// The target of the [`RefSpec`].
    pub target: RefTarget,

    /// A [`RefSpec`] of any kind.
    pub spec: RefSpec,
}

impl Display for TargetedRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.target, self.spec)
    }
}

/// The definition of a tag.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TagSpec {
    /// The name of the tag.
    pub name: String,

    /// Whether or not this tag is peeled.
    ///
    /// This only really makes sense in the context of whether or not the tag is targeted,
    /// but in Git, this is part of the tag name, and therefore needs to be considered
    /// when the [`TagSpec`] is parsed.
    pub peeled: bool,
}

impl TagSpec {
    /// Create a new [`TagSpec`]
    pub fn new(name: &str, peeled: bool) -> Self {
        TagSpec {
            name: name.to_string(),
            peeled,
        }
    }

    /// Convert this [`TagSpec`] into a [`RefSpec`], consuming it.
    pub fn into_ref_spec(self) -> RefSpec {
        RefSpec::Tag(self)
    }
}

impl Display for TagSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let peeled_suffix = if self.peeled { "^{}" } else { "" };
        write!(f, "refs/tags/{}{}", self.name, peeled_suffix)
    }
}

impl FromStr for TagSpec {
    type Err = InvalidRefNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("refs/tags/")
            && s.len() > 10
            && !s[10..].starts_with("/")
            && !s.contains("//")
        {
            let stripped_name = s[10..].strip_suffix("^{}");
            match stripped_name {
                Some(n) => Ok(Self {
                    name: n.to_string(),
                    peeled: true,
                }),
                None => Ok(Self {
                    name: s[10..].to_string(),
                    peeled: false,
                }),
            }
        } else {
            Err(InvalidRefNameError::new(s))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::stores::errors::InvalidRefNameError;

    use super::{BranchLocation, BranchSpec, RefSpec, TagSpec};

    #[test]
    fn branch_location_fmt_local() {
        let test_object = BranchLocation::Local;

        let test_output = test_object.to_string();

        assert_eq!("refs/heads", test_output);
    }

    #[test]
    fn branch_location_fmt_remote() {
        let test_object = BranchLocation::Remote("example-origin".to_string());

        let test_output = test_object.to_string();

        assert_eq!("refs/remotes/example-origin", test_output);
    }

    #[test]
    fn branch_location_from_str_succeeds_for_valid_local() {
        let test_input = "refs/heads/test-branch";

        let test_output = BranchLocation::from_str(test_input).unwrap();

        assert_eq!(BranchLocation::Local, test_output);
    }

    #[test]
    fn branch_location_from_str_succeeds_for_valid_remote() {
        let test_input = "refs/remotes/test-remote/the/remote/branch";

        let test_output = BranchLocation::from_str(test_input).unwrap();

        assert_eq!(
            BranchLocation::Remote("test-remote".to_string()),
            test_output
        );
    }

    #[test]
    fn branch_location_from_str_fails_for_partial_remote() {
        let test_input = "refs/remotes/";

        let test_output = BranchLocation::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_location_from_str_fails_for_tags() {
        let test_input = "refs/tags/test-tag";

        let test_output = BranchLocation::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_location_from_str_fails_for_refs_alone() {
        let test_input = "refs";

        let test_output = BranchLocation::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_location_from_str_fails_for_nonsense() {
        let test_input = "zfymg";

        let test_output = BranchLocation::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_fmt_succeeds_for_tag() {
        let test_object = RefSpec::Tag(TagSpec {
            name: "example/tag".to_string(),
            peeled: false,
        });

        let test_output = test_object.to_string();

        assert_eq!("refs/tags/example/tag", test_output);
    }

    #[test]
    fn ref_spec_fmt_succeeds_for_local_branch() {
        let test_object = RefSpec::Branch(BranchSpec {
            location: BranchLocation::Local,
            name: "test-branch".to_string(),
        });

        let test_output = test_object.to_string();

        assert_eq!("refs/heads/test-branch", test_output);
    }

    #[test]
    fn ref_spec_fmt_succeeds_for_remote_branch() {
        let test_object = RefSpec::Branch(BranchSpec {
            location: BranchLocation::Remote("far".to_string()),
            name: "branch".to_string(),
        });

        let test_output = test_object.to_string();

        assert_eq!("refs/remotes/far/branch", test_output);
    }

    #[test]
    fn ref_spec_from_str_succeeds_for_valid_tag() {
        let test_input = "refs/tags/a/valid/tag-name";

        let test_output = RefSpec::from_str(test_input).unwrap();

        assert_eq!(
            RefSpec::Tag(TagSpec {
                name: "a/valid/tag-name".to_string(),
                peeled: false
            }),
            test_output
        );
    }

    #[test]
    fn ref_spec_from_str_succeeds_for_valid_local_branch() {
        let test_input = "refs/heads/the-branch";

        let test_output = RefSpec::from_str(test_input).unwrap();

        assert_eq!(
            RefSpec::Branch(BranchSpec {
                location: BranchLocation::Local,
                name: "the-branch".to_string()
            }),
            test_output
        );
    }

    #[test]
    fn ref_spec_from_str_succeeds_for_valid_remote_branch() {
        let test_input = "refs/remotes/test-remote/the/branch";

        let test_output = RefSpec::from_str(test_input).unwrap();

        assert_eq!(
            RefSpec::Branch(BranchSpec {
                location: BranchLocation::Remote("test-remote".to_string()),
                name: "the/branch".to_string()
            }),
            test_output
        );
    }

    #[test]
    fn ref_spec_from_str_fails_for_missing_tag_name() {
        let test_input = "refs/tags/";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_from_str_fails_for_tag_name_starting_with_slash() {
        let test_input = "refs/tags//the-tag";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_from_str_fails_for_tag_name_containing_double_slash() {
        let test_input = "refs/tags/the//tag";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_from_str_fails_for_local_branch_with_no_branch_name() {
        let test_input = "refs/heads/";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_from_str_fails_for_remote_branch_with_no_remote_name() {
        let test_input = "refs/remotes/";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_from_str_fails_for_remote_branch_with_no_branch_name() {
        let test_input = "refs/remotes/the-remote/";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_from_str_fails_for_refs_alone() {
        let test_input = "refs/";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_from_str_fails_for_refs_something() {
        let test_input = "refs/anything-invalid";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn ref_spec_from_str_fails_for_nonsense() {
        let test_input = "fejofejnmfvoweirtj";

        let test_output = RefSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_spec_new_succeeds_for_local() {
        let test_branch_name = "test-branch";

        let test_output = BranchSpec::new(test_branch_name, BranchLocation::Local);

        assert_eq!(test_output.name, test_branch_name);
        assert_eq!(BranchLocation::Local, test_output.location);
    }

    #[test]
    fn branch_spec_new_succeeds_for_remote() {
        let test_remote_name = "the-remote";
        let test_branch_name = "test-branch";

        let test_output = BranchSpec::new(
            test_branch_name,
            BranchLocation::Remote(test_remote_name.to_string()),
        );

        assert_eq!(test_output.name, test_branch_name);
        assert_eq!(
            BranchLocation::Remote(test_remote_name.to_string()),
            test_output.location
        );
    }

    #[test]
    fn branch_spec_into_ref_spec_succeeds() {
        let test_object = BranchSpec {
            name: "test-branch".to_string(),
            location: BranchLocation::Local,
        };

        let test_output = test_object.into_ref_spec();

        assert_eq!(
            RefSpec::Branch(BranchSpec {
                location: BranchLocation::Local,
                name: "test-branch".to_string()
            }),
            test_output
        );
    }

    #[test]
    fn branch_spec_fmt_succeeds_for_local() {
        let test_object = BranchSpec {
            name: "test-branch".to_string(),
            location: BranchLocation::Local,
        };

        let test_output = test_object.to_string();

        assert_eq!("refs/heads/test-branch", test_output);
    }

    #[test]
    fn branch_spec_fmt_succeeds_for_remote() {
        let test_object = BranchSpec {
            name: "the/branch".to_string(),
            location: BranchLocation::Remote("test-remote".to_string()),
        };

        let test_output = test_object.to_string();

        assert_eq!("refs/remotes/test-remote/the/branch", test_output);
    }

    #[test]
    fn branch_spec_from_str_succeeds_for_valid_local() {
        let test_input = "refs/heads/some/branch/name";

        let test_output = BranchSpec::from_str(test_input).unwrap();

        assert_eq!(
            BranchSpec {
                location: BranchLocation::Local,
                name: "some/branch/name".to_string()
            },
            test_output
        );
    }

    #[test]
    fn branch_spec_from_str_succeeds_for_valid_remote() {
        let test_input = "refs/remotes/some/branch/name";

        let test_output = BranchSpec::from_str(test_input).unwrap();

        assert_eq!(
            BranchSpec {
                location: BranchLocation::Remote("some".to_string()),
                name: "branch/name".to_string()
            },
            test_output
        );
    }

    #[test]
    fn branch_spec_from_str_fails_for_missing_local_branch_name() {
        let test_input = "refs/heads/";

        let test_output = BranchSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_spec_from_str_fails_for_missing_remote_branch_name() {
        let test_input = "refs/remotes/server/";

        let test_output = BranchSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_spec_from_str_fails_for_missing_remote_server_name() {
        let test_input = "refs/remotes/";

        let test_output = BranchSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_spec_from_str_fails_for_valid_tag() {
        let test_input = "refs/tags/the-tag";

        let test_output = BranchSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_spec_from_str_fails_for_valid_local_branch_with_initial_slash() {
        let test_input = "/refs/heads/the-branch";

        let test_output = BranchSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }

    #[test]
    fn branch_spec_from_str_fails_for_nonsense() {
        let test_input = "fmewiofjwpvita";

        let test_output = BranchSpec::from_str(test_input).unwrap_err();

        assert_eq!(InvalidRefNameError::new(test_input), test_output);
    }
}
