use crate::{config::RemoteInfo, helpers::find_repo_cwd, net::fetch_remote_refs};

/// Entry point for `cv fetch`.  Fetches from all remotes.
pub fn fetch() -> Result<(), anyhow::Error> {
    println!("Stop trying to make fetch happen!");
    let repo = find_repo_cwd()?;
    for remote in repo.list_remote_names() {
        if let Some(remote) = repo.get_remote(remote) {
            fetch_remote(&remote)?;
        }
    }
    Ok(())
}

fn fetch_remote(remote: &RemoteInfo) -> Result<(), anyhow::Error> {
    for url in remote.fetch_urls.iter() {
        println!("Fetching from {} ({})", remote.name, url);
        let remote_info = fetch_remote_refs(url)?;
        if !remote_info.capabilities.is_empty() {
            println!("Server capabilities:");
            for cap in remote_info.capabilities {
                println!("\t{cap}");
            }
        }
        println!("Refs:");
        for rem_ref in remote_info.refs {
            let mapped_refs = rem_ref.map_fetch(&remote.fetch_defs);
            if mapped_refs.is_empty() {
                println!("\t[{rem_ref} ignored]");
            } else {
                for mapped_ref in mapped_refs {
                    println!("\t{} maps to {}", rem_ref, mapped_ref.dest);
                }
            }
        }
    }
    Ok(())
}
