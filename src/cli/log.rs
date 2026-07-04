use crate::{
    helpers::find_repo_cwd,
    objects::{ObjectKind, StoredObject},
    output::{OutputMessage, Printer},
    repo::Repository,
};
use std::collections::HashSet;

/// Entry point for the `cv log` command
pub fn cmd(commit: &str, println: &Printer) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(println)?;
    log_from_repo(repo, commit, println)
}

fn log_from_repo(repo: Repository, commit: &str, println: &Printer) -> Result<(), anyhow::Error> {
    let starting_node = repo.find_object(commit, Some(ObjectKind::Commit), true)?;

    println(&OutputMessage::plain("digraph cvlog{{"));
    println(&OutputMessage::plain("  node[shape=rect]"));
    log_commits_graphviz(&repo, &starting_node, println)?;
    println(&OutputMessage::plain("}}"));
    Ok(())
}

/// Create a GraphViz repository graph, starting from a specific commit.
pub fn log_commits_graphviz(
    repo: &Repository,
    commit_id: &str,
    println: &Printer,
) -> Result<(), anyhow::Error> {
    log_commits_graphviz_impl(repo, commit_id, println, &mut HashSet::<String>::new())
}

fn log_commits_graphviz_impl<'a>(
    repo: &'a Repository,
    commit_id: &'a str,
    println: &Printer,
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
        println(&OutputMessage::plain(
            &format!("  c_{commit_id} [label=\"{commit_id_prefix}: {printable_message}\"]"),
            
        ));
        for p in commit.parents().iter() {
            println(&OutputMessage::plain(
                &format!("  c_{commit_id} -> c_{p};"),
            ));
            log_commits_graphviz_impl(repo, p, println, seen)?;
        }
    }
    Ok(())
}
