use crate::{config::{FetchRefMap, RemoteInfo}, helpers::find_repo_cwd, net::fetch_remote_refs, repo::Repository};

/// Entry point for `cv fetch`.  Fetches from all remotes.
pub fn fetch() -> Result<(), anyhow::Error> {
    println!("Stop trying to make fetch happen!");
    let repo = find_repo_cwd()?;
    for remote in repo.list_remote_names() {
        if let Some(remote) = repo.get_remote(remote) {
            fetch_remote(&repo, &remote)?;
        }
    }
    Ok(())
}

fn fetch_remote(repo: &Repository, remote: &RemoteInfo) -> Result<(), anyhow::Error> {
    for url in remote.fetch_urls.iter() {
        println!("Fetching from {} ({})", remote.name, url);
        let remote_info = fetch_remote_refs(url, true)?;
        if !remote_info.capabilities.is_empty() {
            println!("Server capabilities:");
            for cap in remote_info.capabilities {
                println!("\t{cap}");
            }
        }
        let mut ref_maps: Vec<FetchRefMap> = vec![];
        println!("Refs:");
        for rem_ref in remote_info.refs.iter() {
            let mut mapped_refs = rem_ref.map_fetch(&remote.fetch_defs);
            ref_maps.append(&mut mapped_refs);
        }
        let updates_needed = ref_maps.iter().filter(|m| {
            if let Ok(Some(current_target)) = repo.resolve_ref(&m.dest) {
                current_target != m.source.target_id
            } else {
                true
            }
        }).collect::<Vec<&FetchRefMap>>();
        if !updates_needed.is_empty() {
            println!("Branches to update:");
            for update_spec in updates_needed.iter() {
                println!("\t{} to {}", update_spec.dest, update_spec.source.target_id);
            }
        } else {
            println!("Nothing to update");
            return Ok(());
        }
        let objects_needed: Vec<String> = updates_needed.iter().filter_map(|m| {
            if repo.has_object(&m.source.target_id).unwrap_or(false) {
                None
            } else {
                Some(m.source.target_id.to_string())
            }
        }).collect();
        if !objects_needed.is_empty() {
            println!("Comits needed:");
            for obj in objects_needed.iter() {
                println!("\t{}", obj);
            }
        }
    }
    Ok(())
}
