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
    output::{OutputMessage, OutputService},
    repo::Repository,
    stores::RefSpec,
};

/// Entry point for the `cv where` command.
pub fn current_branch_and_commit(printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    let branch = repo.current_branch()?;
    let commit = repo.current_commit()?;
    printer.println(&OutputMessage::plain(&format!(
        "Branch: {}",
        branch
            .map(|b| b.name)
            .unwrap_or_else(|| "[none]".to_string())
    )));
    printer.println(&OutputMessage::plain(&format!(
        "Commit: {}",
        commit.unwrap_or_else(|| "[none]".to_string())
    )));
    Ok(())
}

/// Entry point for the `cv ls-commits` command.
pub fn list_commits(start: Option<&str>, printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    let commits = repo.commits(start)?;
    for commit in commits {
        let commit = commit?;
        printer.println(&OutputMessage::plain(&commit));
    }
    Ok(())
}

/// Entry point for the `cv ls-files` command.
pub fn list_files(verbose: bool, printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    let index = repo.read_index()?;
    if verbose {
        printer.println(&OutputMessage::plain(&format!(
            "Index file format v{}, containing {} entries",
            index.version,
            index.entries().len()
        )));
    }
    for entry in index.entries() {
        printer.println(&OutputMessage::plain(&entry.object_name));
        if verbose {
            printer.println(&OutputMessage::plain(&format!(
                "  {} with perms: {}",
                entry.mode_type, entry.mode_perms
            )));
            printer.println(&OutputMessage::plain(&format!(
                "  on blob {}",
                entry.object_id
            )));
            printer.println(&OutputMessage::plain(&format!("  size {}", entry.fsize)));
            printer.println(&OutputMessage::plain(&format!(
                "  created {}, modified {}",
                entry.ctime, entry.mtime
            )));
            printer.println(&OutputMessage::plain(&format!(
                "  device {}, inode {}",
                entry.dev, entry.ino
            )));
            printer.println(&OutputMessage::plain(&format!(
                "  user {}, group {}",
                entry.uid, entry.gid
            )));
            printer.println(&OutputMessage::plain(&format!(
                "  flags: stage={}, assume_valid={}",
                entry.flag_stage, entry.flag_assume_valid
            )));
        }
    }
    Ok(())
}

/// Entry point for the `cv check-ignore` command.
pub fn check_ignore(paths: &[String], printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    let ignore_rules = repo.read_ignore_info()?;
    for path in paths {
        if ignore_rules.check(Path::new(path)) {
            printer.println(&OutputMessage::plain(path));
        }
    }
    Ok(())
}

/// Entry point for the `cv rm` command.
pub fn remove_files(
    paths: &[String],
    index_only: bool,
    ignore_no_matches: bool,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    let mut some_removed = false;
    let mut index = repo.read_index()?;
    for path in paths {
        if repo.remove_path_from_index(path, &mut index, !index_only)? {
            some_removed = true;
            printer.println(&OutputMessage::plain(path));
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
pub fn add_files(paths: &[String], printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    repo.add_paths_to_index_and_write(paths)?;
    Ok(())
}

/// Entry point for the `cv status` command.
pub fn status(printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    status_branch(&repo, printer)?;
    let staged_changes = status_index(&repo, printer)?;
    let unstaged_changes = status_worktree(&repo, printer)?;
    if unstaged_changes {
        if !staged_changes {
            printer.println(&OutputMessage::plain(
                "no changes added to commit (use \"cv add\")",
            ));
        }
    } else if !staged_changes {
        printer.println(&OutputMessage::plain(
            "nothing to commit, working tree clean",
        ));
    }
    printer.println_empty();
    Ok(())
}

/// Entry point for the `cv write-tree` command.
pub fn store_index_as_tree(
    no_checks: bool,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    printer.println(&OutputMessage::plain(&store_index_as_tree_repo(
        &repo, no_checks,
    )?));
    Ok(())
}

/// Entry point for the `cv commit-tree` command.
pub fn create_commit_for_tree(
    tree_id: &str,
    parents: &[String],
    message: &str,
    config: &GlobalConfig,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
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
    printer.println(&OutputMessage::plain(&commit_id));
    Ok(())
}

/// Entry point for the `cv commit` command.
pub fn full_commit(
    config: &GlobalConfig,
    message: Option<String>,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    let start_commit = repo.current_commit()?;
    let tree_id = store_index_as_tree_repo(&repo, false)?;
    let parent_id = repo.current_commit()?;
    let timestamp = helpers::now_here();
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
        repo.update_local_branch(&branch.name, &commit_id)?
    } else {
        repo.update_head_detached(&commit_id)?
    }
    let (reflog_refspec, also_update_head) =
        current_branch.map_or((RefSpec::Head, false), |b| (b.into_ref_spec(), true));
    repo.write_ref_log(
        start_commit.as_deref(),
        &commit_id,
        &config.committer(),
        &shorten_and_prefix_message("commit", message),
        &reflog_refspec,
        also_update_head,
    )
}

fn status_branch(repo: &Repository, printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let branch = repo.current_branch()?;
    match branch {
        Some(name) => {
            printer.println(&OutputMessage::plain(&format!("On branch {name}")));
        }
        None => {
            let head_commit = repo.current_commit()?;
            if let Some(head_commit) = head_commit {
                printer.println(&OutputMessage::plain(&format!(
                    "HEAD detached at {head_commit}"
                )));
            } else {
                return Err(anyhow!("missing head"));
            }
        }
    };
    printer.println_empty();
    Ok(())
}

fn status_index(repo: &Repository, printer: &dyn OutputService) -> Result<bool, anyhow::Error> {
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
        printer.println(&OutputMessage::plain("Changes to be committed:"));
        for line in to_print {
            printer.println(&OutputMessage::plain(&line));
        }
        printer.println_empty();
    }
    Ok(printable)
}

fn status_worktree(repo: &Repository, printer: &dyn OutputService) -> Result<bool, anyhow::Error> {
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
                let raw_obj = RawObject::from(&Blob::new_from_path(&entry_full_path)?);
                if raw_obj.object_id() != entry.object_id {
                    to_print.push(format!("\tmodified:   {}", entry.object_name));
                }
            }
        }
        files.retain(|f| *f != entry.object_name);
    }
    let mut printable = !to_print.is_empty();
    if printable {
        printer.println(&OutputMessage::plain("Changes not staged for commit:"));
        for line in to_print {
            printer.println(&OutputMessage::plain(&line));
        }
        printer.println_empty();
    }
    if !files.is_empty() {
        printable = true;
        printer.println(&OutputMessage::plain("Untracked files:"));
        for f in files {
            printer.println(&OutputMessage::plain(&format!("\t{f}")));
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
