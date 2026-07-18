use crate::{
    config::GlobalConfig,
    helpers::{find_repo_cwd, is_ref_name_legal},
    objects::StoredObject,
    output::{OutputMessage, OutputService},
    repo::Repository,
    stores::{BranchLocation, BranchSpec, RefSpec, RefTarget},
};
use anyhow::anyhow;
use colored::Colorize;

/// Entry point for the `cv checkout` command
pub fn checkout(
    target_name: &str,
    config: &GlobalConfig,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    checkout_from_repo(&repo, target_name, config, printer)
}

/// Entry point for the `cv branch <new-branch>` and `cv checkout -b <new-branch>` commands
pub fn new_branch(
    branch_name: &str,
    checkout: bool,
    config: &GlobalConfig,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    new_branch_in_repo(&repo, branch_name, checkout, config)
}

/// Entry point for the `cv branch --list` command.
pub fn list_branches(list_all: bool, printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    list_branches_in_repo(&repo, list_all, printer)
}

fn list_branches_in_repo(
    repo: &Repository,
    list_all: bool,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    let branches = repo.branches()?;
    let cb = repo.current_branch()?;
    for branch in branches
        .into_iter()
        .filter(|b| list_all || (b.location == BranchLocation::Local))
    {
        let is_current = cb.as_ref().map(|b| *b == branch).unwrap_or(false);
        let plain_string = if is_current {
            format!("* {}", branch.distinguished_name())
        } else {
            format!("  {}", branch.distinguished_name())
        };
        let coloured_string = if is_current {
            format!("* {}", branch.distinguished_name().green())
        } else if branch.location != BranchLocation::Local {
            format!("  {}", branch.distinguished_name().red())
        } else {
            format!("  {}", branch.distinguished_name())
        };
        printer.println(&OutputMessage::new(&plain_string, Some(&coloured_string)));
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
    if !is_ref_name_legal(branch_name) {
        return Err(anyhow!("illegal ref name"));
    }
    if ILLEGAL_BRANCH_NAMES.contains(&branch_name) {
        return Err(anyhow!("reserved branch name {branch_name}"));
    }
    if repo.is_branch_name(branch_name)? {
        return Err(anyhow!("Branch {branch_name} exists"));
    }
    let current_commit = repo.current_commit()?;
    if let Some(current_commit) = current_commit {
        repo.update_local_branch(branch_name, &current_commit)?;
        repo.write_ref_log(
            None,
            &current_commit,
            &config.committer(),
            "branch: Created from HEAD",
            &BranchSpec::local(branch_name).into_ref_spec(),
            true,
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
    printer: &dyn OutputService,
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
    } else if repo.is_remote_branch_name(target_name)? {
        repo.update_local_branch(target_name, &target_id)?;
        repo.update_head(target_name)?;
    } else {
        repo.update_head_detached(&target_id)?;
        printer.println(&OutputMessage::plain(&format!(
            "HEAD is detached at {target_id}"
        )));
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
        &ref_log_message,
        &RefSpec::Head,
        false,
    )
}

/// Entry point for `cv branch -d` and `cv branch -D`
pub fn delete_branch(
    branch: &str,
    force_delete: bool,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    delete_branch_from_repo(&mut find_repo_cwd(printer)?, branch, force_delete, printer)
}

fn delete_branch_from_repo(
    repo: &mut Repository,
    branch: &str,
    force_delete: bool,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    if !repo.is_branch_name(branch)? {
        return Err(anyhow!("branch not found"));
    }
    let branch_spec = BranchSpec::local(branch);
    if repo
        .current_branch()?
        .map(|b| b == branch_spec)
        .unwrap_or_default()
    {
        return Err(anyhow!("cannot delete the current branch"));
    }
    let ref_spec = branch_spec.clone().into_ref_spec();
    let branch_target = repo.resolve_ref(&ref_spec)?;
    if !force_delete {
        let current_commit = repo.current_commit()?;
        if let (Some(current_commit), Some(RefTarget::Object(branch_target))) =
            (current_commit, &branch_target)
        {
            if !repo.commit_is_ancestor(&current_commit, branch_target)? {
                return Err(anyhow!("Branch is not fully merged to HEAD"));
            }
        }
    }
    repo.delete_ref(&ref_spec)?;
    repo.delete_ref_log(&ref_spec)?;
    repo.delete_branch_config(&branch_spec)?;
    printer.println(&OutputMessage::plain(&format!(
        "Deleted branch {} (was {})",
        branch,
        branch_target
            .map(|x| x.name())
            .unwrap_or_else(|| "nothing".to_string())
    )));
    Ok(())
}
