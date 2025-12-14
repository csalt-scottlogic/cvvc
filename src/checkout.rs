use crate::shared::{repo_find, Repository, StoredObject};
use anyhow::anyhow;
use std::{fs, path::Path};

pub fn checkout(obj_name: &str, dest: &str) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    match repo {
        Some(repo) => checkout_from_repo(&repo, obj_name, dest),
        None => Ok(()),
    }
}

fn checkout_from_repo(repo: &Repository, obj_name: &str, dest: &str) -> Result<(), anyhow::Error> {
    let obj = repo.object_read(&(repo.find_object(obj_name, None, true)?))?;
    let Some(obj) = obj else {
        return Err(anyhow!("Object {} not found", obj_name));
    };
    let tree_obj = match obj {
        StoredObject::Tree(tree) => tree,
        StoredObject::Commit(commit) => {
            let tree_entry = commit.map().get("tree");
            let Some(tree_entry) = tree_entry else {
                return Err(anyhow!("Commit {} is missing a tree", obj_name));
            };
            let Some(tree_entry) = tree_entry.first() else {
                return Err(anyhow!("Commit {} has an empty tree entry", obj_name));
            };
            let Some(tree_obj) = repo.object_read(&tree_entry)? else {
                return Err(anyhow!("Commit {} points to a non-existent tree", obj_name));
            };
            let StoredObject::Tree(tree_obj) = tree_obj else {
                return Err(anyhow!(
                    "Commit {} points to a non-tree object as its tree",
                    obj_name
                ));
            };
            tree_obj
        }
        _ => {
            return Err(anyhow!(
                "Object {} is not a commit-ish or tree-ish thing",
                obj_name
            ));
        }
    };
    let path = Path::new(dest);
    if path.exists() {
        if !path.is_dir() {
            return Err(anyhow!("Path {} is not a directory", dest));
        }
        if !is_dir_empty(&path)? {
            return Err(anyhow!("Path {} is not empty", dest));
        }
    } else {
        fs::create_dir_all(&path)?;
    }

    tree_obj.checkout(repo, &path.to_path_buf())
}

fn is_dir_empty(dir: &Path) -> Result<bool, anyhow::Error> {
    let mut entries = fs::read_dir(dir)?;
    let first_entry = entries.next();
    Ok(first_entry.is_none())
}
