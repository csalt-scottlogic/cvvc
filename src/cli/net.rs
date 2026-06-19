use std::collections::HashSet;

use chrono::Local;

use crate::{
    config::{FetchRefMap, GlobalConfig, RemoteInfo},
    helpers::find_repo_cwd,
    net::{HttpFetchClient, ProtocolVersion},
    repo::Repository,
};

/// Entry point for `cv fetch`.  Fetches from all remotes.
pub fn fetch(version: Option<u32>, config: &GlobalConfig) -> Result<(), anyhow::Error> {
    println!("Stop trying to make fetch happen!");
    let mut repo = find_repo_cwd()?;
    let remotes = repo.list_remote_names();
    for remote in remotes {
        if let Some(remote) = repo.get_remote(&remote) {
            fetch_remote(&mut repo, &remote, version, config)?;
        }
    }
    Ok(())
}

fn fetch_remote(
    repo: &mut Repository,
    remote: &RemoteInfo,
    version: Option<u32>,
    config: &GlobalConfig,
) -> Result<(), anyhow::Error> {
    for url in remote.fetch_urls.iter() {
        let version = match version {
            Some(x) => Some(ProtocolVersion::try_from(x)?),
            None => None,
        };
        println!("Fetching from {} ({})", remote.name, url);
        let mut fetch_client_engine = HttpFetchClient::new(url, version)?;
        let start_version = fetch_client_engine.version();
        println!("Protocol version {}", start_version);
        let remote_info = fetch_client_engine.fetch_refs_capabilities(true)?;
        let remote_capabilities = fetch_client_engine.capabilities();
        if !remote_capabilities.is_empty() {
            println!("Server capabilities:");
            for cap in remote_capabilities {
                println!("\t{cap}");
            }
        }
        let sniffed_version = fetch_client_engine.version();
        if start_version != sniffed_version {
            println!("Protocol downgraded to {}", sniffed_version);
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
        let objects_needed: HashSet<String> = updates_needed
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
            let reader = fetch_client_engine.fetch_pack(
                &objects_needed
                    .iter()
                    .map(|x| x.as_str())
                    .collect::<HashSet<_>>(),
                repo,
                true,
            )?;
            repo.store_pack(reader)?;
            for update in updates_needed {
                if repo.has_object(&update.source.target.to_string())? {
                    let existing_target = repo.resolve_ref(&update.dest)?.map(|r| r.to_string());
                    repo.update_ref(&update.dest, &update.source.target)?;
                    repo.write_ref_log(
                        existing_target.as_deref(),
                        &update.source.target.to_string(),
                        &config.committer(),
                        &Local::now(),
                        "fetch",
                        &update.dest,
                        false,
                    )?;
                }
            }
        }
    }
    Ok(())
}
