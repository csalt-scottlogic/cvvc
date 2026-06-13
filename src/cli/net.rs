use crate::{
    config::{FetchRefMap, RemoteInfo},
    helpers::find_repo_cwd,
    net::{HttpFetchClient, ProtocolVersion},
    repo::Repository,
};

/// Entry point for `cv fetch`.  Fetches from all remotes.
pub fn fetch(version: Option<u32>) -> Result<(), anyhow::Error> {
    println!("Stop trying to make fetch happen!");
    let mut repo = find_repo_cwd()?;
    let remotes = repo.list_remote_names();
    for remote in remotes {
        if let Some(remote) = repo.get_remote(&remote) {
            fetch_remote(&mut repo, &remote, version)?;
        }
    }
    Ok(())
}

fn fetch_remote(
    repo: &mut Repository,
    remote: &RemoteInfo,
    version: Option<u32>,
) -> Result<(), anyhow::Error> {
    for url in remote.fetch_urls.iter() {
        let version = match version {
            Some(x) => Some(ProtocolVersion::try_from(x)?),
            None => None,
        };
        println!("Fetching from {} ({})", remote.name, url);
        let mut fetch_client_engine = HttpFetchClient::new(url, version)?;
        println!("Protocol version {}", fetch_client_engine.version());
        let remote_info = fetch_client_engine.fetch_refs_capabilities(true)?;
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
        let updates_needed = ref_maps
            .iter()
            .filter(|m| {
                if let Ok(Some(current_target)) = repo.resolve_ref(&m.dest) {
                    current_target != m.source.target
                } else {
                    true
                }
            })
            .collect::<Vec<&FetchRefMap>>();
        if !updates_needed.is_empty() {
            println!("Branches to update:");
            for update_spec in updates_needed.iter() {
                println!("\t{} to {}", update_spec.dest, update_spec.source.target);
            }
        } else {
            println!("Nothing to update");
            return Ok(());
        }
        let objects_needed: Vec<String> = updates_needed
            .iter()
            .filter_map(|m| {
                if repo
                    .has_object(&m.source.target.to_string())
                    .unwrap_or(false)
                {
                    None
                } else {
                    Some(m.source.target.to_string())
                }
            })
            .collect();
        if !objects_needed.is_empty() {
            println!("Commits needed:");
            for obj in objects_needed.iter() {
                println!("\t{}", obj);
            }
            let reader = fetch_client_engine.fetch_pack(&objects_needed.iter().map(|x| x.as_str()).collect::<Vec<_>>(), repo, true)?;
            // let mut pack_data = vec![];
            // let pack_size = reader.read_to_end(&mut pack_data)?;
            // println!("\npack size: {pack_size}");
            repo.store_pack(reader)?;
        }
    }
    Ok(())
}
