use crate::repo::Repository;

/// Entry point for the `cv init` command.
pub fn cmd(pathname: &str, first_branch: &str) -> Result<(), anyhow::Error> {
    println!("Creating repository {pathname}");
    Repository::create(pathname, first_branch)?;
    Ok(())
}
