use crate::{
    output::{OutputMessage, OutputService},
    repo::Repository,
};

/// Entry point for the `cv init` command.
pub fn cmd(
    pathname: &str,
    first_branch: &str,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    printer.println(&OutputMessage::plain(&format!(
        "Creating repository {pathname}"
    )));
    Repository::create(pathname, first_branch, printer)?;
    Ok(())
}
