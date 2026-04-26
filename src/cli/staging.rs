use anyhow::{anyhow, Context};
use chrono::{DateTime, TimeZone, Utc};
use std::{fmt::Display, fs, path::Path, time::SystemTime};

use crate::{
    config::GlobalConfig,
    helpers::{
        self, find_repo_cwd,
        fs::{path_translate, path_translate_rev, walk_fs_pruned},
        shorten_and_prefix_message,
    },
    objects::{Blob, Commit, RawObject},
    repo::Repository,
};

/// Entry point for the `cv ls-files` command.
pub fn list_files(verbose: bool) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let index = repo.read_index()?;
    if verbose {
        println!(
            "Index file format v{}, containing {} entries",
            index.version,
            index.entries().len()
        );
    }
    for entry in index.entries() {
        println!("{}", entry.object_name);
        if verbose {
            println!("  {} with perms: {}", entry.mode_type, entry.mode_perms);
            println!("  on blob {}", entry.object_id);
            println!("  size {}", entry.fsize);
            println!("  created {}, modified {}", entry.ctime, entry.mtime);
            println!("  device {}, inode {}", entry.dev, entry.ino);
            println!("  user {}, group {}", entry.uid, entry.gid);
            println!(
                "  flags: stage={}, assume_valid={}",
                entry.flag_stage, entry.flag_assume_valid
            );
        }
    }
    Ok(())
}

/// Entry point for the `cv check-ignore` command.
pub fn check_ignore(paths: &[String]) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let ignore_rules = repo.read_ignore_info()?;
    for path in paths {
        if ignore_rules.check(Path::new(path)) {
            println!("{path}");
        }
    }
    Ok(())
}

/// Entry point for the `cv rm` command.
pub fn remove_files(
    paths: &[String],
    index_only: bool,
    ignore_no_matches: bool,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let mut some_removed = false;
    let mut index = repo.read_index()?;
    for path in paths {
        if repo.remove_path_from_index(path, &mut index, !index_only)? {
            some_removed = true;
            println!("{path}");
        }
    }
    if some_removed {
        repo.write_index(&index)?;
    } else if !ignore_no_matches {
        return Err(anyhow!("no files removed"));
    }
    Ok(())
}

/// Entry point for the `cv add` command.
pub fn add_files(paths: &[String]) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    repo.add_paths_to_index_and_write(paths)?;
    Ok(())
}

/// Entry point for the `cv status` command.
pub fn status() -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    status_branch(&repo)?;
    let staged_changes = status_index(&repo)?;
    let unstaged_changes = status_worktree(&repo)?;
    if unstaged_changes {
        if !staged_changes {
            println!("no changes added to commit (use \"cv add\")");
        }
    } else if !staged_changes {
        println!("nothing to commit, working tree clean");
    }
    println!();
    Ok(())
}

/// Entry point for the `cv write-tree` command.
pub fn store_index_as_tree(no_checks: bool) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    println!("{}", store_index_as_tree_repo(&repo, no_checks)?);
    Ok(())
}

/// Entry point for the `cv commit-tree` command.
pub fn create_commit_for_tree(
    tree_id: &str,
    parents: &[String],
    message: &str,
    config: &GlobalConfig,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let parent_id = if !parents.is_empty() {
        Some(parents[0].as_str())
    } else {
        None
    };
    let commit = Commit::new(
        tree_id,
        parent_id,
        &config.author(),
        &config.committer(),
        &DateTime::<Utc>::from(SystemTime::now()),
        message,
    );
    let commit_id = repo.write_object(&commit)?;
    println!("{commit_id}");
    Ok(())
}

/// Entry point for the `cv commit` command.
pub fn full_commit(config: &GlobalConfig, message: Option<String>) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let start_commit = repo.current_commit()?;
    let tree_id = store_index_as_tree_repo(&repo, false)?;
    let parent_id = repo.current_commit()?;
    let timestamp = helpers::now();
    let message = message
        .as_deref()
        .unwrap_or("User forgot to enter commit message");
    let commit_id = create_commit_for_repo_tree(
        &repo,
        &tree_id,
        parent_id.as_deref(),
        message,
        &timestamp,
        config,
    )?;
    let current_branch = repo.current_branch()?;
    if let Some(ref branch) = current_branch {
        repo.update_branch(&branch.name, &commit_id)?
    } else {
        repo.update_head_detached(&commit_id)?
    }
    let current_branch_name = current_branch.map(|b| b.name.to_string());
    repo.write_ref_log(
        start_commit.as_deref(),
        &commit_id,
        &config.committer(),
        &timestamp,
        &shorten_and_prefix_message("commit", message),
        current_branch_name.as_deref(),
    )
}

fn status_branch(repo: &Repository) -> Result<(), anyhow::Error> {
    let branch = repo.current_branch()?;
    match branch {
        Some(name) => {
            println!("On branch {name}");
        }
        None => {
            let head_commit = repo.current_commit()?;
            if let Some(head_commit) = head_commit {
                println!("HEAD detached at {head_commit}");
            } else {
                return Err(anyhow!("missing head"));
            }
        }
    };
    println!();
    Ok(())
}

fn status_index(repo: &Repository) -> Result<bool, anyhow::Error> {
    let mut to_print = Vec::<String>::new();
    let mut committed_tree = repo.flatten_head_tree()?;
    let index = repo.read_index()?;
    for entry in index.entries() {
        if committed_tree.contains_key(&entry.object_name) {
            if committed_tree[&entry.object_name] != entry.object_id {
                to_print.push(format!("\tmodified:   {}", entry.object_name));
            }
            committed_tree.remove(&entry.object_name);
        } else {
            to_print.push(format!("\tadded:      {}", entry.object_name));
        }
    }
    for entry in committed_tree.keys() {
        to_print.push(format!("\tdeleted:    {}", entry));
    }
    let printable = !to_print.is_empty();
    if printable {
        println!("Changes to be committed:");
        for line in to_print {
            println!("{line}");
        }
        println!();
    }
    Ok(printable)
}

fn status_worktree(repo: &Repository) -> Result<bool, anyhow::Error> {
    let ignore_info = repo.read_ignore_info()?;
    let mut files = Vec::<String>::new();
    let mut to_print = Vec::<String>::new();
    let index = repo.read_index()?;

    for f in walk_fs_pruned(&repo.worktree, &|p| {
        let rel_p = p.strip_prefix(&repo.worktree);
        let Ok(rel_p) = rel_p else {
            return true;
        };
        p.starts_with(&repo.git_dir) || ignore_info.check(rel_p)
    })? {
        let Ok(f) = f else {
            return Err(f.context("error reading worktree").unwrap_err());
        };
        let rel_path = f
            .strip_prefix(&repo.worktree)
            .context("error converting worktree path to relative path")?;
        files.push(path_translate(rel_path));
    }

    for entry in index.entries() {
        let entry_full_path = repo.worktree.join(path_translate_rev(&entry.object_name));
        if !entry_full_path.exists() {
            to_print.push(format!("\tdeleted: {}", entry.object_name));
        } else {
            let stat = fs::metadata(&entry_full_path).context("could not read file metadata")?;
            // CTime is not available at present on WSL
            let file_ctime: Option<DateTime<Utc>> = match stat.created() {
                Ok(ct) => Some(ct.into()),
                Err(_) => None,
            };
            let file_mtime: DateTime<Utc> = stat.modified()?.into();
            let ctimes_differ = match file_ctime {
                Some(ctime) => entry.ctime != ctime,
                None => false,
            };
            if ctimes_differ || entry.mtime != file_mtime {
                // Timestamps differ; check content.
                let raw_obj = RawObject::from_git_object(&Blob::new_from_path(&entry_full_path)?);
                if raw_obj.object_id() != entry.object_id {
                    to_print.push(format!("\tmodified:   {}", entry.object_name));
                }
            }
        }
        files.retain(|f| *f != entry.object_name);
    }
    let mut printable = !to_print.is_empty();
    if printable {
        println!("Changes not staged for commit:");
        for line in to_print {
            println!("{line}");
        }
        println!();
    }
    if !files.is_empty() {
        printable = true;
        println!("Untracked files:");
        for f in files {
            println!("\t{f}");
        }
    }
    Ok(printable)
}

fn store_index_as_tree_repo(repo: &Repository, no_checks: bool) -> Result<String, anyhow::Error> {
    let index = repo.read_index()?;
    if !no_checks {
        if let Some(obj_id) = repo.check_index(&index)? {
            return Err(anyhow!("Object {obj_id} is missing"));
        }
    }
    repo.store_index(&index)
}

fn create_commit_for_repo_tree<Tz>(
    repo: &Repository,
    tree_id: &str,
    parent: Option<&str>,
    message: &str,
    timestamp: &DateTime<Tz>,
    config: &GlobalConfig,
) -> Result<String, anyhow::Error>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let commit = Commit::new(
        tree_id,
        parent,
        &config.author(),
        &config.committer(),
        timestamp,
        message,
    );
    let commit_id = repo.write_object(&commit)?;
    Ok(commit_id)
}
