use anyhow::anyhow;
use std::{collections::HashMap, fmt::Display};

use chrono::{DateTime, TimeZone};

use crate::{helpers::fs::index_path_parent, repo::Repository};

pub mod fs;

/// Convert a sub-slice of a byte slice to a [`u32`]
///
/// This function takes four consecutive bytes from a `&[u8]` and converts them to [`u32`], using network byte order.
///
/// This function will panic if the start index given is outside the range of the slice, or is closer than four bytes
/// from the end of the slice.
pub fn u32_from_be_bytes_unchecked(data: &[u8], start_idx: usize) -> u32 {
    u32::from_be_bytes(data[start_idx..(start_idx + 4)].try_into().unwrap())
}

/// Convert a sub-slice of a byte slice to a [`u16`]
///
/// This function takes a pair of consecutive bytes from a `&[u8]` and converts them to [`u32`], using network byte order.
///
/// This function will panic if the start index given is outside the range of the slice, or points to the last byte
/// of the slice.
pub fn u16_from_be_bytes_unchecked(data: &[u8], start_idx: usize) -> u16 {
    u16::from_be_bytes(data[start_idx..(start_idx + 2)].try_into().unwrap())
}

/// Convert a [`DateTime`] to a byte sequence.
///
/// The iterator will return a sequence of 12 bytes encoding the timestamp value; the first 8 bytes are the number of
/// seconds since datum in network order, and the final 4 bytes are the number of nanoseconds since then, also
/// in network order.
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

/// Add a value to a map of vectors.
///
/// This function inserts a value into a hashmap which maps strings to vectors of the value type.
///
/// If the key is already present in the map, the value is appended to the appropriate vector.
///
/// If the key is not present in the map, a new vector containing only the given value is inserted.
pub fn add_to_map_of_vecs<T>(map: &mut HashMap<String, Vec<T>>, k: &str, v: T) {
    if !map.contains_key(k) {
        map.insert(k.to_string(), Vec::<T>::new());
    }
    if let Some(arr) = map.get_mut(k) {
        arr.push(v);
    }
}

/// Adds keys representing every directory in a Git-formatted path into a map of vectors.
///
/// This function expects to be passed a string which contains a relative path in Git format, with components
/// separated by the ASCII '/' character (charpoint 47).  If not already present, it creates an entry in the
/// map for each directory in the path.  Each entry consists of an empty vector.
///
/// #Examples
///
/// ```
/// use cvvc::helpers::add_parent_dirs_to_map_of_vecs;
/// 
/// let mut map = std::collections::HashMap::<String, Vec<u8>>::new();
/// add_parent_dirs_to_map_of_vecs(&mut map, "one/two/three");
/// assert!(map.contains_key("one/two/three"));
/// assert!(map.contains_key("one/two"));
/// assert!(map.contains_key("one"));
/// assert_eq!(map["one/two/three"], vec![]);
/// assert_eq!(map["one/two"], vec![]);
/// assert_eq!(map["one"], vec![]);
/// ```
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

/// Try to find a repository from the process's current working directory.
///
/// If the process's current working directory is inside a repository, a [`Repository`] object is created and
/// returned.
///
/// If the process's current working directory is not inside a repository, an error is returned.
pub fn find_repo_cwd() -> Result<Repository, anyhow::Error> {
    let repo = Repository::find_cwd()?;
    match repo {
        Some(r) => Ok(r),
        None => Err(anyhow!("Not in a repository")),
    }
}

/// Returns an owned string consisting of a string parameter and a timestamp.
///
/// The timestamp is formatted as the number of seconds since datum, followed by the timezone offset from UTC.
/// If the string is a name and email address, this is the format used in commit objects and in ref logs.
pub fn timestamped_name<Tz>(name: &str, timestamp: &DateTime<Tz>) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    format!("{} {}", name, timestamp.format("%s %z"))
}

/// Returns an owned string consisting of a prefix string, a colon, and the first line of a second "message" string.
///
/// This is used to create a ref log message from a commit.
pub fn shorten_and_prefix_message(prefix: &str, message: &str) -> String {
    let message_start = match message.lines().next() {
        Some(m) => m.trim(),
        None => "",
    };
    format!("{prefix}: {message_start}")
}
