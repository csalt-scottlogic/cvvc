use std::{fs, path::{Path, PathBuf}};

use crate::{helpers::fs::{check_and_create_dir, path_translate, path_translate_rev, walk_fs, write_single_line}, stores::{BranchKind, BranchSpec, BranchStore}};

pub struct BranchFileStore {
    base_path: PathBuf,
}

impl BranchFileStore {
    pub fn new<P: AsRef<Path>>(base_path: P) -> Self {
        Self {
            base_path: base_path.as_ref().to_path_buf()
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
    fn create(&self) -> Result<(), anyhow::Error> {
        check_and_create_dir(self.local_path(), true)?;
        check_and_create_dir(self.remote_path(), true)?;
        Ok(())
    }

    fn is_valid(&self, branch: &BranchSpec) -> Result<bool, anyhow::Error> {
        let branch_path = self.base_path.join(PathBuf::from(branch));
        Ok(branch_path.try_exists()? && branch_path.is_file())
    }

    fn local_branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error> {
        let mut results = Vec::<BranchSpec>::new();
        let local_path = self.local_path();
        for dir_entry in walk_fs(&local_path)? {
            let dir_entry = dir_entry?;
            results.push(BranchSpec::new(&path_translate(dir_entry.strip_prefix(&local_path)?), BranchKind::Local));
        }
        Ok(results)
    }
    
    fn resolve_branch_target(&self, branch: &BranchSpec) -> Result<Option<String>, anyhow::Error> {
        let branch_path = self.base_path.join(PathBuf::from(branch));
        if !branch_path.exists() || !branch_path.is_file() {
            return Ok(None);
        }
        let ref_conts = fs::read_to_string(branch_path)?;
        Ok(Some(ref_conts))
    }

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
                        results.push(BranchSpec::new(&found_name, BranchKind::Remote(dir_entry.file_name().to_string_lossy().to_string())));
                        break;
                    }
                }
            }
        }
        Ok(results)
    }

    fn update_branch(&self, branch: &BranchSpec, commit_id: &str) -> Result<(), anyhow::Error> {
        let branch_path = self.base_path.join(PathBuf::from(branch));
        write_single_line(branch_path, commit_id)
    }
}

impl From<&BranchSpec> for PathBuf {
    fn from(value: &BranchSpec) -> Self {
        let start = PathBuf::from(&value.kind);
        start.join(&path_translate_rev(&value.name))
    }
}

impl From<&BranchKind> for PathBuf {
    fn from(value: &BranchKind) -> Self {
        let start = Path::new("refs");
        match value {
            BranchKind::Local => start.join("heads"),
            BranchKind::Remote(r) => start.join("remotes").join(r),
        }
    }
}