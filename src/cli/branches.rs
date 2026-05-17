use crate::{
    config::GlobalConfig, helpers::find_repo_cwd, objects::StoredObject, repo::Repository,
};
use anyhow::anyhow;
use chrono::{Local, Utc};

/// Entry point for the `cv checkout` command
pub fn checkout(target_name: &str, config: &GlobalConfig) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    checkout_from_repo(&repo, target_name, config)
}

/// Entry point for the `cv branch <new-branch>` and `cv checkout -b <new-branch>` commands
pub fn new_branch(
    branch_name: &str,
    checkout: bool,
    config: &GlobalConfig,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    new_branch_in_repo(&repo, branch_name, checkout, config)
}

/// Entry point for the `cv branch --list` command.
pub fn list_branches() -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    list_branches_in_repo(&repo)
}

fn list_branches_in_repo(repo: &Repository) -> Result<(), anyhow::Error> {
    let branches = repo.branches()?;
    let cb = repo.current_branch()?;
    for branch in branches {
        let cb_flag = if cb.as_ref().map(|b| &b.name) == Some(&branch.name) {
            "*"
        } else {
            " "
        };
        println!("{cb_flag} {}", branch.name);
    }
    Ok(())
}

const ILLEGAL_BRANCH_NAMES: [&str; 9] = [
    "HEAD",
    "FETCH_HEAD",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "REBASE_HEAD",
    "REVERT_HEAD",
    "CHERRY_PICK_HEAD",
    "BISECT_HEAD",
    "AUTO_MERGE",
];

fn new_branch_in_repo(
    repo: &Repository,
    branch_name: &str,
    checkout: bool,
    config: &GlobalConfig,
) -> Result<(), anyhow::Error> {
    if ILLEGAL_BRANCH_NAMES.contains(&branch_name) {
        return Err(anyhow!("reserved branch name {branch_name}"));
    }
    if repo.is_branch_name(branch_name)? {
        return Err(anyhow!("Branch {branch_name} exists"));
    }
    let current_commit = repo.current_commit()?;
    if let Some(current_commit) = current_commit {
        repo.update_branch(branch_name, &current_commit)?;
        repo.write_ref_log(
            None,
            &current_commit,
            &config.committer(),
            &Local::now(),
            "branch: Created from HEAD",
            Some(branch_name),
        )?;
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
    config: &GlobalConfig,
) -> Result<(), anyhow::Error> {
    let prev_branch = repo.current_branch()?;
    let prev_commit_id = repo.current_commit()?;
    let target_id = repo.find_object(target_name, None, true)?;
    let obj = repo.read_object(&target_id)?;
    let Some(obj) = obj else {
        return Err(anyhow!("Object {} not found", target_name));
    };
    let StoredObject::Commit(commit) = obj else {
        return Err(anyhow!("Cannot checkout object {target_id} (not a commit)"));
    };

    let tree_entry = commit.tree()?;
    let Some(tree_obj) = repo.read_object(&tree_entry)? else {
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

    let objects_checked_out = tree_obj.checkout(repo, &repo.worktree)?;
    let mut index = repo.read_index()?;
    for item in &objects_checked_out {
        index.update_entry(&item.0, &item.1, &repo.worktree)?;
    }
    index.remove_not_present(
        &objects_checked_out
            .iter()
            .map(|x| x.1.as_str())
            .collect::<Vec<&str>>(),
    );
    repo.write_index(&index)?;

    if repo.is_branch_name(target_name)? {
        repo.update_head(target_name)?;
    } else {
        repo.update_head_detached(&target_id)?;
        println!("HEAD is detached at {target_id}");
    }
    let ref_log_source = prev_branch
        .map(|b| b.name)
        .or_else(|| prev_commit_id.clone())
        .unwrap_or_else(|| "00000000000000000000".to_string());
    let ref_log_message = format!("checkout: moving from {ref_log_source} to {target_id}");
    repo.write_ref_log(
        prev_commit_id.as_deref(),
        &target_id,
        &config.committer(),
        &Utc::now(),
        &ref_log_message,
        None,
    )
}
