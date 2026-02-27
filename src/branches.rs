use crate::shared::{config::GlobalConfig, helpers::{find_repo_cwd, shorten_message}, objects::StoredObject, repo::Repository};
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use std::{fs, path::Path, time::SystemTime};

pub fn checkout(target_name: &str, dest: &str, config: &GlobalConfig) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    checkout_from_repo(&repo, target_name, dest, config)
}

pub fn new_branch(branch_name: &str, checkout: bool) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    new_branch_in_repo(&repo, branch_name, checkout)
}

pub fn list_branches() -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    list_branches_in_repo(&repo)
}

fn list_branches_in_repo(repo: &Repository) -> Result<(), anyhow::Error> {
    let branches = repo.branches()?;
    let cb = repo.current_branch()?.unwrap_or_else(String::new);
    for branch in branches {
        let cb_flag = if cb == branch { "*" } else { " " };
        println!("{cb_flag} {branch}");
    }
    Ok(())
}

fn new_branch_in_repo(
    repo: &Repository,
    branch_name: &str,
    checkout: bool,
) -> Result<(), anyhow::Error> {
    if repo.is_branch_name(branch_name)? {
        return Err(anyhow!("Branch {branch_name} exists"));
    }
    let current_commit = repo.resolve_ref("HEAD")?;
    if let Some(current_commit) = current_commit {
        repo.update_branch(branch_name, &current_commit)?;
    }
    if checkout {
        repo.update_head(branch_name)
    } else {
        Ok(())
    }
}

fn checkout_from_repo(
    repo: &Repository,
    target_name: &str,
    dest: &str,
    config: &GlobalConfig
) -> Result<(), anyhow::Error> {
    let prev_commit_id = repo.current_commit()?;
    let target_id = repo.find_object(target_name, None, true)?;
    let obj = repo.read_object(&target_id)?;
    let Some(obj) = obj else {
        return Err(anyhow!("Object {} not found", target_name));
    };
    let StoredObject::Commit(commit) = obj else {
        return Err(anyhow!("Cannot checkout object {target_id} (not a commit)"))
    };

            let tree_entry = commit.map().get("tree");
            let Some(tree_entry) = tree_entry else {
                return Err(anyhow!("Commit {} is missing a tree", target_id));
            };
            let Some(tree_entry) = tree_entry.first() else {
                return Err(anyhow!("Commit {} has an empty tree entry", target_id));
            };
            let Some(tree_obj) = repo.read_object(tree_entry)? else {
                return Err(anyhow!(
                    "Commit {} points to a non-existent tree",
                    target_id
                ));
            };
            let StoredObject::Tree(tree_obj) = tree_obj else {
                return Err(anyhow!(
                    "Commit {} points to a non-tree object as its tree",
                    target_id
                ));
            };

    let path = Path::new(dest);
    if path.exists() {
        if !path.is_dir() {
            return Err(anyhow!("Path {} is not a directory", dest));
        }
        if !is_dir_empty(path)? {
            return Err(anyhow!("Path {} is not empty", dest));
        }
    } else {
        fs::create_dir_all(path)?;
    }

    let objects_checked_out = tree_obj.checkout(repo, path)?;
    let mut index = repo.read_index()?;
    index.remove_not_present(&objects_checked_out);
    repo.write_index(&index)?;

    if repo.is_branch_name(target_name)? {
        repo.update_head(target_name)?;
    } else {
        repo.update_head_detached(&target_id)?;
        println!("HEAD is detached at {target_id}");
    }
    repo.write_ref_log(prev_commit_id.as_deref(), &target_id, &config.committer(), &DateTime::<Utc>::from(SystemTime::now()), &shorten_message("checkout", &commit.message), None)
}

fn is_dir_empty(dir: &Path) -> Result<bool, anyhow::Error> {
    let mut entries = fs::read_dir(dir)?;
    let first_entry = entries.next();
    Ok(first_entry.is_none())
}
