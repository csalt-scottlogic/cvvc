use crate::{
    helpers::find_repo_cwd,
    output::{OutputMessage, OutputService},
    stores::{BranchSpec, RefSpec},
};

/// Entry point for the `cv reflog show` command.
pub fn show(branch: Option<&str>, printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    repo.show_ref_log(&branch.map_or(RefSpec::Head, |b| BranchSpec::local(b).into_ref_spec()))
}

/// Entry point for the `cv reflog exists` command.
pub fn exists(branch: &str, printer: &dyn OutputService) -> Result<bool, anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    repo.check_ref_log_exists(&BranchSpec::local(branch).into_ref_spec())
}

/// Entry point for the `cv reflog list` command.
pub fn list(printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    let ref_logs = repo.list_ref_logs()?;
    for log in ref_logs {
        printer.println(&OutputMessage::plain(&log));
    }
    Ok(())
}
