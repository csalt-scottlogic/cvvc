use crate::helpers::find_repo_cwd;

pub fn show(branch: Option<&str>) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    repo.show_ref_log(branch)
}

pub fn exists(branch: &str) -> Result<bool, anyhow::Error> {
    let repo = find_repo_cwd()?;
    repo.check_ref_log_exists(branch)
}

pub fn list() -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let ref_logs = repo.list_ref_logs()?;
    for log in ref_logs {
        println!("{log}");
    }
    Ok(())
}
