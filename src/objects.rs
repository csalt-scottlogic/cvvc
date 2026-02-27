use anyhow::anyhow;
use std::{
    io::{stdout, Write},
    path::PathBuf,
};

use crate::shared::{
    helpers::find_repo_cwd,
    objects::{Blob, ObjectKind, RawObject, StoredObject},
    repo::Repository,
};

pub fn rev_parse(obj_name: &str) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    println!("{}", &repo.find_object(obj_name, None, true)?);
    Ok(())
}

pub fn cat_file(obj_type: &str, obj_name: &str) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    cat_file_from_repo(repo, obj_type, obj_name)
}

pub fn list_tree(recursive: bool, obj_name: &str) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    list_tree_recursive(recursive, &repo, obj_name, None)
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
    let obj = repo.read_object(&obj_hash)?;
    if let Some(obj) = obj {
        let mut buf = Vec::<u8>::new();
        obj.serialise(&mut buf);
        stdout().write_all(&buf)?;
    }
    Ok(())
}

pub fn object_hash(write: bool, filename: &str) -> Result<(), anyhow::Error> {
    let raw_object = RawObject::from_git_object(&Blob::new_from_path(filename)?);
    println!("{}", raw_object.hash());
    if write {
        if let Some(repo) = Repository::find_cwd()? {
            repo.write_raw_object(&raw_object)?;
        }
    }
    Ok(())
}

fn list_tree_recursive(
    recursive: bool,
    repo: &Repository,
    obj_name: &str,
    prefix: Option<&PathBuf>,
) -> Result<(), anyhow::Error> {
    let obj = repo.read_object(obj_name)?;
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
                    item.object_id
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
                item.mode, item_type, item.object_id, path_str
            );
        } else {
            list_tree_recursive(recursive, repo, &item.object_id, Some(&item.path))?;
        }
    }
    Ok(())
}
