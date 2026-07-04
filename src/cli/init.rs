use crate::{
    output::{OutputMessage, Printer},
    repo::Repository,
};

/// Entry point for the `cv init` command.
pub fn cmd(pathname: &str, first_branch: &str, println: &Printer) -> Result<(), anyhow::Error> {
    println(&OutputMessage::new(
        &format!("Creating repository {pathname}"),
        None,
    ));
    Repository::create(pathname, first_branch, println)?;
    Ok(())
}
