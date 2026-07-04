use anyhow::{anyhow, Context};
use std::{collections::HashMap, fmt::Display, str::FromStr};

use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};

use crate::{helpers::fs::index_path_parent, output::Printer, repo::Repository};

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
/// The iterator will return a sequence of 8 bytes encoding the timestamp value; the first 4 bytes are the number of
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
/// # use cvvc::helpers::add_parent_dirs_to_map_of_vecs;
/// # use std::collections::HashMap;
/// let mut map = HashMap::<String, Vec<u8>>::new();
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
pub fn find_repo_cwd(println: &Printer) -> Result<Repository, anyhow::Error> {
    let repo = Repository::find_cwd(println)?;
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

/// Takes a string in the "user timestamp" format used in commits and tags, and extracts the timestamp.
///
/// The input string format is `Real Name <user@example.com> nnnnnnn +zzzz` where nnnnnnn is the number of seconds
/// since the Unix datum and +zzzz is the explicitly-signed timezone offset in hours and minutes.
///
/// # Errors
///
/// This function returns an error if it cannot find the start of the timestamp, or if the timestamp cannot be
/// correctly parsed.
pub fn timestamp_from_timestamped_name(
    timestamped_name: &str,
) -> Result<DateTime<FixedOffset>, anyhow::Error> {
    let input = timestamped_name.trim();
    let Some(idx) = input.rfind(" ") else {
        return Err(anyhow!("string contains no spaces"));
    };
    let Some(idx) = input[..idx].rfind(" ") else {
        return Err(anyhow!("string contains only one space"));
    };
    DateTime::parse_from_str(&input[idx..], " %s %z")
        .context("could not parse final part of string")
}

/// Returns an owned string consisting of a prefix string, a colon, and the first line of a second "message" string.
///
/// This is used to create a ref log message from a commit.
pub fn shorten_and_prefix_message(prefix: &str, message: &str) -> String {
    let message_start = message.lines().next().map(|x| x.trim()).unwrap_or("");
    format!("{prefix}: {message_start}")
}

/// Converts a `&str` to an owned string, appending a `\n` character if the string does not already end with one.
///
/// ```
/// # use cvvc::helpers::append_newline_if_necessary;
/// let str1 = "test";
/// let output = append_newline_if_necessary(str1);
/// assert_eq!("test\n", output);
///
/// let str2 = "test\n";
/// let output = append_newline_if_necessary(str2);
/// assert_eq!("test\n", output);
/// ```
pub fn append_newline_if_necessary(s: &str) -> String {
    if s.ends_with("\n") {
        String::from_str(s).unwrap()
    } else {
        String::from_str(s).unwrap() + "\n"
    }
}

/// Gets the current UTC date and time, as a [`DateTime<Utc>`].
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Gets the current local date and time, as a [`DateTime<Local>`]
pub fn now_here() -> DateTime<Local> {
    Local::now()
}

/// Determines whether a string is a legal branch or tag name according to Git rules.
pub fn is_ref_name_legal(name: &str) -> bool {
    let contains_patterns = ["/.", "..", " ", ":", "~", "^", "?", "*", "[", "\\"];
    let ends_with_patterns = [".lock", "/", "."];
    !(contains_patterns.iter().any(|p| name.contains(p))
        || ends_with_patterns.iter().any(|p| name.ends_with(p)))
}

/// Convert a byte slice into an ASCII string, displaying any non-ASCII characters as hex escape sequences.
///
/// For brevity, the escape sequences are shown in a more concise format than Rust literals; they consist of a
/// backslash followed by an unpadded hex number.  For example, in this representation, the `LF` character will
/// be displayed as `\a`, rather than the `\x0a` of a Rust literal.
pub fn escaped_byte_string(b: &[u8]) -> String {
    let it = b
        .iter()
        .flat_map(|v| {
            if *v >= 32 && *v < 127 {
                vec![*v]
            } else {
                format!("\\{:x}", v).bytes().collect::<Vec<u8>>()
            }
        })
        .collect();
    String::from_utf8(it).unwrap()
}

#[cfg(test)]
mod tests {

    use std::collections::HashMap;

    use chrono::DateTime;

    use super::{
        add_to_map_of_vecs, datetime_to_bytes, shorten_and_prefix_message, timestamped_name,
        u16_from_be_bytes_unchecked, u32_from_be_bytes_unchecked,
    };

    #[test]
    fn u32_from_be_bytes_unchecked_succeeds_if_at_start() {
        let test_data = [56, 72, 129, 24, 216, 87, 25, 1];
        let expected_result = 944275736;

        let test_output = u32_from_be_bytes_unchecked(&test_data, 0);

        assert_eq!(expected_result, test_output);
    }

    #[test]
    fn u32_from_be_bytes_unchecked_succeeds_if_at_middle() {
        let test_data = [56, 72, 129, 24, 216, 87, 25, 1];
        let expected_result = 2165889111;

        let test_output = u32_from_be_bytes_unchecked(&test_data, 2);

        assert_eq!(expected_result, test_output);
    }

    #[test]
    fn u32_from_be_bytes_unchecked_succeeds_if_at_end() {
        let test_data = [56, 72, 129, 24, 216, 87, 25, 1];
        let expected_result = 3629586689;

        let test_output = u32_from_be_bytes_unchecked(&test_data, 4);

        assert_eq!(expected_result, test_output);
    }

    #[test]
    fn u16_from_be_bytes_unchecked_succeeds_if_at_start() {
        let test_data = [56, 72, 129, 24, 216, 25, 1];
        let expected_result = 14408;

        let test_output = u16_from_be_bytes_unchecked(&test_data, 0);

        assert_eq!(expected_result, test_output);
    }

    #[test]
    fn u16_from_be_bytes_unchecked_succeeds_if_at_middle() {
        let test_data = [56, 72, 129, 24, 216, 25, 1];
        let expected_result = 55321;

        let test_output = u16_from_be_bytes_unchecked(&test_data, 4);

        assert_eq!(expected_result, test_output);
    }

    #[test]
    fn u16_from_be_bytes_unchecked_succeeds_if_at_end() {
        let test_data = [56, 72, 129, 24, 216, 25, 1];
        let expected_result = 6401;

        let test_output = u16_from_be_bytes_unchecked(&test_data, 5);

        assert_eq!(expected_result, test_output);
    }

    #[test]
    fn datetime_to_bytes_succeeds() {
        let test_data = DateTime::parse_from_rfc3339("2026-05-18T21:13:02.598+01:00").unwrap();
        let expected_result = [106, 11, 114, 206, 35, 164, 193, 128];

        let test_output = datetime_to_bytes(&test_data).collect::<Vec<u8>>();

        assert_eq!(test_output, expected_result);
    }

    #[test]
    fn add_to_map_of_vecs_succeeds_when_key_present() {
        let mut test_map = HashMap::<String, Vec<String>>::new();
        test_map.insert("test_key".to_string(), vec!["existing value".to_string()]);
        let new_data = "new data".to_string();

        add_to_map_of_vecs(&mut test_map, "test_key", new_data);

        let test_output = &test_map["test_key"];
        assert_eq!(*test_output, vec!["existing value", "new data"]);
    }

    #[test]
    fn add_to_map_of_vecs_succeeds_when_key_not_present() {
        let mut test_map = HashMap::<String, Vec<String>>::new();
        let test_data = "test data".to_string();

        add_to_map_of_vecs(&mut test_map, "data key", test_data);

        let test_output = &test_map["data key"];
        assert_eq!(*test_output, vec!["test data"]);
    }

    #[test]
    fn timestamped_name_succeeds() {
        let test_name = "Caitlin <cait@example.com>";
        let test_timestamp = DateTime::parse_from_rfc3339("2026-05-18T21:13:02.598+01:00").unwrap();

        let test_output = timestamped_name(test_name, &test_timestamp);

        assert_eq!("Caitlin <cait@example.com> 1779135182 +0100", test_output);
    }

    #[test]
    fn shorten_and_prefix_message_succeeds() {
        let test_message = "This is a message\nwith multiple\nlines in it\n\n";
        let test_prefix = "(flag)";

        let test_output = shorten_and_prefix_message(test_prefix, test_message);

        assert_eq!("(flag): This is a message", test_output);
    }
}
