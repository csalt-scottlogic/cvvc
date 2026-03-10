use anyhow::{anyhow, Context};
use chrono::{DateTime, TimeZone};
use indexmap::IndexMap;
use ini::Ini;
use std::{
    collections::{HashMap, HashSet},
    env,
    fmt::Display,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::shared::{
    config::default_repo_config,
    errors::FindObjectError,
    helpers::{
        add_parent_dirs_to_map_of_vecs, add_to_map_of_vecs,
        fs::{
            check_and_create_dir,
            errors::{PathError, PathErrorKind},
            index_path_file, index_path_parent, path_translate, write_single_line,
        },
        timestamped_name,
    },
    ignore::IgnoreInfo,
    index::{Index, IndexEntry},
    objects::{
        Blob, Commit, GitObject, ObjectKind, RawObject, StoredObject, Tag, Tree, TreeNode, stored_object_matches_kind
    },
    ref_log::{RefLog, RefLogEntry},
    stores::{BranchKind, BranchSpec, BranchStore, ObjectStore, branch_file_store::BranchFileStore, file_store::LooseObjectStore, pack_store::PackStore},
};

pub struct Repository {
    pub worktree: PathBuf,
    pub git_dir: PathBuf,
    loose_object_store: LooseObjectStore,
    packfile_base: PathBuf,
    packs: Vec<PackStore>,
    branch_store: BranchFileStore,
    ref_log_store: RefLog,
    config: Ini,
}

impl Repository {
    pub fn find<P: AsRef<Path>>(path: P) -> Result<Option<Self>, anyhow::Error> {
        let path_buf = path.as_ref().canonicalize()?;
        if path_buf.join(Path::new(".git")).is_dir() {
            return Ok(Some(Self::new(path_buf, false)?));
        }
        match path_buf.parent() {
            Some(p) => Self::find(p),
            None => Ok(None),
        }
    }

    pub fn find_cwd() -> Result<Option<Self>, anyhow::Error> {
        Self::find(env::current_dir()?)
    }

    pub fn new<P: AsRef<Path>>(worktree: P, allow_invalid: bool) -> Result<Self, anyhow::Error> {
        let worktree = worktree.as_ref().canonicalize()?;
        let git_dir = worktree.join(Path::new(".git"));
        if !(allow_invalid || git_dir.is_dir()) {
            return Err(anyhow!("Not a git directory"));
        }
        let config_path = git_dir.join("config");
        let mut wrapped_config: Option<Ini> = None;
        if config_path.is_file() {
            let loaded_config = Ini::load_from_file(config_path);
            if let Err(lce) = loaded_config {
                if !allow_invalid {
                    return Err(
                        anyhow::Error::from(lce).context("Could not open configuration file")
                    );
                }
            } else {
                wrapped_config = Some(loaded_config.unwrap());
            }
        } else if !allow_invalid {
            return Err(anyhow!("Configuration file missing"));
        }

        let config = wrapped_config.unwrap_or_else(default_repo_config);

        if !allow_invalid {
            let core_section = match config.section(Some("core")) {
                Some(s) => s,
                None => {
                    return Err(anyhow!(
                        "Configuration file does not contain a [core] section"
                    ))
                }
            };
            let format_version_property = match core_section.get("repositoryformatversion") {
                Some(s) => s,
                None => {
                    return Err(anyhow!(
                        "Configuration file does not have the repository format version set"
                    ))
                }
            };
            let format_version = format_version_property
                .parse::<i32>()
                .context("repositoryformatversion is not an integer")?;
            if format_version != 0 {
                return Err(anyhow!("Unsupported repository version {format_version}"));
            }
        }

        let loose_store_path = git_dir.join("objects");
        let loose_object_store = LooseObjectStore::new(&loose_store_path)?;
        let branch_store = BranchFileStore::new(&git_dir);
        let ref_log_store = RefLog::new(git_dir.join("logs"));

        let pack_dir = git_dir.join("objects").join("pack");
        let packs = if pack_dir.is_dir() {
            PackStore::find_packs(&pack_dir)?
        } else {
            vec![]
        };

        Ok(Repository {
            worktree,
            git_dir,
            loose_object_store,
            branch_store,
            ref_log_store,
            packfile_base: pack_dir.clone(),
            packs,
            config,
        })
    }

    pub fn create(path: &PathBuf) -> Result<Self, anyhow::Error> {
        let repo = Repository::new(path, true)?;

        if repo.worktree.exists() {
            if !repo.worktree.is_dir() {
                return Err(anyhow!(format!(
                    "Path {} is not a directory",
                    repo.worktree.display()
                )));
            }
            if !repo.git_dir.exists() {
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
        } else {
            fs::create_dir_all(&repo.worktree)
                .context("Could not create all components of directory path")?;
        }

        repo.dir(Path::new("branches"), true)?;
        repo.loose_object_store.create()?;
        fs::create_dir_all(&repo.packfile_base)?;
        repo.ref_log_store.create()?;
        repo.dir(&["refs", "tags"].iter().collect::<PathBuf>(), true)?;
        repo.branch_store.create()?;

        fs::write(
            repo.file_unchecked(Path::new("description")),
            "Unnamed repository\n",
        )?;

        fs::write(
            repo.file_unchecked(Path::new("HEAD")),
            "ref: refs/heads/main\n",
        )?;

        repo.config
            .write_to_file(repo.file_unchecked(Path::new("config")))?;

        Ok(repo)
    }

    pub fn path(&self, path: &Path) -> PathBuf {
        self.git_dir.join(path)
    }

    pub fn worktree_path<T: AsRef<Path> + ToString>(&self, path: T) -> Result<PathBuf, PathError> {
        let abs_path = match fs::canonicalize(&path) {
            Ok(p) => p,
            Err(_) => return Err(PathError::new(path, PathErrorKind::InvalidPath)),
        };
        if !abs_path.starts_with(&self.worktree) {
            return Err(PathError::new(path, PathErrorKind::PathOutsideRepo));
        }
        match abs_path.strip_prefix(&self.worktree) {
            Ok(p) => Ok(p.to_path_buf()),
            Err(_) => Err(PathError::new(path, PathErrorKind::PathOutsideRepo)),
        }
    }

    pub fn canon_path<T: AsRef<Path> + ToString>(&self, path: T) -> Result<PathBuf, PathError> {
        let abs_path = match fs::canonicalize(&path) {
            Ok(p) => p,
            Err(_) => return Err(PathError::new(path, PathErrorKind::InvalidPath)),
        };
        if !abs_path.starts_with(&self.worktree) {
            return Err(PathError::new(path, PathErrorKind::PathOutsideRepo));
        }
        Ok(abs_path)
    }

    pub fn file(&self, path: &Path, mkdir: bool) -> Result<Option<PathBuf>, anyhow::Error> {
        let file_name = path.file_name();
        if file_name.is_none() {
            return Err(anyhow!("Path must not be a directory"));
        }
        let base_path = path.parent().unwrap_or(Path::new(""));
        let dir_path = self.dir(base_path, mkdir)?;
        Ok(dir_path.map(|p| p.join(file_name.unwrap())))
    }

    pub fn file_unchecked(&self, path: &Path) -> PathBuf {
        self.file(path, false).unwrap().unwrap()
    }

    pub fn dir(&self, path: &Path, mkdir: bool) -> Result<Option<PathBuf>, anyhow::Error> {
        let path = self.git_dir.join(path);
        check_and_create_dir(path, mkdir)
    }

    pub fn _dir_unchecked(&self, path: &Path) -> PathBuf {
        self.dir(path, false).unwrap().unwrap()
    }

    fn strip_git_dir(&self, path: &Path) -> PathBuf {
        if path.starts_with(&self.git_dir) {
            path.strip_prefix(&self.git_dir).unwrap().to_path_buf()
        } else {
            path.to_path_buf()
        }
    }

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
            let obj = self.read_object(&current_target)?;
            let Some(obj) = obj else {
                return Err(anyhow::Error::from(FindObjectError::none()));
            };
            if stored_object_matches_kind(&kind, &obj) {
                return Ok(current_target);
            }
            if !follow_tags {
                return Err(anyhow::Error::from(FindObjectError::none()));
            }
            match obj {
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
            let head_ref = self._resolve_ref(name)?;
            return match head_ref {
                Some(hr) => Ok(vec![hr]),
                None => Ok(vec![]),
            };
        }

        let mut collected = Vec::<String>::new();
        if is_partial_object_id(name) {
            let mut all_objects = HashSet::<String>::new();
            for loose_object in self.loose_object_store.search_objects(name)? {
                all_objects.insert(loose_object);
            }
            for pack in &self.packs {
                for packed_object in pack.search_objects(name)? {
                    all_objects.insert(packed_object);
                }
            }
            collected.append(&mut all_objects.into_iter().collect());
        }

        let potential_tag = self._resolve_ref(&("refs/tags/".to_string() + name))?;
        if let Some(potential_tag) = potential_tag {
            collected.push(potential_tag);
        }

        let potential_branch = self.branch_store.resolve_branch_target(&BranchSpec::new(name, BranchKind::Local))?;
        if let Some(potential_branch) = potential_branch {
            collected.push(potential_branch); 
        }

        let potential_remote_branches = self.branch_store.search_remotes_for_branch(name)?;
        for remote_branch in potential_remote_branches {
            if let Some(remote_branch_target) = self.branch_store.resolve_branch_target(&remote_branch)? {
                collected.push(remote_branch_target);
            }
        }

        Ok(collected)
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

    pub fn read_raw_object(&self, object_id: &str) -> Result<Option<RawObject>, anyhow::Error> {
        let source = self.find_store_for_object(object_id)?;
        let Some(source) = source else {
            return Ok(None);
        };

        let raw_object = match source {
            ObjectSource::LooseObjectStore => self.loose_object_store.read_object(object_id)?,
            ObjectSource::Pack(i) => self.packs[i].read_object(object_id)?,
        };

        Ok(raw_object)
    }

    pub fn read_object(&self, object_id: &str) -> Result<Option<StoredObject>, anyhow::Error> {
        let raw_object = self.read_raw_object(object_id)?;
        let Some(raw_object) = raw_object else {
            return Ok(None);
        };
        match raw_object.metadata().kind {
            ObjectKind::Blob => Ok(Some(StoredObject::Blob(Blob::deserialise(
                raw_object.content_headless(),
            )))),
            ObjectKind::Commit => Ok(Some(StoredObject::Commit(Commit::deserialise(
                raw_object.content_headless(),
            )))),
            ObjectKind::Tree => Ok(Some(StoredObject::Tree(Tree::deserialise(
                raw_object.content_headless(),
            )))),
            ObjectKind::Tag => Ok(Some(StoredObject::Tag(Tag::deserialise(
                raw_object.content_headless(),
            )))),
        }
    }

    pub fn write_raw_object(&self, obj: &RawObject) -> Result<String, anyhow::Error> {
        self.loose_object_store.write_raw_object(obj)
    }

    pub fn write_object(&self, obj: &impl GitObject) -> Result<String, anyhow::Error> {
        self.loose_object_store.write_object(obj)
    }

    pub fn _resolve_ref(&self, git_ref: &str) -> Result<Option<String>, anyhow::Error> {
        let path = self.file(&PathBuf::from_iter(git_ref.split("/")), false)?;
        let Some(path) = path else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let ref_conts = fs::read_to_string(path)?;
        if let Some(ref_target) = ref_conts.strip_prefix("ref: ") {
            return self._resolve_ref(ref_target.trim());
        }
        Ok(Some(ref_conts.trim().to_string()))
    }

    pub fn ref_list_dir(
        &self,
        path: Option<&Path>,
    ) -> Result<IndexMap<String, String>, anyhow::Error> {
        let (path, root_path) = match path {
            Some(p) => (p, Some(&p.to_path_buf())),
            None => (Path::new("refs"), None),
        };
        self.ref_list_dir_internal(path, root_path)
    }

    fn ref_list_dir_internal(
        &self,
        path: &Path,
        root_path: Option<&PathBuf>,
    ) -> Result<IndexMap<String, String>, anyhow::Error> {
        let path = self.dir(path, true);
        let Ok(path) = path else {
            return Err(path.err().unwrap());
        };
        let Some(path) = path else {
            return Err(anyhow!("Ref path has disappeared"));
        };
        let dir_entries = fs::read_dir(&path)
            .context(format!("Trying to read path {}", &path.to_string_lossy()))?
            .collect::<Result<Vec<_>, std::io::Error>>()?;
        let mut files = dir_entries
            .iter()
            .filter(|e| e.metadata().is_ok_and(|f| f.is_file()))
            .map(|e| e.path())
            .collect::<Vec<PathBuf>>();
        files.sort();
        let mut dirs = dir_entries
            .iter()
            .filter(|e| e.metadata().is_ok_and(|f| f.is_dir()))
            .map(|e| e.path())
            .collect::<Vec<PathBuf>>();
        dirs.sort();
        let mut output = IndexMap::<String, String>::new();
        for f in files {
            let mut stripped_path = self.strip_git_dir(&f);
            let ref_target = self._resolve_ref(&stripped_path.to_string_lossy())?;
            if let Some(rp) = root_path {
                stripped_path = stripped_path.strip_prefix(rp)?.to_path_buf();
            }
            if let Some(ref_target) = ref_target {
                output.insert(stripped_path.to_string_lossy().to_string(), ref_target);
            }
        }
        for d in dirs {
            let mut rec_result = self.ref_list_dir_internal(&self.strip_git_dir(&d), root_path)?;
            output.append(&mut rec_result);
        }
        Ok(output)
    }

    pub fn create_ref(&self, name: &str, target_name: &str) -> Result<(), anyhow::Error> {
        let ref_file_path = self.file(&PathBuf::from_iter(["refs", name]), true)?;
        let Some(ref_file_path) = ref_file_path else {
            return Err(anyhow!("Failure to create ref path"));
        };
        let mut ref_file = File::create(&ref_file_path)?;
        ref_file.write_all(target_name.as_bytes())?;
        ref_file.write_all("\n".as_bytes())?;
        Ok(())
    }

    pub fn read_index(&self) -> Result<Index, anyhow::Error> {
        let file = self.file(Path::new("index"), false)?;
        let file = match file {
            Some(f) => f,
            None => {
                return Ok(Index::new());
            }
        };
        let data = std::fs::read(file).context("error loading index file")?;
        let index = Index::from_bytes(&data).context("malformed index file")?;
        Ok(index)
    }

    pub fn write_index(&self, index: &Index) -> Result<(), anyhow::Error> {
        let tmp_file = self.path(Path::new("index.lck"));
        let final_file = self.path(Path::new("index"));
        let mut data = Vec::<u8>::new();
        index.serialise(&mut data);
        fs::write(&tmp_file, &data).context("error writing temporary index")?;
        fs::rename(&tmp_file, &final_file).context("failed to rename temporary index file")?;
        Ok(())
    }

    pub fn read_ignore_info(&self) -> Result<IgnoreInfo, anyhow::Error> {
        let mut repo_wide_file: PathBuf = self.git_dir.join("info");
        repo_wide_file.push("exclude");
        let repo_file = if repo_wide_file.exists() {
            Some(repo_wide_file)
        } else {
            None
        };

        let config_dir_var = env::var("XDG_CONFIG_HOME");
        let config_dir = match config_dir_var {
            Ok(var) => Some(PathBuf::from_str(&var).unwrap().join("git")),
            Err(_) => env::home_dir().map(|hd| hd.join(".config").join("git")),
        };
        let global_file = if let Some(config_dir) = config_dir {
            let global_exclude_file = config_dir.join("ignore");
            if global_exclude_file.exists() {
                Some(global_exclude_file)
            } else {
                None
            }
        } else {
            None
        };

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

        IgnoreInfo::from_files(global_file, repo_file, scoped_files)
    }

    pub fn current_branch(&self) -> Result<Option<BranchSpec>, anyhow::Error> {
        let head = self
            .file(Path::new("HEAD"), false)
            .context("error finding HEAD")?;
        let Some(head) = head else {
            return Err(anyhow!("missing HEAD"));
        };
        let head_conts = std::fs::read_to_string(head).context("failed to read HEAD")?;
        if let Some(head_target) = head_conts.strip_prefix("ref: ") {
            Ok(Some(BranchSpec::from_str(head_target.trim())?))
        } else {
            Ok(None)
        }
    }

    pub fn current_commit(&self) -> Result<Option<String>, anyhow::Error> {
        if let Some(current_branch) = self.current_branch()? {
            self.branch_store.resolve_branch_target(&current_branch)
        } else {
            self._resolve_ref("HEAD")
        }
    }

    pub fn branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error> {
        let mut branches = self.branch_store.local_branches()?;
        if let Some(cb) = self.current_branch()? {
            if !branches.contains(&cb) {
                branches.push(cb);
            }
        }
        branches.sort();
        Ok(branches)
    }

    pub fn is_branch_name(&self, query_name: &str) -> Result<bool, anyhow::Error> {
        self.branch_store.is_valid(&BranchSpec::new(query_name, BranchKind::Local))
    }

    pub fn update_branch(
        &self,
        branch_name: &str,
        commit_id: &str,
    ) -> Result<(), anyhow::Error> {
        self.branch_store.update_branch(&BranchSpec::new(branch_name, BranchKind::Local), commit_id)
    }

    fn _branch_path<P: AsRef<Path>>(&self, branch_name: P) -> PathBuf {
        self.git_dir.join("refs").join("heads").join(branch_name)
    }

    pub fn update_head(&self, branch_name: &str) -> Result<(), anyhow::Error> {
        self.update_head_detached(&format!("ref: refs/heads/{branch_name}"))
    }

    pub fn update_head_detached(&self, commit_id: &str) -> Result<(), anyhow::Error> {
        write_single_line(self.git_dir.join("HEAD"), commit_id)
    }

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
            let entry_path = entry.path.to_string_lossy();
            let full_path = match prefix {
                "" => entry_path.to_string(),
                _ => format!("{prefix}/{entry_path}"),
            };
            if entry.mode < 0o100000 {
                // Directory
                let subresult = self.flatten_tree_recursive(&entry.object_id, &full_path)?;
                map.extend(subresult.into_iter());
            } else {
                map.insert(full_path, entry.object_id.clone());
            }
        }
        Ok(map)
    }

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

    pub fn add_paths_to_index_and_write<T: AsRef<Path>>(
        &self,
        paths: &[T],
    ) -> Result<(), anyhow::Error> {
        let mut index = self.read_index()?;
        for path in paths {
            let new_entry = self.add_path_partial(path, &mut index)?;
            if let Some(new_entry) = new_entry {
                index.add_unsorted(new_entry);
            }
        }
        index.sort();
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

    pub fn check_index(&self, index: &Index) -> Result<Option<String>, anyhow::Error> {
        for entry in index.entries() {
            if self.resolve_object(&entry.object_id)?.len() != 1 {
                return Ok(Some(entry.object_id.to_string()));
            }
        }
        Ok(None)
    }

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

    pub fn write_ref_log<Tz>(
        &self,
        old_object_id: Option<&str>,
        new_object_id: &str,
        committer_name: &str,
        timestamp: &DateTime<Tz>,
        message: &str,
        branch_name: Option<&str>,
    ) -> Result<(), anyhow::Error>
    where
        Tz: TimeZone,
        Tz::Offset: Display,
    {
        self.ref_log_store.write(
            &RefLogEntry::new(
                old_object_id,
                new_object_id,
                &timestamped_name(committer_name, timestamp),
                message,
            ),
            branch_name,
        )
    }

    pub fn show_ref_log(&self, branch_name: Option<&str>) -> Result<(), anyhow::Error> {
        self.ref_log_store.dump(branch_name)
    }

    pub fn check_ref_log_exists(&self, branch_name: &str) -> Result<bool, anyhow::Error> {
        Ok(self.ref_log_store.check_exists(branch_name))
    }

    pub fn list_ref_logs(&self) -> Result<Vec<String>, anyhow::Error> {
        self.ref_log_store.list_ref_logs()
    }
}

pub fn is_partial_object_id(id: &str) -> bool {
    // IDs are 20 bytes, represented as 40 hex chars; we don't try to identify an ID that's less than 4 chars
    id.len() >= 4 && id.len() <= 40 && id.chars().all(|c| c.is_ascii_hexdigit())
}

enum ObjectSource {
    LooseObjectStore,
    Pack(usize),
}
