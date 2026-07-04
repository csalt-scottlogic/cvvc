use anyhow::anyhow;
use indexmap::IndexMap;

use crate::{
    config::GlobalConfig,
    helpers::{self, find_repo_cwd, is_ref_name_legal},
    objects::Tag,
    output::{OutputMessage, Printer},
    repo::Repository,
    stores::RefTarget,
};

/// Entry point for the `cv show-ref` coommand
pub fn show_refs(println: &Printer) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(println)?;
    show_refs_in_repo(&repo, println)
}

/// Entry point for the `cv tag` command (with no arguments).
pub fn show_tags(println: &Printer) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(println)?;
    show_tags_in_repo(&repo, println)
}

/// Entry point for the `cv tag <new-tag>` command.
pub fn create_tag(
    config: &GlobalConfig,
    name: &str,
    target: &str,
    chunky: bool,
    message: Option<&str>,
    println: &Printer,
) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(println)?;
    if !is_ref_name_legal(name) {
        return Err(anyhow!("illegal ref name"));
    }
    let absolute_target = repo.find_object(target, None, true)?;
    if chunky {
        create_chunky_tag(&repo, config, name, &absolute_target, message)
    } else {
        repo.create_ref(&format!("tags/{name}"), &absolute_target)
    }
}

/// Entry point for the `cv check-ref-format` command.
pub fn check_format(ref_name: &str) -> bool {
    is_ref_name_legal(ref_name)
}

fn create_chunky_tag(
    repo: &Repository,
    config: &GlobalConfig,
    name: &str,
    target: &str,
    message: Option<&str>,
) -> Result<(), anyhow::Error> {
    let tag = Tag::new(
        target,
        name,
        message,
        &config.author(),
        &helpers::now_here(),
    );
    let tag_id = repo.write_object(&tag)?;
    let name = format!("tags/{name}");
    repo.create_ref(&name, &tag_id)
}

fn show_refs_in_repo(repo: &Repository, println: &Printer) -> Result<(), anyhow::Error> {
    let ref_map = repo.ref_list()?;
    print_refs(ref_map, true, "", println);
    Ok(())
}

fn show_tags_in_repo(repo: &Repository, println: &Printer) -> Result<(), anyhow::Error> {
    let ref_map = repo.tag_list()?;
    print_refs(ref_map, false, "", println);
    Ok(())
}

fn print_refs(
    ref_map: IndexMap<String, RefTarget>,
    with_hash: bool,
    prefix: &str,
    println: &Printer,
) {
    for item in ref_map {
        let msg = if with_hash {
            format!("{} {}{}", item.1, prefix, item.0)
        } else {
            format!("{}{}", prefix, item.0)
        };
        println(&OutputMessage::plain(&msg));
    }
}
