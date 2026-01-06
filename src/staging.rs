use anyhow::anyhow;
use std::path::Path;

use crate::shared::repo_find;

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
