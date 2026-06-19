use crate::{
    helpers::find_repo_cwd,
    stores::{BranchSpec, RefSpec},
};

/// Entry point for the `cv reflog show` command.
pub fn show(branch: Option<&str>) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    repo.show_ref_log(&branch.map_or(RefSpec::Head, |b| BranchSpec::local(b).into_ref_spec()))
}

/// Entry point for the `cv reflog exists` command.
pub fn exists(branch: &str) -> Result<bool, anyhow::Error> {
    let repo = find_repo_cwd()?;
    repo.check_ref_log_exists(&BranchSpec::local(branch).into_ref_spec())
}

/// Entry point for the `cv reflog list` command.
pub fn list() -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let ref_logs = repo.list_ref_logs()?;
    for log in ref_logs {
        println!("{log}");
    }
    Ok(())
}
