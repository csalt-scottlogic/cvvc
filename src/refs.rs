use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::shared::{object_write, repo_find, Repository, Tag};

pub fn show_refs() -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    match repo {
        Some(repo) => show_refs_in_repo(&repo),
        None => Ok(()),
    }
}

pub fn show_tags() -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    match repo {
        Some(repo) => show_tags_in_repo(&repo),
        None => Ok(()),
    }
}

pub fn create_tag(name: &str, target: &str, chunky: bool) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    let Some(repo) = repo else { return Ok(()) };
    let absolute_target = repo.find_object(target);
    if chunky {
        create_chunky_tag(&repo, name, target)
    } else {
        repo.ref_create(&format!("tags/{name}"), absolute_target)
    }
}

fn create_chunky_tag(repo: &Repository, name: &str, target: &str) -> Result<(), anyhow::Error> {
    let tag = Tag::create(target, name);
    let tag_id = object_write(&tag, Some(&repo))?;
    let name = format!("tags/{name}");
    repo.ref_create(&name, &tag_id)
}

fn show_refs_in_repo(repo: &Repository) -> Result<(), anyhow::Error> {
    let ref_map = repo.ref_list_dir(None)?;
    print_refs(ref_map, true, "");
    Ok(())
}

fn show_tags_in_repo(repo: &Repository) -> Result<(), anyhow::Error> {
    let ref_map = repo.ref_list_dir(Some(&PathBuf::from_iter(["refs", "tags"])))?;
    print_refs(ref_map, false, "");
    Ok(())
}

fn print_refs(ref_map: IndexMap<String, String>, with_hash: bool, prefix: &str) {
    for item in ref_map {
        if with_hash {
            println!("{} {}{}", item.1, prefix, item.0);
        } else {
            println!("{}{}", prefix, item.0);
        }
    }
}
