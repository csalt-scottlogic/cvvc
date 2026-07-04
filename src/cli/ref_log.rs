use crate::{
    helpers::find_repo_cwd,
    output::{OutputMessage, Printer},
    stores::{BranchSpec, RefSpec},
};

/// Entry point for the `cv reflog show` command.
pub fn show(branch: Option<&str>, println: &Printer) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(println)?;
    repo.show_ref_log(&branch.map_or(RefSpec::Head, |b| BranchSpec::local(b).into_ref_spec()))
}

/// Entry point for the `cv reflog exists` command.
pub fn exists(branch: &str, println: &Printer) -> Result<bool, anyhow::Error> {
    let repo = find_repo_cwd(println)?;
    repo.check_ref_log_exists(&BranchSpec::local(branch).into_ref_spec())
}

/// Entry point for the `cv reflog list` command.
pub fn list(println: &Printer) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(println)?;
    let ref_logs = repo.list_ref_logs()?;
    for log in ref_logs {
        println(&OutputMessage::plain(&log));
    }
    Ok(())
}
