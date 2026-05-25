use crate::helpers::find_repo_cwd;

/// List the remote repositories the current repository is linked to.
pub fn list_remotes(_verbose: bool) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let remotes = repo.list_remote_names();
    for remote in remotes {
        println!("{remote}");
    }
    Ok(())
}
