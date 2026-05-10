//! This module contains the code for dealing with Git's filesystem representation of local and remote branches.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    helpers::fs::{
        check_and_create_dir, path_translate, path_translate_rev, walk_fs, write_single_line,
    },
    stores::{BranchKind, BranchSpec, BranchStore},
};

/// The git-compatible filesystem store for local and remote branch information.
///
/// At present this is the only branch store implementation provided as part of CVVC.
pub struct BranchFileStore {
    base_path: PathBuf,
}

impl BranchFileStore {
    /// Create a new branch file store.
    ///
    /// The base path of the branch store is the `.git` directory.
    pub fn new<P: AsRef<Path>>(base_path: P) -> Self {
        Self {
            base_path: base_path.as_ref().to_path_buf(),
        }
    }

    fn local_path(&self) -> PathBuf {
        self.base_path.join("refs").join("heads")
    }

    fn remote_path(&self) -> PathBuf {
        self.base_path.join("refs").join("heads")
    }
}

impl BranchStore for BranchFileStore {
    /// Create a branch file store.
    ///
    /// This method is called on repository initialisation, and creates the necessary `refs/heads` and `refs/remotes`
    /// directories inside the `.git` directory.
    fn create(&self) -> Result<(), anyhow::Error> {
        check_and_create_dir(self.local_path())?;
        check_and_create_dir(self.remote_path())?;
        Ok(())
    }

    /// Is the given branch specification valid?
    ///
    /// This method checks if the given branch specification points to a valid branch.  In other words,
    /// this checks if the branch specification can be interpreted as the name of an extant file on file filesystem.
    ///
    /// This method does not check if the head of the branch is itself valid.
    fn is_valid(&self, branch: &BranchSpec) -> Result<bool, anyhow::Error> {
        let branch_path = self.base_path.join(PathBuf::from(branch));
        Ok(branch_path.try_exists()? && branch_path.is_file())
    }

    /// List all of the local branches in the repository.
    ///
    /// On success, this method returns a [`Vec`] of all of the local branches currently stored in the repository.
    /// It does not check the branches themselves for validity --- in other words, it does not check that the head
    /// of each branch is a valid commit.
    ///
    /// This method may return an error, if any filesystem errors were encountered.
    fn local_branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error> {
        let mut results = Vec::<BranchSpec>::new();
        let local_path = self.local_path();
        for dir_entry in walk_fs(&local_path)? {
            let dir_entry = dir_entry?;
            results.push(BranchSpec::new(
                &path_translate(dir_entry.strip_prefix(&local_path)?),
                BranchKind::Local,
            ));
        }
        Ok(results)
    }

    /// Get the ID of the commit at the head of a branch
    ///
    /// On success, if the `branch` parameter is a valid branch, this method returns the commit ID at the
    /// head of that branch.  The branch may be local or remote. The method does not check that the commit ID
    /// returned is a valid commit ID within the current repository.
    ///
    /// If the `branch` parameter does not represent a valid branch, this method returns `Ok(None)`.
    ///
    /// This method may return an error, if any filesystem errors were encountered.
    fn resolve_branch_target(&self, branch: &BranchSpec) -> Result<Option<String>, anyhow::Error> {
        let branch_path = self.base_path.join(PathBuf::from(branch));
        if !branch_path.exists() || !branch_path.is_file() {
            return Ok(None);
        }
        let ref_conts = fs::read_to_string(branch_path)?.trim().to_string();
        Ok(Some(ref_conts))
    }

    /// Find which remote repositories (if any) contain a branch with the given name.
    ///
    /// This method searches the repository for remote branches with the given `name`.  On success,
    /// it returns a `Vec<BranchSpec>`, which will be empty if no remote branches with the given name
    /// are present in the repository.
    ///
    /// This method may return an error, if any filesystem errors were encountered.
    fn search_remotes_for_branch(&self, name: &str) -> Result<Vec<BranchSpec>, anyhow::Error> {
        let mut results = Vec::<BranchSpec>::new();
        for dir_entry in fs::read_dir(self.remote_path())? {
            let dir_entry = dir_entry?;
            let file_type = dir_entry.file_type()?;
            if file_type.is_dir() {
                for rem_dir_entry in walk_fs(dir_entry.path())? {
                    let rem_dir_entry = rem_dir_entry?;
                    let found_name = path_translate(rem_dir_entry.strip_prefix(dir_entry.path())?);
                    if found_name == name {
                        results.push(BranchSpec::new(
                            &found_name,
                            BranchKind::Remote(dir_entry.file_name().to_string_lossy().to_string()),
                        ));
                        break;
                    }
                }
            }
        }
        Ok(results)
    }

    /// Update the head of a branch to point to the given commit ID.
    ///
    /// This method updates either remote or local branches.  It does not confirm that the given ID is a valid
    /// commit ID within the repository.
    ///
    /// This method does not carry out any sort of pull operation or update the branch's ref log; it assumes that
    /// the calling code will be responsible for those actions.
    ///
    /// This method may return an error, if any filesystem errors were encountered.
    fn update_branch(&self, branch: &BranchSpec, commit_id: &str) -> Result<(), anyhow::Error> {
        let branch_path = self.base_path.join(PathBuf::from(branch));
        check_and_create_dir(branch_path.parent().unwrap())?;
        write_single_line(branch_path, commit_id)
    }
}

impl From<&BranchSpec> for PathBuf {
    /// Convert a [`BranchSpec`] to a [`PathBuf`] representing where the branch's information will be stored.
    ///
    /// This function returns a [`PathBuf`] relative to the repository base path.
    fn from(value: &BranchSpec) -> Self {
        let start = PathBuf::from(&value.kind);
        start.join(path_translate_rev(&value.name))
    }
}

impl From<&BranchKind> for PathBuf {
    /// Convert a [`BranchKind`] to a [`PathBuf`] representing where this kind of branch's information will be
    /// stored.
    ///
    /// This function returns `refs/heads` for local branches and `refs/remotes` for remote branches.
    fn from(value: &BranchKind) -> Self {
        let start = Path::new("refs");
        match value {
            BranchKind::Local => start.join("heads"),
            BranchKind::Remote(r) => start.join("remotes").join(r),
        }
    }
}
