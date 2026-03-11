use crate::{
    helpers::find_repo_cwd,
    objects::{ObjectKind, StoredObject},
    repo::Repository,
};
use std::collections::HashSet;

pub fn cmd(commit: &str) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    log_from_repo(repo, commit)
}

pub fn log_from_repo(repo: Repository, commit: &str) -> Result<(), anyhow::Error> {
    let mut seen = HashSet::<String>::new();
    let starting_node = repo.find_object(commit, Some(ObjectKind::Commit), true)?;

    println!("digraph cvlog{{");
    println!("  node[shape=rect]");
    log_object_graphviz(&repo, &starting_node, &mut seen)?;
    println!("}}");
    Ok(())
}

pub fn log_object_graphviz<'a>(
    repo: &'a Repository,
    object_name: &'a str,
    seen: &mut HashSet<String>,
) -> Result<(), anyhow::Error> {
    if seen.contains(object_name) {
        return Ok(());
    }
    seen.insert(String::from(object_name));

    let commit = repo.read_object(object_name)?;
    if let Some(StoredObject::Commit(commit)) = commit {
        let message = commit
            .message
            .trim()
            .replace("\\", "\\\\")
            .replace("\"", "\\\"");
        let printable_message = message.split("\n").next().unwrap();
        let object_name_start = if object_name.len() > 7 {
            &object_name[..8]
        } else {
            object_name
        };
        println!("  c_{object_name} [label=\"{object_name_start}: {printable_message}\"]");

        if commit.map().contains_key("parent") {
            for p in commit.map()["parent"].iter() {
                println!("  c_{object_name} -> c_{p};");
                log_object_graphviz(repo, p, seen)?;
            }
        }
    }
    Ok(())
}
