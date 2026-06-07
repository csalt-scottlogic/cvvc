//! This module contains the code for dealing with Git's filesystem representation of loose refs (tags, and local and remote branches).

use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::anyhow;

use crate::{
    helpers::fs::{
        check_and_create_dir, path_translate, path_translate_rev, walk_fs, write_single_line,
    },
    stores::{BranchLocation, BranchSpec, RefSpec, RefStore, RefTarget, TagSpec, TargetedRef},
};

/// The git-compatible filesystem store for local and remote branch information.
///
/// At present this is the only branch store implementation provided as part of CVVC.
pub struct RefFileStore {
    base_path: PathBuf,
    local_branch_path: PathBuf,
    remote_branch_path: PathBuf,
    tag_path: PathBuf,
}

impl RefFileStore {
    /// Create a new branch file store.
    ///
    /// The base path of the branch store is the `.git` directory.
    pub fn new<P: AsRef<Path>>(base_path: P) -> Self {
        let ref_path = base_path.as_ref().join("refs");
        Self {
            base_path: base_path.as_ref().to_path_buf(),
            local_branch_path: ref_path.join("heads"),
            remote_branch_path: ref_path.join("remotes"),
            tag_path: ref_path.join("tags"),
        }
    }

    fn all_refs_in_path<P: AsRef<Path>>(&self, path: P) -> Result<Vec<RefSpec>, anyhow::Error> {
        let mut results: Vec<RefSpec> = vec![];
        for item in walk_fs(path)? {
            let item = item?;
            let item = item.strip_prefix(&self.base_path)?;
            let item = RefSpec::from_str(&path_translate(item))?;
            results.push(item);
        }
        Ok(results)
    }
}

impl RefStore for RefFileStore {
    /// Create a branch file store.
    ///
    /// This method is called on repository initialisation, and creates the necessary `refs/heads` and `refs/remotes`
    /// directories inside the `.git` directory.
    fn create(&self) -> Result<(), anyhow::Error> {
        check_and_create_dir(&self.local_branch_path)?;
        check_and_create_dir(&self.remote_branch_path)?;
        check_and_create_dir(&self.tag_path)?;
        Ok(())
    }

    /// Is the given branch specification valid?
    ///
    /// This method checks if the given branch specification points to a valid branch.  In other words,
    /// this checks if the branch specification can be interpreted as the name of an extant file on file filesystem.
    ///
    /// This method does not check if the head of the branch is itself valid.
    fn is_existing_ref(&self, branch: &RefSpec) -> Result<bool, anyhow::Error> {
        let ref_path = self.base_path.join(PathBuf::from(branch));
        Ok(ref_path.try_exists()? && ref_path.is_file())
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
        for dir_entry in walk_fs(&self.local_branch_path)? {
            let dir_entry = dir_entry?;
            results.push(BranchSpec::new(
                &path_translate(dir_entry.strip_prefix(&self.local_branch_path)?),
                BranchLocation::Local,
            ));
        }
        Ok(results)
    }

    /// Get the ID of the commit at the head of a branch, or the target of a tag ref
    ///
    /// On success, if the parameter is a valid branch, this method returns the commit ID at the
    /// head of that branch.  The branch may be local or remote.  If it is a pointer to another
    /// branch (as is normal for the special `HEAD` reference), it unpeels that pointer.
    ///
    /// This method follows symbolic references to return an object ID, but will throw an error
    /// if the symbolic reference is to an unborn branch.
    ///
    /// If the parameter is a thin tag, this method returns its target, but it does not unpeel
    /// chunky tags.
    ///
    /// The method does not check that the commit ID
    /// returned is a valid commit ID within the current repository.
    ///
    /// If the parameter does not represent a valid branch or tag, this method returns `Ok(None)`.
    ///
    /// This method may return an error, if any filesystem errors were encountered.
    fn resolve_target(&self, r: &RefSpec) -> Result<Option<RefTarget>, anyhow::Error> {
        let ref_path = self.base_path.join(PathBuf::from(r));
        if !(ref_path.exists() && ref_path.is_file()) {
            return Ok(None);
        }
        let ref_conts = fs::read_to_string(ref_path)?.trim().to_string();
        if let Some(nested_ref) = ref_conts.strip_prefix("ref: ") {
            self.resolve_target(&RefSpec::from_str(nested_ref.trim())?)
        } else {
            Ok(Some(RefTarget::Object(ref_conts)))
        }
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
        for dir_entry in fs::read_dir(&self.remote_branch_path)? {
            let dir_entry = dir_entry?;
            let file_type = dir_entry.file_type()?;
            if file_type.is_dir() {
                for rem_dir_entry in walk_fs(dir_entry.path())? {
                    let rem_dir_entry = rem_dir_entry?;
                    let found_name = path_translate(rem_dir_entry.strip_prefix(dir_entry.path())?);
                    if found_name == name {
                        results.push(BranchSpec::new(
                            &found_name,
                            BranchLocation::Remote(
                                dir_entry.file_name().to_string_lossy().to_string(),
                            ),
                        ));
                        break;
                    }
                }
            }
        }
        Ok(results)
    }

    /// Update the head of a branch to point to the given commit ID, creating the branch if it does not exist.
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

    fn tags(&self) -> Result<Vec<RefSpec>, anyhow::Error> {
        let mut results = Vec::<RefSpec>::new();
        for dir_entry in walk_fs(&self.tag_path)? {
            let dir_entry = dir_entry?;
            results.push(RefSpec::Tag(TagSpec::new(
                &path_translate(dir_entry.strip_prefix(&self.local_branch_path)?),
                false,
            )));
        }
        Ok(results)
    }

    fn create_ref(&self, r: &RefSpec, object_id: &str) -> Result<(), anyhow::Error> {
        let ref_path = self.base_path.join(PathBuf::from(r));
        check_and_create_dir(ref_path.parent().unwrap())?;
        write_single_line(ref_path, object_id)
    }

    fn all_refs(&self) -> Result<Vec<RefSpec>, anyhow::Error> {
        let mut results = vec![];
        results.append(&mut self.all_refs_in_path(&self.local_branch_path)?);
        results.append(&mut self.all_refs_in_path(&self.remote_branch_path)?);
        results.append(&mut self.all_refs_in_path(&self.tag_path)?);
        Ok(results)
    }

    fn all_ref_targets(&self) -> Result<Vec<TargetedRef>, anyhow::Error> {
        let refs = self.all_refs()?;
        let mut results: Vec<TargetedRef> = vec![];
        for item in refs {
            let Some(target) = self.resolve_target(&item)? else {
                return Err(anyhow!("ref has disappeared"));
            };
            results.push(TargetedRef { spec: item, target });
        }
        Ok(results)
    }
}

impl From<&BranchSpec> for PathBuf {
    /// Convert a [`BranchSpec`] to a [`PathBuf`] representing where the branch's information will be stored.
    ///
    /// This function returns a [`PathBuf`] relative to the repository base path.
    fn from(value: &BranchSpec) -> Self {
        let start = PathBuf::from(&value.location);
        start.join(path_translate_rev(&value.name))
    }
}

impl From<&BranchLocation> for PathBuf {
    /// Convert a [`BranchLocation`] to a [`PathBuf`] representing where this kind of branch's information will be
    /// stored.
    ///
    /// This function returns `refs/heads` for local branches and `refs/remotes` for remote branches.
    fn from(value: &BranchLocation) -> Self {
        let start = Path::new("refs");
        match value {
            BranchLocation::Local => start.join("heads"),
            BranchLocation::Remote(r) => start.join("remotes").join(r),
        }
    }
}

impl From<&RefSpec> for PathBuf {
    fn from(value: &RefSpec) -> Self {
        match value {
            RefSpec::Branch(branch_spec) => PathBuf::from(branch_spec),
            RefSpec::Tag(tag_spec) => PathBuf::from("refs").join("tags").join(&tag_spec.name),
            RefSpec::Head => PathBuf::from("HEAD"),
        }
    }
}
