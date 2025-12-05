use crate::shared::{repo_find, Repository, StoredObject};
use std::{collections::HashSet, path::Path};

pub fn cmd(commit: &str) {
    let repo = repo_find(Path::new("."));
    if let Ok(Some(the_repo)) = repo {
        log_from_repo(the_repo, commit)
    }
}

pub fn log_from_repo(repo: Repository, commit: &str) {
    let mut seen = HashSet::<String>::new();

    println!("digraph ryaglog{{");
    println!("  node[shape=rect]");
    log_object_graphviz(&repo, repo.find_object(commit), &mut seen);
    println!("}}");
}

pub fn log_object_graphviz<'a>(
    repo: &'a Repository,
    object_name: &'a str,
    seen: &mut HashSet<String>,
) {
    if seen.contains(object_name) {
        return;
    }
    seen.insert(String::from(object_name));

    let commit = repo.object_read(object_name).unwrap();
    if let Some(StoredObject::Commit(commit)) = commit {
        let message = commit
            .message
            .trim()
            .replace("\\", "\\\\")
            .replace("\"", "\\\"");
        let printable_message = message.split("\n").next().unwrap();
        let object_name_start: &str;
        if object_name.len() > 7 {
            object_name_start = &object_name[..8];
        } else {
            object_name_start = object_name;
        }
        println!("  c_{object_name} [label=\"{object_name_start}: {printable_message}\"]");

        if commit.map().contains_key("parent") {
            for p in commit.map()["parent"].iter() {
                println!("  c_{object_name} -> c_{p};");
                log_object_graphviz(repo, &p, seen);
            }
        }
    }
}
