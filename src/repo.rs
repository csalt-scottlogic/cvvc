use anyhow::{anyhow, Context};
use chrono::{Local, Utc};
use indexmap::IndexMap;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::{
    config::{RemoteInfo, RepoConfig},
    helpers::{
        add_parent_dirs_to_map_of_vecs, add_to_map_of_vecs,
        fs::{
            check_and_create_dir, index_path_file, index_path_parent, path_translate,
            write_single_line, PathError, PathErrorKind,
        },
        timestamped_name,
    },
    ignore::IgnoreInfo,
    index::{Index, IndexEntry},
    objects::{
        Blob, FindObjectError, GitObject, ObjectKind, RawObject, RawObjectData, StoredObject, Tree,
        TreeNode,
    },
    output::OutputService,
    ref_log::{RefLog, RefLogEntry},
    stores::{
        BranchLocation, BranchSpec, CombinedRefStore, LooseObjectStore, ObjectStore, PackStore,
        RefSpec, RefStore, RefTarget, TagSpec, TargetedRef,
    },
};

/// A Git/CVVC repository.
///
/// The repository need not exist on disk.  If it does not, the [`Repository::create`] method will create a minimal empty repository
/// at the given path.
pub struct Repository {
    /// The canonical path of the repository root.
    pub worktree: PathBuf,

    /// The canonical path of the repository's backend directory.  Conventionally this is `<worktree>/.git`.
    pub git_dir: PathBuf,

    loose_object_store: LooseObjectStore,
    packfile_base: PathBuf,
    packs: Vec<PackStore>,
    ref_store: CombinedRefStore,
    ref_log_store: RefLog,
    config: RepoConfig,
}

impl Repository {
    /// If the given path is within the worktree or `.git` directory of a repository, this method returns a [`Repository`]
    /// object describing it.  If not, this method returns `None`.
    ///
    /// This function checks if the current directory, or any of its parents, contain a `.git` directory.  If so, it assumes that
    /// the parent of the `.git` directory is the root of the repository worktree.
    ///
    /// # Limitations
    ///
    /// CVVC does not currently support multiple worktrees for a repository.  If you call this function for a path within a linked
    /// worktree of a repository, CVVC will assume that it has found a main worktree, and will error.
    ///
    /// # Errors
    ///
    /// This function will error if the `path` argument is not a legal path or cannot be canonicalised.
    ///
    /// This function will also error if it determines the path is within a corrupt repository.  See the documentation for
    /// [`Repository::new`] for details of the sanity checks carried out.
    pub fn find<P: AsRef<Path>>(
        path: P,
        printer: &dyn OutputService,
    ) -> Result<Option<Self>, anyhow::Error> {
        let path_buf = path.as_ref().canonicalize()?;
        if path_buf.join(Path::new(".git")).is_dir() {
            return Ok(Some(Self::new(path_buf, printer)?));
        }
        match path_buf.parent() {
            Some(p) => Self::find(p, printer),
            None => Ok(None),
        }
    }

    /// This function tries to determine if the process's current working directory is inside a repository, and returns a [`Repository`]
    /// object if it is, or `None` if it is not.
    ///
    /// See the [`Repository::find`] function for further information.
    pub fn find_cwd(printer: &dyn OutputService) -> Result<Option<Self>, anyhow::Error> {
        Self::find(env::current_dir()?, printer)
    }

    /// Create a new [`Repository`] object representing a repository at a given filesystem path.  
    ///
    /// This function also carries out some basic sanity checks on the repository,
    /// and validate that the repository does not use Git extensions that CVVC doesn't support.
    ///
    /// The sanity checks consist of checking that:
    /// - the path is a valid path which can be canonicalised.
    /// - the root path contains a `.git` directory.
    /// - the file `.git/config` must be a syntactically valid config file.
    /// - the config file must contain a `[core]` section.
    /// - the config file must contain a `core.repositoryformatversion` setting equal to zero.
    ///
    /// If the repository cannot be read due to permissions errors, the function will return errors implying that the
    /// repository does not exist.
    pub fn new<P: AsRef<Path>>(
        worktree: P,
        printer: &dyn OutputService,
    ) -> Result<Self, anyhow::Error> {
        Self::new_impl(worktree, false, printer)
    }

    fn new_impl<P: AsRef<Path>>(
        worktree: P,
        allow_invalid: bool,
        printer: &dyn OutputService,
    ) -> Result<Self, anyhow::Error> {
        let worktree = worktree.as_ref().canonicalize()?;
        let git_dir = worktree.join(Path::new(".git"));
        if !(allow_invalid || git_dir.is_dir()) {
            return Err(anyhow!("Not a git directory"));
        }
        let config_path = git_dir.join("config");
        if (!allow_invalid) && (!config_path.is_file()) {
            return Err(anyhow!("configuration file missing"));
        }
        let config = RepoConfig::new(config_path);
        let version = config.version()?;
        if version != 0 {
            return Err(anyhow!("unsupported repository format version {version}"));
        }

        let loose_store_path = git_dir.join("objects");
        let loose_object_store = LooseObjectStore::new(&loose_store_path)?;
        let packed_refs_path = git_dir.join("packed-refs");
        let packed_refs_path = if packed_refs_path.exists() {
            Some(packed_refs_path)
        } else {
            None
        };
        let ref_store = CombinedRefStore::new(&git_dir, packed_refs_path)?;
        let ref_log_store = RefLog::new(git_dir.join("logs"));

        let pack_dir = git_dir.join("objects").join("pack");
        let packs = if pack_dir.is_dir() {
            PackStore::find_packs(&pack_dir, printer)?
        } else {
            vec![]
        };

        Ok(Repository {
            worktree,
            git_dir,
            loose_object_store,
            ref_store,
            ref_log_store,
            packfile_base: pack_dir.clone(),
            packs,
            config,
        })
    }

    /// Create a minimal empty repository at a given path, if one does already exist.
    ///
    /// If the path already exists, this function will attempt to turn it into a repository.
    /// If the path does not already exist, this function tries to create it, including its ancestor
    /// directories.
    ///
    /// Inside the `.git` directory, this function will create the following directories:
    /// - `logs`
    /// - `objects`
    /// - `objects/pack`
    /// - `refs`
    /// - `refs/heads`
    /// - `refs/remotes`
    /// - `refs/tags`
    ///
    /// If these succeed, it then creates:
    /// - a minimal `description` file
    /// - a minimal `config` file
    /// - a `HEAD` file pointing to a (nonexistent) `main` branch.
    ///
    /// # Errors
    ///
    /// An error is returned if this function encounters any filesystem errors, or if any of the
    /// following applies:
    /// - the path exists but is not a directory
    /// - a file named `.git` exists in the path directory.
    pub fn create<P: AsRef<Path>>(
        path: P,
        first_branch: &str,
        printer: &dyn OutputService,
    ) -> Result<Self, anyhow::Error> {
        if !path.as_ref().exists() {
            fs::create_dir_all(&path)?;
        }
        let repo = Repository::new_impl(path, true, printer)?;

        if !repo.worktree.is_dir() {
            return Err(anyhow!(format!(
                "Path {} is not a directory",
                repo.worktree.display()
            )));
        }
        if !repo.git_dir.exists() {
            fs::create_dir(&repo.git_dir).context("could not create .git directory")?;
        } else if !repo.git_dir.is_dir() {
            return Err(anyhow!(format!(
                "Path {} is not a directory",
                repo.git_dir.display()
            )));
        }

        let mut dir_contents = repo
            .git_dir
            .read_dir()
            .context("Could not attempt to read contents of repository")?;
        if dir_contents.next().is_some() {
            return Err(anyhow!("Repository directory is not empty"));
        }

        repo.loose_object_store.create()?;
        fs::create_dir_all(&repo.packfile_base)?;
        repo.ref_log_store.create()?;
        repo.ref_store.create()?;

        write_single_line(repo.file("description")?, "Unnamed repository")?;
        write_single_line(
            repo.file("HEAD")?,
            &format!("ref: refs/heads/{first_branch}"),
        )?;

        repo.config.save()?;

        Ok(repo)
    }

    /// Convert a path relative to the git directory into a canonical path
    fn path<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        self.git_dir.join(path)
    }

    /// Convert an absolute path, or a path relative to the process's current working directory,
    /// and convert it into a path relative to the repository worktree.
    ///
    /// # Errors
    ///
    /// This functions returns [`PathError`] errors with the [`PathError::kind`] property set to the
    /// type of error.
    ///
    /// If the path cannot be canonicalised or is otherwise invalid, it returns [`PathErrorKind::InvalidPath`]
    ///
    /// If the path's tip is not inside this repository, it returns [`PathErrorKind::PathOutsideRepo`]
    pub fn worktree_path<T: AsRef<Path> + ToString>(&self, path: T) -> Result<PathBuf, PathError> {
        let mut abs_path = match std::path::absolute(&path) {
            Ok(p) => p,
            Err(_) => return Err(PathError::new(path, PathErrorKind::InvalidPath)),
        };
        if abs_path.exists() {
            abs_path = match fs::canonicalize(&abs_path) {
                Ok(p) => p,
                Err(_) => return Err(PathError::new(path, PathErrorKind::InvalidPath)),
            };
        }
        match abs_path.strip_prefix(&self.worktree) {
            Ok(p) => Ok(p.to_path_buf()),
            Err(_) => Err(PathError::new(path, PathErrorKind::PathOutsideRepo)),
        }
    }

    /// Canonicalise an absolute path, or a path relative to the process's current working directory,
    /// and confirm that the path's tip is inside the repository.
    ///
    /// Returns an error if the path is not valid or is outside the repository.
    pub fn canon_path<T: AsRef<Path> + ToString>(&self, path: T) -> Result<PathBuf, PathError> {
        let abs_path = match std::path::absolute(&path) {
            Ok(p) => p,
            Err(_) => return Err(PathError::new(path, PathErrorKind::InvalidPath)),
        };
        if !abs_path.starts_with(&self.worktree) {
            return Err(PathError::new(path, PathErrorKind::PathOutsideRepo));
        }
        if abs_path.exists() {
            let abs_path = match fs::canonicalize(abs_path) {
                Ok(p) => p,
                Err(_) => return Err(PathError::new(path, PathErrorKind::InvalidPath)),
            };
            if !abs_path.starts_with(&self.worktree) {
                return Err(PathError::new(path, PathErrorKind::PathOutsideRepo));
            }
            Ok(abs_path)
        } else {
            Ok(abs_path)
        }
    }

    /// Converts a file path relative to the .git directory, to an absolute path.
    fn file<P: AsRef<Path> + std::fmt::Debug>(&self, path: P) -> Result<PathBuf, anyhow::Error> {
        let abs_path = std::path::absolute(self.path(path))?;
        if !abs_path.starts_with(&self.git_dir) {
            return Err(anyhow!("Path is outside repository"));
        }

        if abs_path.exists() {
            let abs_path = fs::canonicalize(abs_path)?;
            if !abs_path.starts_with(&self.git_dir) {
                return Err(anyhow!("Path is outside repository"));
            }
            if abs_path.is_file() {
                Ok(abs_path)
            } else {
                Err(anyhow!("Path must not be a directory"))
            }
        } else {
            let parent = abs_path.parent().unwrap(); // Unwrappable because it must be at least self.git_dir
            if parent.exists() {
                if parent.is_dir() {
                    Ok(abs_path)
                } else {
                    Err(anyhow!("Parent of path must be a directory"))
                }
            } else {
                check_and_create_dir(parent)?;
                Ok(abs_path)
            }
        }
    }

    /// Store a new pack in the repository
    pub fn store_pack<R: Read>(
        &mut self,
        mut reader: R,
        printer: &dyn OutputService,
    ) -> Result<(), anyhow::Error> {
        let new_pack = PackStore::store_pack(&self.packfile_base, &mut reader, printer)?;
        self.packs.push(new_pack);
        Ok(())
    }

    /// Find the canonical ID of an object.
    ///
    /// The name parameter to this method can be any of the following:
    /// - a full object ID
    /// - a partial ID,
    /// - a tag name
    /// - a local branch name
    /// - a remote branch name.   Remote branch names are only searched
    ///   if no local branch name match is found.
    ///
    /// If the `follow_tags` parameter is true and the object is a chunky tag, the method will return
    /// the tag's target.
    ///
    /// If the `kind` parameter is set, this method will only be
    /// successful if one object is found and that object's kind matches the parameter.  If `kind` is
    /// set to [`ObjectKind::Tree`], `follow_tags` is true, and the object found is a commit object,
    /// the method returns the commit's root tree.
    ///
    /// An error is returned if multiple candidate objects were found.  In general, this
    /// can only happen if the `name` parameter is a partial object ID.  The error will
    /// be a [`FindObjectError`] struct with its [`FindObjectError::candidates`] field set to
    /// a vector of the matching object IDs.
    ///
    /// An error is also returned if no matching objects were found, or if any errors were
    /// encountered seaching the object stores, etc.
    pub fn find_object(
        &self,
        name: &str,
        kind: Option<ObjectKind>,
        follow_tags: bool,
    ) -> Result<String, anyhow::Error> {
        let resolve_result = self.resolve_object(name)?;
        if resolve_result.is_empty() {
            return Err(anyhow::Error::from(FindObjectError::none()));
        }
        if resolve_result.len() > 1 {
            return Err(anyhow::Error::from(FindObjectError::some(&resolve_result)));
        }
        let Some(kind) = kind else {
            return Ok(resolve_result[0].to_string());
        };
        let mut current_target = resolve_result[0].to_string();
        loop {
            let obj = self.read_raw_object(&current_target)?;
            let Some(obj) = obj else {
                return Err(anyhow::Error::from(FindObjectError::none()));
            };
            if obj.metadata().kind == kind {
                return Ok(current_target);
            }
            if !follow_tags {
                return Err(anyhow::Error::from(FindObjectError::none()));
            }
            match StoredObject::try_from(&obj)? {
                StoredObject::Tag(tag) => {
                    current_target = tag.target().context("chunky tag has invalid target")?;
                }
                StoredObject::Commit(commit) => {
                    if let ObjectKind::Tree = kind {
                        current_target = commit.tree().context("commit has no tree")?;
                    }
                }
                _ => {
                    return Err(anyhow::Error::from(FindObjectError::none()));
                }
            }
        }
    }

    fn resolve_object(&self, name: &str) -> Result<Vec<String>, anyhow::Error> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(Vec::<String>::new());
        }

        if name == "HEAD" {
            let head_ref = self.current_commit()?;
            return match head_ref {
                Some(hr) => Ok(vec![hr]),
                None => Ok(vec![]),
            };
        }

        let mut collected = HashSet::<String>::new();
        if is_partial_object_id(name) {
            let mut all_objects = HashSet::<String>::new();
            for loose_object in self.loose_object_store.objects_by_id(name)? {
                all_objects.insert(loose_object);
            }
            for pack in &self.packs {
                for packed_object in pack.objects_by_id(name)? {
                    all_objects.insert(packed_object);
                }
            }
            for item in all_objects {
                collected.insert(item);
            }
        }

        let potential_tag = self
            .ref_store
            .resolve_target(&RefSpec::Tag(TagSpec::new(name, false)))?;
        if let Some(RefTarget::Object(potential_tag)) = potential_tag {
            collected.insert(potential_tag);
        }

        let potential_branch = self
            .ref_store
            .resolve_target(&BranchSpec::new(name, BranchLocation::Local).into_ref_spec())?;
        if let Some(RefTarget::Object(potential_branch)) = potential_branch {
            collected.insert(potential_branch);
        } else {
            let potential_remote_branches = self.ref_store.remote_branches_by_name(name)?;
            for remote_branch in potential_remote_branches {
                if let Some(RefTarget::Object(remote_branch_target)) = self
                    .ref_store
                    .resolve_target(&remote_branch.into_ref_spec())?
                {
                    collected.insert(remote_branch_target);
                }
            }
        }

        Ok(collected.into_iter().collect::<Vec<String>>())
    }

    fn find_store_for_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ObjectSource>, anyhow::Error> {
        if self.loose_object_store.has_object(object_id)? {
            return Ok(Some(ObjectSource::LooseObjectStore));
        }
        for i in 0..self.packs.len() {
            if self.packs[i].has_object(object_id)? {
                return Ok(Some(ObjectSource::Pack(i)));
            }
        }
        Ok(None)
    }

    /// Determine if an object is present in the repository.
    ///
    /// The parameter should be the full ID of an object.
    pub fn has_object(&self, object_id: &str) -> Result<bool, anyhow::Error> {
        Ok(self.find_store_for_object(object_id)?.is_some())
    }

    fn read_raw_object_data(
        &self,
        object_id: &str,
    ) -> Result<Option<RawObjectData>, anyhow::Error> {
        let source = self.find_store_for_object(object_id)?;
        let Some(source) = source else {
            return Ok(None);
        };

        let raw_object = match source {
            ObjectSource::LooseObjectStore => {
                self.loose_object_store.read_raw_object_data(object_id)?
            }
            ObjectSource::Pack(i) => self.packs[i].read_raw_object_data(object_id)?,
        };

        Ok(raw_object)
    }

    /// Reads a raw object from the object stores.
    ///
    /// The `object_id` parameter should be a full, valid object ID.  If this object is not present, the method will
    /// return `Ok(None)`.  If the parameter is not a valid object ID, the method may return an error.
    ///
    /// If an object is present in the loose object store, it is loaded from there.  If not,
    /// it is loaded from the first packfile it is found in.  The order packfiles are searched in is not guaranteed,
    /// but it will be consistent across calls to the same object.
    ///
    /// This method will return an error if it encounters any errors reading from the object stores.
    pub fn read_raw_object(&self, object_id: &str) -> Result<Option<RawObject>, anyhow::Error> {
        let raw_object_data = self.read_raw_object_data(object_id)?;
        let Some(raw_object_data) = raw_object_data else {
            return Ok(None);
        };
        let raw_object_data = match &raw_object_data.metadata().kind {
            ObjectKind::Delta(base) => {
                let base_object = self.read_raw_object(base)?;
                let Some(base_object) = base_object else {
                    return Err(anyhow!("named delta object base not found in repository"));
                };
                raw_object_data.combine(&base_object)
            }
            _ => raw_object_data,
        };

        Ok(Some(RawObject::try_from(raw_object_data)?))
    }

    /// Reads an object from the object stores.
    ///
    /// The `object_id` parameter should be a full, valid object ID.  If this object is not present, the method will
    /// return `Ok(None)`.  If the parameter is not a valid object ID, the method may return an error.
    ///
    /// See [`Repository::read_raw_object`] for details of how the object store is selected, and errors that may
    /// be returned.
    pub fn read_object(&self, object_id: &str) -> Result<Option<StoredObject>, anyhow::Error> {
        let raw_object = self.read_raw_object(object_id)?;
        let Some(raw_object) = raw_object else {
            return Ok(None);
        };
        Ok(Some(StoredObject::try_from(&raw_object)?))
    }

    /// Write a [`RawObject`] to the loose object store.
    ///
    /// This method writes the content of a [`RawObject`] to the loose object store, if it does not already
    /// exist, and returns the object's ID.
    ///
    /// If an object with the same ID already exists in the loose object store, this method does not overwrite
    /// it, assuming that collisions are rare enough that we can assume the files have the same content.
    ///
    /// This method returns an error if it encounters errors on writing to the filesystem.
    pub fn write_raw_object(&self, obj: &RawObject) -> Result<String, anyhow::Error> {
        self.loose_object_store.write_raw_object(obj)
    }

    /// Write an object to the loose object store.
    ///
    /// This method serialises an object and writes it content to the loose object store, if it does not
    /// already exist, and returns the object's ID.
    ///
    /// See [`Repository::write_raw_object`] for further details.
    pub fn write_object(&self, obj: &impl GitObject) -> Result<String, anyhow::Error> {
        self.loose_object_store.write_object(obj)
    }

    /// Returns a map of references in the repository and the objects they point to, unpeeled
    /// in the case of tags.
    ///
    /// Returns an error if any errors are encountered accessing the filesystem.
    pub fn ref_list(&self) -> Result<IndexMap<String, RefTarget>, anyhow::Error> {
        let mut refs = self
            .ref_store
            .ref_targets()?
            .map(|x| (x.spec.to_string(), x.target))
            .collect::<Vec<(String, RefTarget)>>();
        refs.sort_by(|a, b| a.0.cmp(&b.0));
        let mut result = IndexMap::<String, RefTarget>::new();
        for item in refs {
            result.insert(item.0, item.1);
        }
        Ok(result)
    }

    /// Returns a map of tags in the repository and the objects they point to, unpeeled.
    ///
    /// Returns an error if any errors are encountered accessing the filesystem.
    pub fn tag_list(&self) -> Result<IndexMap<String, RefTarget>, anyhow::Error> {
        let refs = self.ref_store.ref_targets()?;
        let mut result = IndexMap::<String, RefTarget>::new();
        for item in refs
            .filter(|x| matches!(x.spec, RefSpec::Tag(_)))
            .map(|x| (x.spec.to_string(), x.target))
        {
            result.insert(item.0, item.1);
        }
        Ok(result)
    }

    /// Resolve a reference to its target.
    pub fn resolve_ref(&self, ref_spec: &RefSpec) -> Result<Option<RefTarget>, anyhow::Error> {
        self.ref_store.resolve_target(ref_spec)
    }

    /// Creates a thin reference to an object.
    ///
    /// The `target_id` parameter should be a valid object ID, but is not validated.
    ///
    /// # Errors
    ///
    /// An error is returned if there are any issues writing to the repository.
    pub fn create_ref(&self, name: &str, target_id: &str) -> Result<(), anyhow::Error> {
        self.ref_store
            .create_update_ref(&RefSpec::from_str(name)?, &RefTarget::from_str(target_id)?)
    }

    /// Loads the repository index file, named `.git/index`.
    ///
    /// # Errors
    ///
    /// An error is returned if there are any issues reading from the filesystem,
    /// or if the index file is corrupt or malformed.
    pub fn read_index(&self) -> Result<Index, anyhow::Error> {
        let file = self.file(Path::new("index"))?;
        if !file.exists() {
            return Ok(Index::new());
        }
        let data = std::fs::read(file).context("error loading index file")?;
        let index = Index::from_bytes(&data).context("malformed index file")?;
        Ok(index)
    }

    /// Writes an in-memory index to the repository index file `.git/index`.
    ///
    /// This function operates by writing to a file named `.git/index.lck`, and renaming `index.lck`
    /// to `index` when the write is complete.  If this function fails, it may leave a partially-written
    /// `index.lck` file, which may cause issues with Git interoperability or other tools which will
    /// check for the presence of this file.
    ///
    /// # Errors
    ///
    /// An error is returned if there any issues encountered accessing the filesystem.
    pub fn write_index(&self, index: &Index) -> Result<(), anyhow::Error> {
        let tmp_file = self.path("index.lck");
        let final_file = self.path("index");
        let mut data = Vec::<u8>::new();
        index.serialise(&mut data);
        fs::write(&tmp_file, &data).context("error writing temporary index")?;
        fs::rename(&tmp_file, &final_file).context("failed to rename temporary index file")?;
        Ok(())
    }

    /// Load the repository's ignore rulesets.
    ///
    /// Ignore ruleset files will be loaded from the following locations:
    /// - in the repository, the file `.git/info/exclude`, if it exists
    /// - if the user's `XDG_CONFIG_HOME` environment variable is set, from
    ///   the file `$XDG_CONFIG_HOME/git/ignore`
    /// - if the user's `XDG_CONFIG_HOME` environment variable is not set,
    ///   from the file `.config/git` in the user's home directory.
    /// - any `.gitignore` files in the worktree, *as long as they have already
    ///   been stored in the repository and written to the index*.  They do not have
    ///   to have been committed.  The last-added version of each `.gitignore` file is
    ///   the one which will be used.
    ///
    /// `.gitignore` files, if any, are loaded as "scoped files".  In other words, their rules
    /// only apply to their parent directory and any subdirectories underneath it.  A `.gitignore`
    /// file in a subdirectory can override one at a higher level, as long as its parent directory
    /// is not itself ignored.
    ///
    /// # Errors
    ///
    /// An error will be returned if the method encounters any errors reading from the filesystem
    /// or the object stores.
    pub fn read_ignore_info(&self) -> Result<IgnoreInfo, anyhow::Error> {
        let mut absolute_exclude_files = Vec::<PathBuf>::new();
        let repo_wide_file: PathBuf = self.git_dir.join("info").join("exclude");
        if repo_wide_file.exists() && repo_wide_file.is_file() {
            absolute_exclude_files.push(repo_wide_file);
        }

        let config_dir_var = env::var("XDG_CONFIG_HOME");
        let config_dir = match config_dir_var {
            Ok(var) => Some(PathBuf::from_str(&var).unwrap().join("git")),
            Err(_) => env::home_dir().map(|hd| hd.join(".config").join("git")),
        };
        if let Some(config_dir) = config_dir {
            let global_exclude_file = config_dir.join("ignore");
            if global_exclude_file.exists() && global_exclude_file.is_file() {
                absolute_exclude_files.push(global_exclude_file);
            }
        }

        let mut scoped_files = HashMap::<String, Blob>::new();
        let index = self.read_index()?;
        for entry in index
            .entries()
            .iter()
            .filter(|e| e.object_name == ".gitignore" || e.object_name.ends_with("/.gitignore"))
        {
            let slash_idx = entry.object_name.rfind("/");
            let entry_dir = match slash_idx {
                Some(idx) => entry.object_name[..idx].to_string(),
                None => String::new(),
            };
            let contents = self.read_object(&entry.object_id)?;
            let Some(contents) = contents else {
                return Err(anyhow!(
                    "ignore file {} ({}) listed in index is not present in object store",
                    entry.object_name,
                    entry.object_id
                ));
            };
            let StoredObject::Blob(blob) = contents else {
                return Err(anyhow!(
                    "ignore file {} ({}) listed in index is not a blob",
                    entry.object_name,
                    entry.object_id
                ));
            };
            scoped_files.insert(entry_dir, blob);
        }

        IgnoreInfo::from_files(absolute_exclude_files, scoped_files)
    }

    /// Returns the name of the current branch, or `Ok(None)` if there is no current branch
    /// (the so-called "detached HEAD" state).
    ///
    /// This method will return the current branch name set in `HEAD` regardless of whether or
    /// not that branch exists.  This situation is normally only found for a new repository
    /// with no commits.
    ///
    /// # Errors
    ///
    /// Returns an error if errors are encountered reading from the filesystem, or if the file
    /// `.git/HEAD` is missing.
    pub fn current_branch(&self) -> Result<Option<BranchSpec>, anyhow::Error> {
        let head = self.file(Path::new("HEAD")).context("error finding HEAD")?;
        if !head.exists() {
            return Err(anyhow!("missing HEAD"));
        }
        let head_conts = std::fs::read_to_string(head).context("failed to read HEAD")?;
        if let Some(head_target) = head_conts.strip_prefix("ref: ") {
            Ok(Some(BranchSpec::from_str(head_target.trim())?))
        } else {
            Ok(None)
        }
    }

    /// Returns the details of the remote-tracking branch that the current branch (if any) is mapped to.
    /// Returns `Ok(None)` if there is no current branch, or if the current branch is not mapped to a remote.
    ///
    /// # Errors
    ///
    /// Returns an error if errors are encountered reading from the filesystem, or if the file `.git/HEAD` is missing.
    pub fn current_remote_tracking_branch(&self) -> Result<Option<BranchSpec>, anyhow::Error> {
        let current_local_branch = self.current_branch()?;
        Ok(current_local_branch
            .map(|b| self.config.branch_config(&b).and_then(|c| c.remote()))
            .flatten())
    }

    /// Returns the ID of the current commit, or `Ok(None)` if there is no current commit.
    ///
    /// "Current commit" means the commit pointed to by `.git/HEAD`, whether directly or as the tip
    /// of the current branch.  This method will return `Ok(None)` if the current branch does not exist.
    /// If the current branch does exist, or if `HEAD` is detached, it does not verify that the
    /// commit exists in the repository.
    ///
    /// # Errors
    ///
    /// Returns an error if errors are encountered reading from the filesystem, or if the file
    /// `.git/HEAD` is missing.
    pub fn current_commit(&self) -> Result<Option<String>, anyhow::Error> {
        let path = self.file(PathBuf::from("HEAD"))?;
        if !path.exists() {
            return Ok(None);
        }
        let head_conts = fs::read_to_string(path)?;
        let head_target = RefTarget::from_str(&head_conts)?;
        match head_target {
            RefTarget::SymbolicRef(r) => {
                Ok(self.ref_store.resolve_target(&r)?.map(|t| t.to_string()))
            }
            RefTarget::Object(id) => Ok(Some(id)),
        }
    }

    /// Lists all branches present in the repository.
    ///
    /// Always lists the current branch pointed to by `HEAD` in the results (if there is one) even if
    /// that branch does not exist (in Git terminology, if it is an unborn branch).  Creating a commit
    /// on that branch will force it to be created.
    ///
    /// # Errors
    ///
    /// Returns an error if errors are encountered reading from the filesystem or if the file `.git/HEAD`
    /// is missing.
    pub fn branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error> {
        let mut branches: Vec<BranchSpec> = self.ref_store.branches()?.collect();
        if let Some(cb) = self.current_branch()? {
            if !branches.contains(&cb) {
                branches.push(cb);
            }
        }
        branches.sort();
        Ok(branches)
    }

    /// Determine if a given string is a valid local branch name.
    ///
    /// This method will only return `Ok(true)` if the branch is present in store.  It is therefore possible
    /// for this method to return `Ok(false)` for a branch name that is included in the output of
    /// [`Repository::branches`], if `HEAD` points to a currently non-existent branch.
    pub fn is_branch_name(&self, query_name: &str) -> Result<bool, anyhow::Error> {
        self.ref_store
            .is_existing_ref(&BranchSpec::new(query_name, BranchLocation::Local).into_ref_spec())
    }

    /// Determine if a given string is a valid remote branch name.
    pub fn is_remote_branch_name(&self, query_name: &str) -> Result<bool, anyhow::Error> {
        let mut search_results = self.ref_store.remote_branches_by_name(query_name)?;
        Ok(search_results.next().is_some())
    }

    /// Update the commit that a branch points to, creating it if it does not exist.
    ///
    /// The caller is responsible for verifying the commit ID is valid, updating the branch's ref log,
    /// and potentially updating the `HEAD` ref log if this branch is the current branch.
    ///
    /// The branch is specified by name, as it is assumed to be local.
    ///
    /// If the branch does not exist, it will be created.
    pub fn update_local_branch(
        &self,
        branch_name: &str,
        commit_id: &str,
    ) -> Result<(), anyhow::Error> {
        self.ref_store.create_update_ref(
            &BranchSpec::new(branch_name, BranchLocation::Local).into_ref_spec(),
            &RefTarget::from_str(commit_id)?,
        )
    }

    /// Update a ref, creating it if it does not exist.
    pub fn update_ref(&self, refspec: &RefSpec, target: &RefTarget) -> Result<(), anyhow::Error> {
        self.ref_store.create_update_ref(refspec, target)
    }

    /// Delete a ref, if it exists.
    pub fn delete_ref(&mut self, refspec: &RefSpec) -> Result<(), anyhow::Error> {
        self.ref_store.delete_ref(refspec)
    }

    /// Delete the configuration for a branch
    pub fn delete_branch_config(&mut self, branch_spec: &BranchSpec) -> Result<(), anyhow::Error> {
        self.config.branch_config_delete(branch_spec)
    }

    /// Update the branch that `HEAD` points to.
    ///
    /// The parameter is assumed to be a branch name, but is not checked; the branch does
    /// not have to exist, as committing to it will create it.  Passing an object ID will
    /// result in `HEAD` pointing to a branch whose name looks like an object ID, which is probably not
    /// what you want.  To point `HEAD` to an object directly, use [`Repository::update_head_detached`].
    ///
    /// The caller is responsible for updating the `HEAD` ref log.
    ///
    /// # Errors
    ///
    /// An error is returned if any errors are encountered writing to the filesystem.
    pub fn update_head(&self, branch_name: &str) -> Result<(), anyhow::Error> {
        self.update_head_detached(&format!("ref: refs/heads/{branch_name}"))
    }

    /// Update `HEAD`, pointing it directly to a commit (so-called "detached HEAD mode")
    ///
    /// The parameter is assumed to be a full commit ID.
    ///
    /// The caller is responsible for confirming the validity of the `commit_id` parameter,
    /// and for updating the `HEAD` ref log.
    ///
    /// # Errors
    ///
    /// An error is returned if any errors are encountered writing to the filesystem.
    pub fn update_head_detached(&self, commit_id: &str) -> Result<(), anyhow::Error> {
        write_single_line(self.git_dir.join("HEAD"), commit_id)
    }

    /// Generates a map of every file referred to in the current commit.
    ///
    /// This method takes the current commit pointed to by `HEAD`, gets its tree, and returns
    /// a map of item path to object ID for every blob referred to by the current tree and its
    /// subtrees.  The item paths are in Git index form, using ASCII `/` as the path separator.
    ///
    /// # Errors
    ///
    /// An error is returned if there are any issues reading from the object stores, or if the
    /// current commit's tree is corrupt---for example, if a subtree is not present in the
    /// repository, or a subtree entry points to a non-tree object.
    pub fn flatten_head_tree(&self) -> Result<HashMap<String, String>, anyhow::Error> {
        self.flatten_tree_recursive("HEAD", "")
    }

    fn flatten_tree_recursive(
        &self,
        tree_id: &str,
        prefix: &str,
    ) -> Result<HashMap<String, String>, anyhow::Error> {
        let mut map = HashMap::<String, String>::new();
        let tree_id = self
            .find_object(tree_id, Some(ObjectKind::Tree), true)
            .context("could not find tree")?;
        let tree = self.read_object(&tree_id).context("error reading tree")?;
        let Some(tree) = tree else {
            return Err(anyhow!("tree has suddenly disappeared"));
        };
        let StoredObject::Tree(tree) = tree else {
            return Err(anyhow!("tree is not actually a tree"));
        };
        for entry in tree.entries() {
            let full_path = match prefix {
                "" => entry.name().to_string(),
                _ => format!("{prefix}/{}", entry.name()),
            };
            if entry.mode < 0o100000 {
                // Directory
                let subresult = self.flatten_tree_recursive(&entry.object_id, &full_path)?;
                map.extend(subresult);
            } else {
                map.insert(full_path, entry.object_id.clone());
            }
        }
        Ok(map)
    }

    /// Removes a path from the given index, optionally deleting the indexed file from the filesystem.
    ///
    /// The `hard_delete` parameter indicates whether to delete the indexed file (`true`) or not (`false`).
    ///
    /// Returns `Ok(false)` if the path did not exist in the index, and `Ok(true)` if it has been removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is not valid, the path is outside the worktree, or an error was
    /// encountered attempting to delete the path from the filesystem.
    ///
    /// This method is not atomic.  If an error occurs because the path could not be deleted from the filesystem,
    /// it will already have been removed from the index.
    pub fn remove_path_from_index(
        &self,
        path: &str,
        index: &mut Index,
        hard_delete: bool,
    ) -> Result<bool, anyhow::Error> {
        let worktree_path = self.worktree_path(path)?;
        let index_path = path_translate(&worktree_path);
        if !index.contains_path(&index_path) {
            return Ok(false);
        }
        index.remove(&index_path);
        if hard_delete {
            let abs_path = self.canon_path(path).context("invalid path to remove")?;
            fs::remove_file(&abs_path)
                .context(format!("could not delete file {}", abs_path.display()))?;
        }
        Ok(true)
    }

    /// Adds the contents of a sequence of files to the repository as blobs, and to the index.
    ///
    /// The index is loaded from disk and saved back to it.
    ///
    /// If a path already existed in the index, its entry will be replaced.
    ///
    /// This method only adds paths that are inside the worktree but outside the `.git` directory.
    /// Paths inside the `.git` directory will be silently ignored.
    ///
    /// # Errors
    ///
    /// An error will be returned if any filesystem errors are encountered reading blob contents,
    /// saving blobs to the loose object store, or for any other reason.
    ///
    /// This method is not atomic.  However, it will only attempt to write the index to disk if
    /// all items have successfully been written to the repository as blobs.  If this method returns
    /// an error, it may leave orphaned blobs written to the repository.
    pub fn add_paths_to_index_and_write<T: AsRef<Path>>(
        &self,
        paths: &[T],
    ) -> Result<(), anyhow::Error> {
        let mut index = self.read_index()?;
        let mut new_entries: Vec<IndexEntry> = vec![];
        for path in paths {
            let new_entry = self.add_path_partial(path, &mut index)?;
            if let Some(new_entry) = new_entry {
                new_entries.push(new_entry);
            }
        }
        index.add_range(&mut new_entries);
        self.write_index(&index)?;
        Ok(())
    }

    /// Adds a path to the repository.  Removes any existing entry from the index, and returns a new index entry.  
    fn add_path_partial<T: AsRef<Path>>(
        &self,
        path: T,
        index: &mut Index,
    ) -> Result<Option<IndexEntry>, anyhow::Error> {
        let absolute_path = fs::canonicalize(path).context("could not make path valid")?;
        if !absolute_path.starts_with(&self.worktree) {
            return Err(anyhow!("path is outside the worktree"));
        }
        // Trying to add something inside the repo to the repo appears to be an error-free no-op in git
        if absolute_path.starts_with(&self.git_dir) {
            return Ok(None);
        }
        let relative_path = absolute_path.strip_prefix(&self.worktree)?;
        let index_path = path_translate(relative_path);
        let hash = self.write_object(&Blob::new_from_path(&absolute_path)?)?;
        index.remove(&index_path);
        Ok(Some(IndexEntry::from_file(
            &absolute_path,
            hash,
            index_path,
        )?))
    }

    /// Check the index for missing objects.
    ///
    /// If there are any objects listed in the index that are not present in the repository, this method returns `Ok(Some(object_id))`
    /// where `object_id` is the first such object found.
    ///
    /// If all of the objects listed in the index are present in the repository, it returns `Ok(None)`.
    ///
    /// # Errors
    ///
    /// An error is returned if any errors are encountered reading from the filesystem or the object stores.
    pub fn check_index(&self, index: &Index) -> Result<Option<String>, anyhow::Error> {
        for entry in index.entries() {
            if self.resolve_object(&entry.object_id)?.len() != 1 {
                return Ok(Some(entry.object_id.to_string()));
            }
        }
        Ok(None)
    }

    /// Store the index as a set of tree objects, returning the ID of the root object.
    ///
    /// The index contents are not validated; it is assumed that all of the objects
    /// listed in the index are extant repository objects, and that the state of the index
    /// accurately reflects the state of the worktree.
    ///
    /// # Errors
    ///
    /// An error is returned if there are any errors encountered writing to the loose object store.
    pub fn store_index(&self, index: &Index) -> Result<String, anyhow::Error> {
        let mut dir_contents = HashMap::<String, Vec<&IndexEntry>>::new();
        for entry in index.entries() {
            let entry_dir_name = entry.object_directory_name();
            add_to_map_of_vecs(&mut dir_contents, entry_dir_name, entry);
            add_parent_dirs_to_map_of_vecs(&mut dir_contents, index_path_parent(entry_dir_name));
        }
        let mut dirs = dir_contents.keys().collect::<Vec<&String>>();
        // reverse sort by length
        dirs.sort_by_key(|a| std::cmp::Reverse(a.len()));
        let mut trees = HashMap::<String, Vec<TreeNode>>::new();
        let mut final_tree = String::new();
        for dir in dirs {
            let dir_name = index_path_file(dir);
            let parent_dir = index_path_parent(dir);
            let subdirs = if trees.contains_key(dir) {
                &trees[dir]
            } else {
                &Vec::new()
            };
            let dir_id = self.store_partial_index(&dir_contents[dir], subdirs)?;
            if dir.is_empty() {
                final_tree = dir_id;
            } else {
                let dir_node = TreeNode::from_subtree(dir_name, &dir_id);
                add_to_map_of_vecs(&mut trees, parent_dir, dir_node);
            }
        }
        Ok(final_tree)
    }

    fn store_partial_index(
        &self,
        entries: &[&IndexEntry],
        subtrees: &[TreeNode],
    ) -> Result<String, anyhow::Error> {
        let mut tree = Tree::new();
        let mut nodes = entries
            .iter()
            .map(|ixe| TreeNode::from_index_entry(ixe))
            .collect::<Vec<TreeNode>>();
        nodes.append(&mut subtrees.to_vec());
        tree.add_entries(&mut nodes);
        self.write_object(&tree)
    }

    /// Write a ref log entry.
    ///
    /// As per for [`RefLog::write`], this method will always write to the `HEAD` ref log,
    /// and will also write to a branch ref log if the `branch_name` parameter is set.
    ///
    /// If the specified ref log does not exist, it will be created.
    ///
    /// # Errors
    ///
    /// An error will be returned if any errors are encountered writing to the filesystem.
    pub fn write_ref_log(
        &self,
        old_object_id: Option<&str>,
        new_object_id: &str,
        committer_name: &str,
        message: &str,
        branch: &RefSpec,
        also_update_head: bool,
    ) -> Result<(), anyhow::Error> {
        let entry = RefLogEntry::new(
            old_object_id,
            new_object_id,
            &timestamped_name(committer_name, &Local::now()),
            message,
        );
        if also_update_head {
            self.ref_log_store.write(&entry, &RefSpec::Head)?;
        }
        self.ref_log_store.write(&entry, branch)
    }

    /// Output the contents of a ref log to `stdout`.
    ///
    /// The ref given does not need to exist, as long as its ref log file exists.
    ///
    /// # Errors
    ///
    /// This method will return an error if it encounters any errors reading from
    /// the filesystem, or if the branch given does not have a ref log file.
    pub fn show_ref_log(&self, branch: &RefSpec) -> Result<(), anyhow::Error> {
        self.ref_log_store.dump(branch)
    }

    /// Determine whether a ref log exists for a given branch.
    ///
    /// This method is infallible, and returns `Ok(false)` if it encounters filesystem errors.
    pub fn check_ref_log_exists(&self, branch: &RefSpec) -> Result<bool, anyhow::Error> {
        Ok(self.ref_log_store.check_exists(branch))
    }

    /// Delete a ref log, if it exists.
    pub fn delete_ref_log(&self, branch: &RefSpec) -> Result<(), anyhow::Error> {
        self.ref_log_store.delete(branch)
    }

    /// List the ref logs present in the repository
    ///
    /// This method returns an error if it encounters any errors reading from the filesystem.
    pub fn list_ref_logs(&self) -> Result<Vec<String>, anyhow::Error> {
        self.ref_log_store.list_ref_logs()
    }

    /// List the names of remotes from the repository's config.
    pub fn list_remote_names(&self) -> Vec<String> {
        self.config
            .remote_names()
            .iter()
            .map(|r| r.to_string())
            .collect()
    }

    /// Get details of a remote, or `None` if the remote does not exist.
    pub fn get_remote<'a>(&'a self, name: &'a str) -> Option<RemoteInfo> {
        self.config.remote_info(name)
    }

    /// Get an iterator that returns either all of the reachable commit IDs in the repository,
    /// or, if the `start` parameter is `Some(commit)`, that commit and all of its ancestors.
    ///
    /// # Errors
    ///
    /// This method returns an error if it encounters any errors reading the repository's
    /// refs.
    pub fn commits<'a>(&'a self, start: Option<&str>) -> Result<Commits<'a>, anyhow::Error> {
        Commits::new(self, start)
    }

    /// Determines whether or not `descendant` is genuinely a descendant of `ancester`.
    pub fn commit_is_ancestor(
        &self,
        descendant: &str,
        ancestor: &str,
    ) -> Result<bool, anyhow::Error> {
        Ok(self
            .commits(Some(descendant))?
            .filter_map(|x| x.ok())
            .any(|x| x == ancestor))
    }

    /// Determines whether or not `descendant` is a pure descendant of `ancestor`.  In other words,
    /// `ancestor` is an ancestor of `descendant`, and all of `decendant`'s other ancestors are either
    /// ancestors of or descendants of `ancestor`.
    pub fn commit_is_pure_ancestor(
        &self,
        descendant: &str,
        ancestor: &str,
    ) -> Result<bool, anyhow::Error> {
        if !self.commit_is_ancestor(descendant, ancestor)? {
            return Ok(false);
        }
        let ancestor_ancestors: Vec<String> = self
            .commits(Some(ancestor))?
            .filter_map(|id| id.ok())
            .collect();
        let ancestor_ancestors_ref: Vec<&str> =
            ancestor_ancestors.iter().map(String::as_ref).collect();
        let descendant_ancestors: HashSet<String> =
            Commits::new_prunable(self, Some(descendant), &ancestor_ancestors_ref)?
                .filter_map(|id| id.ok())
                .collect();
        for da in descendant_ancestors {
            if !self.commit_is_ancestor(&da, ancestor)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// An iterator which iterates over the valid commit IDs in the repository.  Each commit ID
/// is guaranteed to only be returned once.
///
/// This iterator's associated type is `Result<String, Error>`, because at each iteration
/// it reads the returned commit to determine its parents and potentially queue them for
/// later output.  If that read fails, it will error.
pub struct Commits<'a> {
    repo: &'a Repository,
    queue: VecDeque<String>,
    seen: HashSet<String>,
    prune: HashSet<String>,
}

impl<'a> Commits<'a> {
    fn new(repo: &'a Repository, start: Option<&str>) -> Result<Commits<'a>, anyhow::Error> {
        Self::new_prunable(repo, start, &[])
    }

    fn new_prunable(
        repo: &'a Repository,
        start: Option<&str>,
        prune_commits: &[&str],
    ) -> Result<Commits<'a>, anyhow::Error> {
        let prune: HashSet<String> = prune_commits.iter().map(|id| id.to_string()).collect();
        let seen = match start {
            Some(sid) => {
                if prune.contains(sid) {
                    HashSet::new()
                } else if let Some(cid) = peel_to_commit(sid, repo) {
                    let mut hs = HashSet::new();
                    hs.insert(cid);
                    hs
                } else {
                    HashSet::new()
                }
            }
            None => repo
                .ref_store
                .ref_targets()?
                .filter_map(|r| target_to_commit_id(r, repo))
                .filter_map(|id| peel_to_commit(&id, repo))
                .filter(|id| !prune.contains(id))
                .collect::<HashSet<String>>(),
        };

        let queue = Self::generate_queue(repo, &seen);
        Ok(Commits {
            repo,
            queue,
            seen,
            prune,
        })
    }

    /// Get the number of commits currently queued in the iterator.
    ///
    /// If this is called on a newly-created iterator which was created using [`Repository::commits()`] with
    /// a `None` parameter, it is effectively the number of commits in the repository pointed to by
    /// the current set of branch tips and tags.
    ///
    /// Calling it at other times is probably not very meaningful.
    pub fn queue_length(&self) -> usize {
        self.queue.len()
    }

    fn generate_queue(repo: &Repository, set: &HashSet<String>) -> VecDeque<String> {
        let mut ref_commits = set.iter().map(|x| x.as_str()).collect::<Vec<&str>>();
        ref_commits.sort_by_key(|id| {
            let commit = repo.read_object(id);
            let Ok(Some(StoredObject::Commit(commit))) = commit else {
                return Utc::now().into();
            };
            if let Some(timestamp) = commit.timestamp() {
                timestamp
            } else {
                Utc::now().into()
            }
        });
        ref_commits.reverse();
        let mut queue = VecDeque::<String>::new();
        for commit in ref_commits {
            queue.push_back(commit.to_string());
        }
        queue
    }
}

impl<'a> Iterator for Commits<'a> {
    type Item = Result<String, anyhow::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let next_val = self.queue.pop_front()?;
        let commit = self.repo.read_object(&next_val);
        let Ok(commit) = commit else {
            return Some(Err(anyhow!("error loading commit")));
        };
        let Some(StoredObject::Commit(commit)) = commit else {
            return Some(Err(anyhow!("error loading commit, or ID is not a commit")));
        };
        for parent in commit.parents() {
            if !self.seen.contains(&parent) && !self.prune.contains(&parent) {
                self.queue.push_back(parent.clone());
                self.seen.insert(parent);
            }
        }
        Some(Ok(next_val))
    }
}

/// Determines whether a string is potentially a valid object ID or partial object ID.
///
/// A valid object ID string consists of ASCII hex digits, and a full object ID will be 40
/// chars long (or 64 chars for a SHA-256 ID).  This function returns true if a string is
/// in the range 4 character to 40 characters inclusive (CVVC doesn't support SHA-256 yet!)
/// and consists solely of lower-case ASCII hex digits.
pub fn is_partial_object_id(id: &str) -> bool {
    // SHA-1 IDs are 20 bytes, represented as 40 hex chars; we don't try to identify an ID that's less than 4 chars
    id.len() >= 4
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase()))
}

enum ObjectSource {
    LooseObjectStore,
    Pack(usize),
}

fn target_to_commit_id(targeted_ref: TargetedRef, repo: &Repository) -> Option<String> {
    match targeted_ref.target {
        RefTarget::SymbolicRef(_) => None,
        RefTarget::Object(obj) => {
            if !repo.has_object(&obj).unwrap_or(false) {
                None
            } else if let Ok(Some(raw_object)) = repo.read_raw_object(&obj) {
                if raw_object.metadata().kind == ObjectKind::Commit {
                    Some(obj)
                } else {
                    None
                }
            } else {
                None
            }
        }
    }
}

fn peel_to_commit(commit_or_tag: &str, repo: &Repository) -> Option<String> {
    let raw_obj = repo.read_raw_object(commit_or_tag);
    if let Ok(Some(raw_obj)) = raw_obj {
        match raw_obj.metadata().kind {
            ObjectKind::Commit => Some(commit_or_tag.to_string()),
            ObjectKind::Tag => {
                if let Ok(StoredObject::Tag(tag)) = StoredObject::try_from(&raw_obj) {
                    tag.target().ok()
                } else {
                    None
                }
            }
            _ => None,
        }
    } else {
        None
    }
}
