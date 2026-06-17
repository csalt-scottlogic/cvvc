use crate::helpers::find_repo_cwd;

/// List the remote repositories the current repository is linked to.
pub fn list_remotes(verbose: bool) -> Result<(), anyhow::Error> {
    let repo = find_repo_cwd()?;
    let remotes = repo.list_remote_names();
    if !verbose {
        for remote in remotes {
            println!("{remote}");
        }
    } else {
        for remote in remotes {
            let remote_details = repo.get_remote(&remote);
            if let Some(remote_details) = remote_details {
                for fetch in remote_details.fetch_urls {
                    println!(
                        "{}",
                        remote_formatter(&remote_details.name, &fetch, "fetch")
                    );
                }
                for push in remote_details.push_urls {
                    println!("{}", remote_formatter(&remote_details.name, &push, "push"));
                }
            };
        }
    }
    Ok(())
}

fn remote_formatter(name: &str, url: &str, direction: &str) -> String {
    format!("{name}\t{url} ({direction})")
}
