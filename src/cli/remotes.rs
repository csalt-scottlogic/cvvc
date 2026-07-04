use crate::{
    helpers::find_repo_cwd,
    output::{OutputMessage, Printer},
};

/// List the remote repositories the current repository is linked to.
pub fn list_remotes(verbose: bool, println: &Printer) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd(println)?;
    let remotes = repo.list_remote_names();
    if !verbose {
        for remote in remotes {
            println(&OutputMessage::new(&remote, None));
        }
    } else {
        for remote in remotes {
            let remote_details = repo.get_remote(&remote);
            if let Some(remote_details) = remote_details {
                for fetch in remote_details.fetch_urls {
                    println(&OutputMessage::new(
                        &remote_formatter(&remote_details.name, &fetch, "fetch"),
                        None,
                    ));
                }
                for push in remote_details.push_urls {
                    println(&OutputMessage::new(
                        &remote_formatter(&remote_details.name, &push, "push"),
                        None,
                    ));
                }
            };
        }
    }
    Ok(())
}

fn remote_formatter(name: &str, url: &str, direction: &str) -> String {
    format!("{name}\t{url} ({direction})")
}
