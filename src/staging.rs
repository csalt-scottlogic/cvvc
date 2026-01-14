use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use std::{fs, path::Path};

use crate::shared::{
    helpers::{path_translate, path_translate_rev, walk_fs_pruned},
    object_hash_file, repo_find, Repository,
};

pub fn list_files(verbose: bool) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    let Some(repo) = repo else { return Ok(()) };
    let index = repo.index_read()?;
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
            let entry_type = match entry.mode_type {
                0b1000 => "regular file",
                0b1010 => "symlink",
                0b1110 => "git link",
                _ => {
                    return Err(anyhow!("Unknown index entry mode {}", entry.mode_type));
                }
            };
            println!("  {} with perms: {:04o}", entry_type, entry.mode_perms);
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

pub fn check_ignore(paths: &[String]) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    let Some(repo) = repo else { return Ok(()) };
    let ignore_rules = repo.ignore_info_read()?;
    for path in paths {
        if ignore_rules.check(Path::new(path)) {
            println!("{path}");
        }
    }
    Ok(())
}

pub fn remove_files(paths: &[String], index_only: bool, ignore_no_matches: bool) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    let Some(repo) = repo else { return Ok(()) };
    let mut some_removed = false;
    let mut index = repo.index_read()?;
    for path in paths {
        if repo.remove_path(path, &mut index, !index_only)? {
            some_removed = true;
            println!("{path}");
        }
    }
    if some_removed {
        repo.index_write(&index)?;
    } else if !ignore_no_matches {
        return Err(anyhow!("no files removed"));
    }
    Ok(())
}

pub fn status() -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    let Some(repo) = repo else { return Ok(()) };
    status_branch(&repo)?;
    let staged_changes = status_index(&repo)?;
    let unstaged_changes = status_worktree(&repo)?;
    if unstaged_changes {
        if !staged_changes {
            println!("no changes added to commit (use \"ryag add\")");
        }
    } else if !staged_changes {
        println!("nothing to commit, working tree clean");
    }
    println!("");
    Ok(())
}

fn status_branch(repo: &Repository) -> Result<(), anyhow::Error> {
    let branch = repo.current_branch()?;
    match branch {
        Some(name) => {
            println!("On branch {name}");
        }
        None => {
            let head_commit = repo.ref_resolve("HEAD")?;
            if let Some(head_commit) = head_commit {
                println!("HEAD detached at {head_commit}");
            } else {
                return Err(anyhow!("missing head"));
            }
        }
    };
    println!("");
    Ok(())
}

fn status_index(repo: &Repository) -> Result<bool, anyhow::Error> {
    let mut to_print = Vec::<String>::new();
    let mut committed_tree = repo.flatten_head_tree()?;
    let index = repo.index_read()?;
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
    let printable = to_print.len() > 0;
    if printable {
        println!("Changes to be committed:");
        for line in to_print {
            println!("{line}");
        }
        println!("");
    }
    Ok(printable)
}

fn status_worktree(repo: &Repository) -> Result<bool, anyhow::Error> {
    let ignore_info = repo.ignore_info_read()?;
    let mut files = Vec::<String>::new();
    let mut to_print = Vec::<String>::new();
    let index = repo.index_read()?;

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
                let new_id = object_hash_file(&entry_full_path, "blob", Some(repo))?;
                if new_id != entry.object_id {
                    to_print.push(format!("\tmodified:   {}", entry.object_name));
                }
            }
        }
        files.retain(|f| *f != entry.object_name);
    }
    let mut printable = to_print.len() > 0;
    if printable {
        println!("Changes not staged for commit:");
        for line in to_print {
            println!("{line}");
        }
        println!("");
    }
    if files.len() > 0 {
        printable = true;
        println!("Untracked files:");
        for f in files {
            println!("\t{f}");
        }
    }
    Ok(printable)
}
