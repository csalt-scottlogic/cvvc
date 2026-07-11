use std::collections::HashSet;

use crate::{
    config::{FetchRefMap, GlobalConfig, RemoteInfo},
    helpers::{abbrev_commit_id, find_repo_cwd},
    net::{HttpFetchClient, ProtocolVersion},
    output::{OutputMessage, OutputService},
    repo::Repository,
    stores::TargetedRef,
};

/// Entry point for `cv fetch`.  Fetches from all remotes.
pub fn fetch(config: &GlobalConfig, printer: &dyn OutputService) -> Result<(), anyhow::Error> {
    let mut repo = find_repo_cwd(printer)?;
    let remotes = repo.list_remote_names();
    for remote in remotes {
        if let Some(remote) = repo.get_remote(&remote) {
            fetch_remote(&mut repo, &remote, None, config, printer)?;
        }
    }
    Ok(())
}

fn fetch_remote(
    repo: &mut Repository,
    remote: &RemoteInfo,
    version: Option<u32>,
    config: &GlobalConfig,
    printer: &dyn OutputService,
) -> Result<(), anyhow::Error> {
    let mut warn_flag = false;
    for url in remote.fetch_urls.iter() {
        let protocol_version = version
            .map(ProtocolVersion::try_from)
            .map_or(Ok(None), |v| v.map(Some))?;
        let mut fetch_client_engine = HttpFetchClient::new(url, protocol_version)?;
        let remote_info = fetch_client_engine.fetch_refs_capabilities(printer)?;
        let deduped_rem_refs = remote_info.refs.iter().collect::<HashSet<&TargetedRef>>();
        let ref_maps: Vec<FetchRefMap> = deduped_rem_refs
            .into_iter()
            .flat_map(|rr| rr.map_fetch(&remote.fetch_defs).into_iter())
            .collect();
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
        if updates_needed.is_empty() {
            printer.println(&OutputMessage::plain("Nothing to update"));
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
        if objects_needed.is_empty() {
            printer.println(&OutputMessage::plain("No objects needed"));
        } else {
            let reader = fetch_client_engine.fetch_pack(
                &objects_needed
                    .iter()
                    .map(|x| x.as_str())
                    .collect::<HashSet<_>>(),
                repo,
                printer,
            )?;
            repo.store_pack(reader, printer)?;
        }
        let current_branch = repo
            .current_remote_tracking_branch()?
            .map(|b| b.into_ref_spec());

        for update in updates_needed {
            let new_target = update.source.target.to_string();
            if repo.has_object(&new_target)? {
                let existing_target = repo.resolve_ref(&update.dest)?.map(|r| r.to_string());
                let is_fast_forward = existing_target
                    .as_deref()
                    .map(|tid| repo.commit_is_pure_ancestor(&new_target, tid))
                    .map_or(Ok(None), |v| v.map(Some))?;

                let message = if let Some(ref et) = existing_target {
                    if is_fast_forward.unwrap_or(false) {
                        "fetch (fast forward)"
                    } else if update.force {
                        "fetch (forced)"
                    } else {
                        printer.println(&OutputMessage::plain(&format!(
                            "Skipping update {} => {} for {} (not fast-forwardable)",
                            &abbrev_commit_id(et),
                            &abbrev_commit_id(&new_target),
                            &update.dest
                        )));
                        continue;
                    }
                } else {
                    "fetch (new branch)"
                };

                repo.update_ref(&update.dest, &update.source.target)?;
                if Some(&update.dest) == current_branch.as_ref() {
                    warn_flag = true;
                }
                repo.write_ref_log(
                    existing_target.as_deref(),
                    &new_target,
                    &config.committer(),
                    message,
                    &update.dest,
                    false,
                )?;
                printer.println(&OutputMessage::plain(&format!(
                    "Updated {}: {} => {}",
                    &update.dest,
                    &abbrev_commit_id(&existing_target.unwrap_or_else(|| "(none)".to_string())),
                    &abbrev_commit_id(&new_target)
                )));
            }
        }
    }
    if warn_flag {
        printer.println(&OutputMessage::plain("Your current branch has been updated on its remote server.\nPull to bring these changes in locally."));
    }
    Ok(())
}
