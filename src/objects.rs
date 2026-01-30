use anyhow::anyhow;
use std::{
    io::{stdout, Write},
    path::{Path, PathBuf},
};

use crate::shared::{object_hash_file, repo_find, ObjectKind, Repository, StoredObject};

pub fn rev_parse(obj_name: &str) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    let Some(repo) = repo else { return Ok(()) };
    println!("{}", &repo.find_object(obj_name, None, true)?);
    Ok(())
}

pub fn cat_file(obj_type: &str, obj_name: &str) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    match repo {
        Some(repo) => cat_file_from_repo(repo, obj_type, obj_name),
        None => Ok(()),
    }
}

fn cat_file_from_repo(
    repo: Repository,
    obj_type: &str,
    obj_name: &str,
) -> Result<(), anyhow::Error> {
    let kind = match obj_type {
        "commit" => Some(ObjectKind::Commit),
        "blob" => Some(ObjectKind::Blob),
        "tag" => Some(ObjectKind::Tag),
        "tree" => Some(ObjectKind::Tree),
        _ => {
            return Err(anyhow!("Unknown object type {}", obj_type));
        }
    };
    let obj_hash = repo.find_object(obj_name, kind, false)?;
    let obj = repo.object_read(&obj_hash)?;
    if let Some(obj) = obj {
        let mut buf = Vec::<u8>::new();
        obj.serialise(&mut buf);
        stdout().write_all(&buf)?;
    }
    Ok(())
}

pub fn object_hash(write: bool, obj_type: &str, filename: &str) -> Result<(), anyhow::Error> {
    let repo = if write {
        repo_find(Path::new("."))?
    } else {
        None
    };

    let sha = object_hash_file(filename, obj_type, repo.as_ref())?;
    println!("{}", sha);
    Ok(())
}

pub fn list_tree(recursive: bool, obj_name: &str) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    let Some(repo) = repo else {
        return Err(anyhow!("Not a repository"));
    };
    list_tree_recursive(recursive, &repo, obj_name, None)
}

fn list_tree_recursive(
    recursive: bool,
    repo: &Repository,
    obj_name: &str,
    prefix: Option<&PathBuf>,
) -> Result<(), anyhow::Error> {
    let obj = repo.object_read(obj_name)?;
    let Some(obj) = obj else {
        return Err(anyhow!("Item {obj_name} does not exist"));
    };
    let StoredObject::Tree(obj) = obj else {
        return Err(anyhow!("Item {obj_name} is not a tree"));
    };
    for item in obj.entries() {
        let item_id_bytes = (item.mode >> 12) & 0o77;
        let item_type = match item_id_bytes {
            0o4 => "tree",
            0o10 => "blob",
            0o12 => "blob",   // Actually a symlink
            0o16 => "commit", // Actually a submodule
            _ => {
                return Err(anyhow!(
                    "Unknown mode field {:o} found for tree item {}",
                    item.mode,
                    item.object_name
                ));
            }
        };
        if !(recursive && item_type == "tree") {
            let path_str = match prefix {
                Some(prefix) => prefix.join(&item.path).to_string_lossy().to_string(),
                None => item.path.to_string_lossy().to_string(),
            };
            println!(
                "{:06o} {} {}\t{}",
                item.mode, item_type, item.object_name, path_str
            );
        } else {
            list_tree_recursive(recursive, repo, &item.object_name, Some(&item.path))?;
        }
    }
    Ok(())
}
