use std::path::PathBuf;

use indexmap::IndexMap;

use crate::{
    config::GlobalConfig,
    helpers::{self, find_repo_cwd},
    objects::Tag,
    repo::Repository,
};

/// Entry point for the `cv show-ref` coommand
pub fn show_refs() -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    show_refs_in_repo(&repo)
}

/// Entry point for the `cv tag` command (with no arguments).
pub fn show_tags() -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    show_tags_in_repo(&repo)
}

/// Entry point for the `cv tag <new-tag>` command.
pub fn create_tag(
    config: &GlobalConfig,
    name: &str,
    target: &str,
    chunky: bool,
    message: Option<&str>,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let absolute_target = repo.find_object(target, None, true)?;
    if chunky {
        create_chunky_tag(&repo, config, name, &absolute_target, message)
    } else {
        repo.create_ref(&format!("tags/{name}"), &absolute_target)
    }
}

fn create_chunky_tag(
    repo: &Repository,
    config: &GlobalConfig,
    name: &str,
    target: &str,
    message: Option<&str>,
) -> Result<(), anyhow::Error> {
    let tag = Tag::create(target, name, message, &config.author(), &helpers::now());
    let tag_id = repo.write_object(&tag)?;
    let name = format!("tags/{name}");
    repo.create_ref(&name, &tag_id)
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
