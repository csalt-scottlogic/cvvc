use crate::{
    helpers::find_repo_cwd,
    objects::{ObjectKind, StoredObject},
    repo::Repository,
};
use std::collections::HashSet;

/// Entry point for the `cv log` command
pub fn cmd(commit: &str) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    log_from_repo(repo, commit)
}

fn log_from_repo(repo: Repository, commit: &str) -> Result<(), anyhow::Error> {
    let mut seen = HashSet::<String>::new();
    let starting_node = repo.find_object(commit, Some(ObjectKind::Commit), true)?;

    println!("digraph cvlog{{");
    println!("  node[shape=rect]");
    log_object_graphviz(&repo, &starting_node, &mut seen)?;
    println!("}}");
    Ok(())
}

/// Create a GraphViz repository graph, starting from a specific commit.
///
/// This function is intended to be called recursively from the tip of a branch,
/// so includes a `seen` parameter which lists commits whose ancestral lines
/// have already been followed.  Callers should pass an empty [`HashSet<String>`].
pub fn log_object_graphviz<'a>(
    repo: &'a Repository,
    commit_id: &'a str,
    seen: &mut HashSet<String>,
) -> Result<(), anyhow::Error> {
    if seen.contains(commit_id) {
        return Ok(());
    }
    seen.insert(String::from(commit_id));

    let commit = repo.read_object(commit_id)?;
    if let Some(StoredObject::Commit(commit)) = commit {
        let message = commit
            .message
            .trim()
            .replace("\\", "\\\\")
            .replace("\"", "\\\"");
        let printable_message = message.split("\n").next().unwrap();
        let commit_id_prefix = if commit_id.len() > 7 {
            &commit_id[..8]
        } else {
            commit_id
        };
        println!("  c_{commit_id} [label=\"{commit_id_prefix}: {printable_message}\"]");
        for p in commit.parents().iter() {
            println!("  c_{commit_id} -> c_{p};");
            log_object_graphviz(repo, p, seen)?;
        }
    }
    Ok(())
}
