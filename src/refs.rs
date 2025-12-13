use std::path::Path;

use indexmap::IndexMap;

use crate::shared::{repo_find, Repository};

pub fn show_refs() -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    match repo {
        Some(repo) => show_refs_in_repo(&repo),
        None => Ok(()),
    }
}

fn show_refs_in_repo(repo: &Repository) -> Result<(), anyhow::Error> {
    let ref_map = repo.ref_list_dir(None)?;
    print_refs(ref_map, true, "");
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
