use crate::{
    helpers::find_repo_cwd,
    output::{OutputMessage, OutputService},
};

/// List the remote repositories the current repository is linked to.
pub fn list_remotes(verbose: bool, printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(printer)?;
    let remotes = repo.list_remote_names();
    if !verbose {
        for remote in remotes {
            printer.println(&OutputMessage::plain(&remote));
        }
    } else {
        for remote in remotes {
            let remote_details = repo.get_remote(&remote);
            if let Some(remote_details) = remote_details {
                for fetch in remote_details.fetch_urls {
                    printer.println(&OutputMessage::plain(&remote_formatter(
                        &remote_details.name,
                        &fetch,
                        "fetch",
                    )));
                }
                for push in remote_details.push_urls {
                    printer.println(&OutputMessage::plain(&remote_formatter(
                        &remote_details.name,
                        &push,
                        "push",
                    )));
                }
            };
        }
    }
    Ok(())
}

fn remote_formatter(name: &str, url: &str, direction: &str) -> String {
    format!("{name}\t{url} ({direction})")
}
