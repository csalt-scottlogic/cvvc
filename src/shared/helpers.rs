use anyhow::anyhow;
use std::{collections::HashMap, fmt::Display};

use chrono::{DateTime, TimeZone};

use crate::shared::{helpers::fs::index_path_parent, repo::Repository};

pub mod fs;

pub fn u32_from_be_bytes_unchecked(data: &[u8], start_idx: usize) -> u32 {
    u32::from_be_bytes(data[start_idx..(start_idx + 4)].try_into().unwrap())
}

pub fn u16_from_be_bytes_unchecked(data: &[u8], start_idx: usize) -> u16 {
    u16::from_be_bytes(data[start_idx..(start_idx + 2)].try_into().unwrap())
}

pub fn datetime_to_bytes<Z>(dt: &DateTime<Z>) -> impl Iterator<Item = u8>
where
    Z: TimeZone,
{
    (dt.timestamp() as u32)
        .to_be_bytes()
        .iter()
        .copied()
        .chain(dt.timestamp_subsec_nanos().to_be_bytes().iter().copied())
        .collect::<Vec<u8>>()
        .into_iter()
}

pub fn add_to_map_of_vecs<T>(map: &mut HashMap<String, Vec<T>>, k: &str, v: T) {
    if !map.contains_key(k) {
        map.insert(k.to_string(), Vec::<T>::new());
    }
    if let Some(arr) = map.get_mut(k) {
        arr.push(v);
    }
}

pub fn add_parent_dirs_to_map_of_vecs<T>(map: &mut HashMap<String, Vec<T>>, path: &str) {
    let mut shrunk_path = path;
    loop {
        if !map.contains_key(shrunk_path) {
            map.insert(shrunk_path.to_string(), Vec::new());
        }
        if shrunk_path.is_empty() {
            break;
        }
        shrunk_path = index_path_parent(shrunk_path);
    }
}

pub fn find_repo_cwd() -> Result<Repository, anyhow::Error> {
    let repo = Repository::find_cwd()?;
    match repo {
        Some(r) => Ok(r),
        None => Err(anyhow!("Not in a repository")),
    }
}

pub fn timestamped_name<Tz>(name: &str, timestamp: &DateTime<Tz>) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    format!("{} {}", name, timestamp.format("%s %z"))
}

pub fn shorten_message(prefix: &str, message: &str) -> String {
    let message_start = match message.lines().next() {
        Some(m) => m.trim(),
        None => ""
    };
    format!("{prefix}: {message_start}")
}
