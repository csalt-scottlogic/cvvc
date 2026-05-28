use anyhow::{anyhow, Context};
use chrono::{DateTime, TimeZone};
use indexmap::IndexMap;
use std::{
    cmp::Ordering,
    fmt::Display,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{
    helpers::{self, timestamped_name},
    index::IndexEntry,
    objects::errors::InvalidObjectIdError,
    repo::Repository,
};

/// Object-related error structs.
pub mod errors;

mod raw;
pub use raw::{combine_object_delta_data, ObjectMetadata, RawObject, RawObjectData};

/// The legal types of repository object.
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectKind {
    /// A non-delta blob object.
    Blob,

    /// A non-delta commit object.
    Commit,

    /// A non-delta tree object.
    Tree,

    /// A non-delta chunky (annotated) tag object.
    Tag,

    /// An unresolved delta based on a named object.
    Delta(String),
}

impl ObjectKind {
    /// Get a byte representation of an [`ObjectKind`] value.
    ///
    /// The byte representations are as used in the header of a loose object, and
    /// consist of the ASCII strings `blob`, `commit`, `tree` and `tag`.
    pub fn bytes(&self) -> &[u8] {
        match self {
            ObjectKind::Blob => b"blob",
            ObjectKind::Commit => b"commit",
            ObjectKind::Tag => b"tag",
            ObjectKind::Tree => b"tree",
            _ => b"",
        }
    }
}

impl TryFrom<&[u8]> for ObjectKind {
    type Error = anyhow::Error;

    /// Attempt to parse a byte sequence as an [`ObjectKind`] value.
    ///
    /// The byte sequence must match one of the values that can be
    /// output by [`ObjectKind::bytes`] otherwise an error will be returned.
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        match value {
            b"blob" => Ok(ObjectKind::Blob),
            b"commit" => Ok(ObjectKind::Commit),
            b"tree" => Ok(ObjectKind::Tree),
            b"tag" => Ok(ObjectKind::Tag),
            _ => Err(anyhow!("unrecognised object type")),
        }
    }
}

/// An enumeration that is similar to [`ObjectKind`], but also wraps the object itself.
#[derive(Debug)]
pub enum StoredObject {
    /// A blob object.
    Blob(Blob),

    /// A commit object.
    Commit(Commit),

    /// A tree object.
    Tree(Tree),

    /// A chunky (annotated) tag object.
    Tag(Tag),
}

impl StoredObject {
    /// Serialise the object stored in this enum.
    pub fn serialise(&self, buf: &mut Vec<u8>) {
        match self {
            StoredObject::Blob(x) => x.serialise(buf),
            StoredObject::Commit(x) => x.serialise(buf),
            StoredObject::Tree(x) => x.serialise(buf),
            StoredObject::Tag(x) => x.serialise(buf),
        }
    }
}

/// The trait which describes all repository objects, supporting binary serialisation and
/// deserialisation to and from byte sequences.
///
/// Implementations of this trait must be on [`Sized`] structs.
pub trait GitObject {
    /// Get the appropriate [`ObjectKind`] value for this repository object.
    fn kind(&self) -> ObjectKind;

    /// Convert this tag to a byte sequence
    fn serialise(&self, buf: &mut Vec<u8>);

    /// Parse a byte sequence into an in-memory repository object.
    fn deserialise(data: &[u8]) -> Result<Self, anyhow::Error>
    where
        Self: Sized;
}

/// In-memory representation of a repository blob object.
#[derive(Debug)]
pub struct Blob {
    data: Vec<u8>,
}

impl Blob {
    /// Load a blob from a reader.
    ///
    /// # Errors
    ///
    /// This function will return an error if the reader's [`Read::read_to_end`] implementation
    /// returns an error.
    pub fn new_from_read(source: &mut impl Read) -> Result<Self, anyhow::Error> {
        let mut buf: Vec<u8> = Vec::new();
        source
            .read_to_end(&mut buf)
            .context("Failed to read blob from source")?;
        Ok(Blob { data: buf })
    }

    /// Load a blob from a file.
    ///
    /// # Errors
    ///
    /// This function will return an error if it encounters any errors reading from the filesystem.
    pub fn new_from_path<P: AsRef<Path>>(source_path: P) -> Result<Self, anyhow::Error> {
        let mut file = std::fs::File::open(source_path).context("could not read file")?;
        Self::new_from_read(&mut file)
    }

    /// Get the content of the blob.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl GitObject for Blob {
    /// Get the [`ObjectKind`] value of this repository object.
    ///
    /// This method always returns [`ObjectKind::Blob`]
    fn kind(&self) -> ObjectKind {
        ObjectKind::Blob
    }

    /// Serialise this blob to a byte sequence.
    fn serialise(&self, buf: &mut Vec<u8>) {
        buf.clear();
        buf.extend_from_slice(&self.data);
    }

    /// Parse a byte sequence as a [`Blob`].
    ///
    /// This function is infallible.
    fn deserialise(data: &[u8]) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        Ok(Blob {
            data: data.to_vec(),
        })
    }
}

/// In-memory representation of a repository commit object.
#[derive(Debug)]
pub struct Commit {
    map: IndexMap<String, Vec<String>>,

    /// The commit's commit message.
    pub message: String,
}

impl Commit {
    /// Get the ID of the root tree object for this commit.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidObjectIdError`] if the commit does not have a tree object.
    pub fn tree(&self) -> Result<String, InvalidObjectIdError> {
        let target = self.map.get("tree");
        let Some(target) = target else {
            return Err(InvalidObjectIdError {});
        };
        let target = target.first();
        let Some(target) = target else {
            return Err(InvalidObjectIdError {});
        };
        Ok(target.to_string())
    }

    ///  Gets the parent(s) of this commit.
    ///
    /// Returns an empty vector if the commit has no parents.
    pub fn parents(&self) -> Vec<String> {
        if self.map.contains_key("parent") {
            self.map["parent"]
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<String>>()
        } else {
            vec![]
        }
    }

    /// Create a new commit with zero or one parents.
    pub fn new<Tz>(
        tree_id: &str,
        parent_id: Option<&str>,
        author: &str,
        committer: &str,
        timestamp: &DateTime<Tz>,
        message: &str,
    ) -> Self
    where
        Tz: TimeZone,
        Tz::Offset: Display,
    {
        let mut map = IndexMap::<String, Vec<String>>::new();
        map.insert(String::from("tree"), vec![String::from(tree_id)]);
        if let Some(parent_id) = parent_id {
            map.insert(String::from("parent"), vec![String::from(parent_id)]);
        }
        map.insert(
            String::from("author"),
            vec![timestamped_name(author, timestamp)],
        );
        map.insert(
            String::from("committer"),
            vec![timestamped_name(committer, timestamp)],
        );
        let message = message.trim().to_owned() + "\n";
        Commit { map, message }
    }
}

impl GitObject for Commit {
    /// Get an [`ObjectKind`] value for this repository object.
    ///
    /// This method always returns [`ObjectKind::Commit`]
    fn kind(&self) -> ObjectKind {
        ObjectKind::Commit
    }

    /// Serialise this blob to a byte sequence.
    fn serialise(&self, buf: &mut Vec<u8>) {
        kvlm_serialise(&self.map, &self.message, buf)
    }

    /// Parse a byte sequence as a [`Commit`].
    ///
    /// This function will return an error if the commit cannot be parsed.  This may occur
    /// if the commit data does not use Unix line endings (whatever the system), or if it
    /// contains text which cannot be cleanly converted to UTF-8.
    fn deserialise(data: &[u8]) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        let mut map = IndexMap::<String, Vec<String>>::new();
        let message = kvlm_parse(data, &mut map).context("Failed to parse commit")?;
        Ok(Commit { map, message })
    }
}

/// In-memory representation of a tag object, also known as a "chunky tag" or "annotated tag"
#[derive(Debug)]
pub struct Tag {
    map: IndexMap<String, Vec<String>>,

    /// The tag's tagging message.
    pub message: String,
}

impl Tag {
    /// Create a repository tag object, with a default tagging message.
    pub fn new<Tz>(
        target: &str,
        name: &str,
        message: Option<&str>,
        committer: &str,
        timestamp: &DateTime<Tz>,
    ) -> Self
    where
        Tz: TimeZone,
        Tz::Offset: Display,
    {
        let message = String::from(message.unwrap_or("CV: The user forgot to enter the message"));
        let mut map = IndexMap::<String, Vec<String>>::new();
        map.insert(String::from("object"), vec![target.to_string()]);
        map.insert(String::from("type"), vec![String::from("commit")]);
        map.insert(String::from("tag"), vec![String::from(name)]);
        map.insert(
            String::from("tagger"),
            vec![timestamped_name(committer, timestamp)],
        );
        Tag { map, message }
    }

    /// Get the target ID of this tag
    ///
    /// Returns an error if the tag object's "target" property is missing, but does not check if it
    /// is a valid object ID.
    pub fn target(&self) -> Result<String, InvalidObjectIdError> {
        let target = self.map.get("object");
        let Some(target) = target else {
            return Err(InvalidObjectIdError {});
        };
        let target = target.first();
        let Some(target) = target else {
            return Err(InvalidObjectIdError {});
        };
        Ok(target.to_string())
    }
}

impl GitObject for Tag {
    /// Get an [`ObjectKind`] value for this repository object.
    ///
    /// This method always returns [`ObjectKind::Tag`]
    fn kind(&self) -> ObjectKind {
        ObjectKind::Tag
    }

    /// Convert this tag to a byte sequence.
    fn serialise(&self, buf: &mut Vec<u8>) {
        kvlm_serialise(&self.map, &self.message, buf)
    }

    /// Parse a byte sequence into a [`Tag`] object.
    ///
    /// This function will return an error if the tag cannot be parsed.  This may occur
    /// if the tag data does not use Unix line endings (whatever the system), or if it
    /// contains text which cannot be cleanly converted to UTF-8.
    fn deserialise(data: &[u8]) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        let mut map = IndexMap::<String, Vec<String>>::new();
        let message = kvlm_parse(data, &mut map).context("Failed to parse tag")?;
        Ok(Tag { map, message })
    }
}

/// An individual entry in a repository tree object.
///
/// The object ID field points to either a tree object or blob object.
#[derive(Clone, Debug)]
pub struct TreeNode {
    /// The item's file mode
    pub mode: u32,

    /// The filename of the item
    pub name: String,

    /// The object ID of the item as stored in the repository.
    pub object_id: String,
}

struct TreeNodeParsingResult {
    consumed: usize,
    node: TreeNode,
}

impl TreeNode {
    fn from_bytes(data: &[u8]) -> Result<TreeNodeParsingResult, anyhow::Error> {
        let space_pos = data.iter().position(|x| *x == 0x20);
        let Some(space_pos) = space_pos else {
            return Err(anyhow!("Mode terminator character not found in tree entry"));
        };
        if space_pos != 5 && space_pos != 6 {
            return Err(anyhow!("Mode field of tree entry is incorrect length"));
        }
        let mode_str = str::from_utf8(&data[..space_pos])
            .context("Could not parse mode field of tree entry as valid UTF8")?;
        let mode = u32::from_str_radix(mode_str, 8)
            .context("Could not parse mode field of tree entry as valid octal integer")?;
        let null_pos = &data[(space_pos + 1)..].iter().position(|x| *x == 0);
        let Some(null_pos) = null_pos else {
            return Err(anyhow!("Path terminator character not found in tree entry"));
        };
        if space_pos + null_pos + 21 >= data.len() {
            return Err(anyhow!(
                "Tree entry is too short to contain valid object name"
            ));
        }
        let path = str::from_utf8(&data[(space_pos + 1)..(space_pos + null_pos + 1)])
            .context("Could not parse path field of tree entry as valid UTF8")?;
        let object_id = hex::encode(&data[(space_pos + null_pos + 2)..(space_pos + null_pos + 22)]);
        Ok(TreeNodeParsingResult {
            consumed: space_pos + null_pos + 22,
            node: TreeNode {
                mode,
                name: path.to_string(),
                object_id,
            },
        })
    }

    /// Create a [`TreeNode`] from an [`IndexEntry`]
    pub fn from_index_entry(ixe: &IndexEntry) -> Self {
        Self {
            mode: ixe.mode(),
            name: ixe.object_file_name().to_string(),
            object_id: ixe.object_id.to_string(),
        }
    }

    /// Create a [`TreeNode`] from a subtree.
    ///
    /// It is implied that the `object_id` parameter should be a valid
    /// object ID that points to another tree object, but this is not
    /// validated by the function.
    pub fn from_subtree(dir_name: &str, object_id: &str) -> Self {
        Self {
            mode: 0o40000,
            name: dir_name.to_string(),
            object_id: object_id.to_string(),
        }
    }

    fn ordering_path(&self) -> String {
        if self.mode >= 0o100000 {
            self.name.to_string()
        } else {
            self.name.to_string() + "/"
        }
    }
}

impl Ord for TreeNode {
    /// Returns an ordering between two [`TreeNode`] objects.
    ///
    /// If the objects point to files, they are ordered by [`TreeNode::name`].
    /// If they point to directories, they are ordered by [`TreeNode::name`] with
    /// a `/` character prepended.  This can change the ordering in cases where
    /// a directory name is a prefixed substring of a filename in the same parent
    /// directory.
    fn cmp(&self, other: &Self) -> Ordering {
        self.ordering_path().cmp(&other.ordering_path())
    }
}

impl PartialOrd for TreeNode {
    /// Returns an ordering between two [`TreeNode`] objects.  See the documentation
    /// for [`TreeNode::cmp`] for further information.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TreeNode {
    fn eq(&self, rhs: &Self) -> bool {
        matches!(self.cmp(rhs), Ordering::Equal)
    }
}

impl Eq for TreeNode {}

/// An in-memory representation of a repository tree object.
///
/// A tree object is the stored representation of a single directory in the worktree,
/// rather than representing an entire directory tree.
#[derive(Debug)]
pub struct Tree {
    entries: Vec<TreeNode>,
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

impl Tree {
    /// Create an empty tree
    pub fn new() -> Tree {
        Tree {
            entries: Vec::<TreeNode>::new(),
        }
    }

    /// Get a reference to the entries in this tree.
    pub fn entries(&self) -> &[TreeNode] {
        &self.entries
    }

    /// Add entries to this tree, moving ownership of them to the tree.
    ///
    /// The tree is re-sorted after insertion.
    pub fn add_entries(&mut self, entries: &mut Vec<TreeNode>) {
        self.entries.append(entries);
        self.entries.sort();
    }

    /// Read all of the contents of this tree and its subtrees from the repository, and copy
    /// them to a given directory in the filesystem.
    ///
    /// If successful, this method returns a vector of all of the objects which were written
    /// to the filesystem, both their full path and their ID.
    ///
    /// This method is called recursively to check out subtrees.
    ///
    /// This method is not atomic.  If this method returns an error, any changes it has already
    /// made to the filesystem will not be undone.  
    ///
    /// # Errors
    ///
    /// This function will error if an object cannot be found in the repository, or if it encounters
    /// any errors upon writing to the filesystem.
    ///
    /// This function will error if the tree contains a link to a submodule.  CVVC does not currently
    /// support submodules.
    pub fn checkout<P: AsRef<Path>>(
        &self,
        repo: &Repository,
        path: P,
    ) -> Result<Vec<(PathBuf, String)>, anyhow::Error> {
        let mut objects_checked_out = Vec::<(PathBuf, String)>::new();
        for entry in &self.entries {
            let obj = repo.read_object(&entry.object_id)?;
            let Some(obj) = obj else {
                return Err(anyhow!("Object {} not found", entry.object_id));
            };
            let path = path.as_ref().join(&entry.name);
            match obj {
                StoredObject::Tree(tree) => {
                    if !path.is_dir() {
                        fs::create_dir(&path)?;
                    }
                    let mut subdir_checked_out = tree.checkout(repo, &path)?;
                    objects_checked_out.append(&mut subdir_checked_out);
                }
                StoredObject::Blob(blob) => {
                    fs::write(&path, blob.data)?;
                    objects_checked_out.push((path, entry.object_id.to_string()));
                }
                StoredObject::Tag(_) => (),
                StoredObject::Commit(_) => {
                    return Err(anyhow!(
                        "Submodules, like object {}, are not currently supported.",
                        entry.object_id
                    ));
                }
            }
        }
        Ok(objects_checked_out)
    }
}

impl GitObject for Tree {
    /// Get an [`ObjectKind`] value for this repository object.
    ///
    /// This method always returns [`ObjectKind::Tree`]
    fn kind(&self) -> ObjectKind {
        ObjectKind::Tree
    }

    /// Convert this tree to a byte sequence.
    ///
    /// If any tree entries' object IDs are not valid, in the sense that they
    /// cannot be converted into a byte sequence in the expected way, the entry
    /// will be skipped.  This does not require the object IDs to represent
    /// extant, valid repository objects.
    fn serialise(&self, buf: &mut Vec<u8>) {
        for entry in self.entries() {
            let mode_str = format!("{:05o}", entry.mode);
            let Ok(hex_id) = hex::decode(&entry.object_id) else {
                continue;
            };
            buf.extend(mode_str.bytes());
            buf.push(0x20);
            buf.extend(entry.name.bytes());
            buf.push(0);
            buf.extend(hex_id);
        }
    }

    /// Parse a tree object from a byte sequence
    ///
    /// This function will return an error if any entries in the tree cannot be parsed.
    fn deserialise(data: &[u8]) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        let mut entries = Vec::<TreeNode>::new();
        let mut pos: usize = 0;
        let data_len = data.len();
        while pos < data_len {
            let node = TreeNode::from_bytes(&data[pos..])?;
            entries.push(node.node);
            pos += node.consumed;
        }

        let mut tree = Self::new();
        tree.add_entries(&mut entries);
        Ok(tree)
    }
}

fn kvlm_parse(
    raw_data: &[u8],
    map: &mut IndexMap<String, Vec<String>>,
) -> Result<String, anyhow::Error> {
    let space_index = raw_data.iter().position(|x| *x == 0x20);
    let nl_index = raw_data.iter().position(|x| *x == 0x0a);

    if space_index.is_none() || nl_index.unwrap_or(usize::MAX) < space_index.unwrap() {
        let message = String::from_utf8(raw_data[1..].to_vec())?;
        return Ok(message);
    }
    let space_index = space_index.unwrap();

    let key = str::from_utf8(&raw_data[0..space_index])?;
    let end = find_without(&raw_data[(space_index + 1)..], 0x0a, 0x20);
    let data_slice = str::from_utf8(match end {
        Some(x) => &raw_data[(space_index + 1)..(space_index + 1 + x)],
        None => &raw_data[(space_index + 1)..],
    })?
    .replace("\n ", "\n");

    if map.contains_key(key) {
        map[key].push(data_slice);
    } else {
        map.insert(key.to_string(), vec![data_slice]);
    }

    if let Some(end) = end {
        return kvlm_parse(&raw_data[(end + space_index + 2)..], map);
    }
    Ok(String::new())
}

fn kvlm_serialise(map: &IndexMap<String, Vec<String>>, message: &str, buf: &mut Vec<u8>) {
    buf.clear();
    for k in map.keys() {
        if k.is_empty() {
            continue;
        }
        let val = &map[k];
        for v in val.iter() {
            buf.append(k.as_bytes().to_vec().as_mut());
            buf.push(0x20);
            buf.append(
                v.replace("\n", "\n ")
                    .trim_end()
                    .as_bytes()
                    .to_vec()
                    .as_mut(),
            );
            buf.push(0x0a);
        }
    }
    buf.push(0x0a);
    buf.append(
        helpers::append_newline_if_necessary(message)
            .as_bytes()
            .to_vec()
            .as_mut(),
    );
}

// Find the first index in a slice of a particular value, where it's not followed immediately by another specific value.
fn find_without(data: &[u8], with: u8, without: u8) -> Option<usize> {
    let mut next_with = 0;
    loop {
        next_with += data[next_with..].iter().position(|x| *x == with)?;
        if data[next_with + 1] != without {
            break;
        }
        next_with += 1;
    }
    Some(next_with)
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use crate::objects::find_without;

    use super::kvlm_parse;

    #[test]
    fn find_without_succeeds() {
        let test_data = [
            45, 45, 45, 45, 45, 66, 69, 71, 73, 78, 32, 80, 71, 80, 32, 83, 73, 71, 78, 65, 84, 85,
            82, 69, 45, 45, 45, 45, 45, 10, 32, 10, 32, 119, 115, 70, 99, 66, 65, 65, 66, 67, 65,
            65, 81, 66, 81, 74, 113, 67, 88, 107, 68, 67, 82, 67, 49, 97, 81, 55, 117, 117, 53, 85,
            104, 108, 65, 65, 65, 79, 75, 115, 81, 65, 69, 83, 57, 43, 119, 53, 47, 89, 114, 72,
            101, 48, 109, 83, 89, 48, 101, 106, 111, 107, 66, 110, 99, 10, 32, 103, 65, 48, 50, 89,
            43, 122, 105, 74, 105, 118, 76, 56, 88, 77, 90, 82, 105, 102, 104, 43, 75, 56, 106, 57,
            55, 114, 67, 88, 119, 98, 87, 53, 85, 82, 73, 101, 47, 43, 104, 90, 115, 57, 48, 120,
            50, 98, 107, 121, 105, 67, 103, 109, 105, 79, 65, 90, 49, 82, 81, 82, 116, 110, 73, 10,
            32, 65, 74, 55, 69, 86, 79, 112, 111, 54, 48, 104, 78, 85, 119, 53, 90, 78, 111, 53,
            54, 78, 107, 108, 57, 122, 80, 113, 104, 112, 85, 69, 119, 97, 66, 85, 79, 67, 57, 78,
            98, 78, 81, 54, 51, 118, 50, 69, 66, 68, 119, 88, 107, 102, 48, 52, 86, 68, 78, 49,
            113, 105, 122, 84, 120, 10, 32, 47, 98, 122, 77, 56, 57, 66, 105, 72, 75, 116, 104, 86,
            76, 101, 105, 48, 56, 97, 82, 90, 104, 75, 115, 89, 111, 85, 80, 43, 69, 57, 111, 74,
            86, 113, 74, 75, 99, 90, 99, 103, 71, 115, 54, 118, 105, 89, 85, 53, 109, 76, 99, 109,
            72, 111, 69, 108, 74, 103, 110, 68, 83, 104, 84, 10, 32, 83, 66, 77, 79, 89, 53, 98,
            106, 101, 107, 109, 86, 101, 69, 84, 82, 82, 55, 89, 121, 53, 74, 77, 89, 86, 54, 89,
            122, 81, 55, 49, 67, 83, 75, 110, 85, 66, 51, 81, 121, 57, 106, 101, 67, 57, 98, 73,
            85, 119, 102, 72, 116, 43, 114, 68, 74, 85, 48, 55, 100, 87, 90, 55, 73, 10, 32, 113,
            65, 76, 88, 78, 108, 90, 85, 67, 72, 122, 100, 107, 99, 119, 118, 72, 105, 104, 77,
            122, 55, 113, 79, 82, 76, 48, 66, 57, 83, 97, 87, 88, 90, 81, 68, 112, 51, 103, 84, 50,
            55, 79, 70, 83, 101, 97, 65, 74, 68, 97, 121, 99, 70, 98, 90, 76, 53, 82, 47, 105, 72,
            48, 109, 10, 32, 107, 113, 104, 53, 53, 73, 120, 70, 97, 97, 119, 79, 53, 111, 101, 70,
            50, 79, 113, 77, 69, 109, 47, 86, 112, 54, 97, 101, 47, 110, 116, 86, 65, 47, 117, 99,
            90, 111, 97, 82, 49, 49, 81, 84, 121, 103, 89, 87, 100, 53, 65, 111, 117, 98, 87, 67,
            90, 51, 43, 99, 87, 75, 69, 81, 10, 32, 54, 104, 80, 100, 83, 85, 82, 55, 43, 105, 99,
            68, 66, 122, 56, 120, 106, 104, 120, 100, 82, 84, 113, 84, 101, 73, 66, 77, 78, 78, 68,
            76, 74, 48, 103, 116, 55, 107, 100, 76, 109, 49, 119, 53, 117, 100, 87, 99, 104, 86,
            99, 56, 75, 87, 75, 90, 112, 81, 85, 110, 111, 102, 88, 98, 10, 32, 57, 72, 66, 57,
            109, 81, 89, 53, 66, 73, 86, 78, 65, 99, 122, 116, 82, 55, 112, 116, 119, 56, 81, 80,
            98, 122, 106, 47, 67, 74, 87, 88, 86, 104, 48, 85, 102, 74, 100, 79, 117, 111, 71, 99,
            87, 120, 68, 119, 76, 81, 47, 90, 77, 86, 86, 83, 90, 84, 99, 81, 56, 101, 67, 67, 10,
            32, 85, 89, 117, 51, 82, 109, 85, 43, 111, 56, 121, 75, 82, 98, 83, 70, 112, 101, 49,
            82, 84, 115, 55, 101, 122, 86, 70, 43, 121, 108, 81, 97, 80, 107, 87, 79, 78, 100, 114,
            72, 117, 75, 82, 70, 77, 119, 43, 114, 99, 120, 43, 83, 97, 99, 56, 98, 90, 53, 86,
            118, 67, 104, 99, 74, 10, 32, 47, 106, 66, 103, 69, 88, 118, 107, 55, 99, 77, 104, 74,
            72, 111, 107, 56, 47, 47, 120, 53, 120, 114, 66, 112, 55, 68, 49, 53, 111, 118, 120,
            82, 57, 107, 109, 56, 56, 99, 122, 89, 115, 121, 50, 104, 122, 82, 117, 107, 69, 113,
            113, 76, 90, 79, 85, 55, 86, 114, 115, 111, 75, 43, 50, 10, 32, 110, 74, 57, 72, 120,
            72, 86, 85, 56, 108, 66, 97, 118, 122, 100, 85, 100, 69, 112, 79, 10, 32, 61, 43, 102,
            88, 53, 10, 32, 45, 45, 45, 45, 45, 69, 78, 68, 32, 80, 71, 80, 32, 83, 73, 71, 78, 65,
            84, 85, 82, 69, 45, 45, 45, 45, 45, 10, 32, 10, 10, 77, 101, 114, 103, 101, 32, 112,
            117, 108, 108, 32, 114, 101, 113, 117, 101, 115, 116, 32, 35, 57, 49, 32, 102, 114,
            111, 109, 32, 99, 97, 105, 116, 108, 105, 110, 115, 97, 108, 116, 47, 98, 117, 103,
            102, 105, 120, 47, 53, 51, 47, 99, 104, 101, 99, 107, 111, 117, 116, 45, 110, 101, 115,
            116, 101, 100, 45, 114, 101, 109, 111, 116, 101, 45, 98, 114, 97, 110, 99, 104, 10, 10,
            73, 115, 115, 117, 101, 32, 35, 53, 51, 32, 67, 104, 101, 99, 107, 32, 111, 117, 116,
            32, 114, 101, 109, 111, 116, 101, 32, 98, 114, 97, 110, 99, 104, 101, 115, 32, 112,
            114, 111, 112, 101, 114, 108, 121,
        ];
        let expected_result = 817;

        let test_output = find_without(&test_data, 10, 32);

        assert_eq!(Some(expected_result), test_output);
    }

    #[test]
    fn kvlm_parse_handles_gpgsig_properly() {
        let test_data = [
            116, 114, 101, 101, 32, 97, 48, 56, 49, 99, 51, 51, 51, 97, 48, 101, 49, 51, 102, 56,
            99, 101, 53, 101, 56, 54, 98, 51, 54, 53, 99, 53, 101, 97, 101, 97, 48, 54, 100, 48,
            50, 55, 49, 52, 101, 10, 112, 97, 114, 101, 110, 116, 32, 52, 49, 56, 55, 100, 98, 49,
            99, 56, 56, 53, 50, 55, 51, 56, 56, 48, 52, 51, 98, 98, 97, 52, 97, 97, 52, 99, 101,
            55, 52, 98, 97, 57, 102, 48, 101, 50, 52, 100, 52, 10, 112, 97, 114, 101, 110, 116, 32,
            57, 57, 98, 97, 50, 97, 55, 49, 55, 98, 102, 48, 101, 101, 50, 56, 101, 55, 54, 98,
            101, 51, 102, 50, 48, 97, 100, 48, 55, 53, 98, 51, 50, 100, 54, 56, 55, 55, 102, 97,
            10, 97, 117, 116, 104, 111, 114, 32, 67, 97, 105, 116, 108, 105, 110, 32, 83, 97, 108,
            116, 32, 60, 52, 56, 50, 50, 53, 56, 55, 53, 43, 99, 97, 105, 116, 108, 105, 110, 115,
            97, 108, 116, 64, 117, 115, 101, 114, 115, 46, 110, 111, 114, 101, 112, 108, 121, 46,
            103, 105, 116, 104, 117, 98, 46, 99, 111, 109, 62, 32, 49, 55, 55, 57, 48, 48, 53, 54,
            57, 57, 32, 43, 48, 49, 48, 48, 10, 99, 111, 109, 109, 105, 116, 116, 101, 114, 32, 71,
            105, 116, 72, 117, 98, 32, 60, 110, 111, 114, 101, 112, 108, 121, 64, 103, 105, 116,
            104, 117, 98, 46, 99, 111, 109, 62, 32, 49, 55, 55, 57, 48, 48, 53, 54, 57, 57, 32, 43,
            48, 49, 48, 48, 10, 103, 112, 103, 115, 105, 103, 32, 45, 45, 45, 45, 45, 66, 69, 71,
            73, 78, 32, 80, 71, 80, 32, 83, 73, 71, 78, 65, 84, 85, 82, 69, 45, 45, 45, 45, 45, 10,
            32, 10, 32, 119, 115, 70, 99, 66, 65, 65, 66, 67, 65, 65, 81, 66, 81, 74, 113, 67, 88,
            107, 68, 67, 82, 67, 49, 97, 81, 55, 117, 117, 53, 85, 104, 108, 65, 65, 65, 79, 75,
            115, 81, 65, 69, 83, 57, 43, 119, 53, 47, 89, 114, 72, 101, 48, 109, 83, 89, 48, 101,
            106, 111, 107, 66, 110, 99, 10, 32, 103, 65, 48, 50, 89, 43, 122, 105, 74, 105, 118,
            76, 56, 88, 77, 90, 82, 105, 102, 104, 43, 75, 56, 106, 57, 55, 114, 67, 88, 119, 98,
            87, 53, 85, 82, 73, 101, 47, 43, 104, 90, 115, 57, 48, 120, 50, 98, 107, 121, 105, 67,
            103, 109, 105, 79, 65, 90, 49, 82, 81, 82, 116, 110, 73, 10, 32, 65, 74, 55, 69, 86,
            79, 112, 111, 54, 48, 104, 78, 85, 119, 53, 90, 78, 111, 53, 54, 78, 107, 108, 57, 122,
            80, 113, 104, 112, 85, 69, 119, 97, 66, 85, 79, 67, 57, 78, 98, 78, 81, 54, 51, 118,
            50, 69, 66, 68, 119, 88, 107, 102, 48, 52, 86, 68, 78, 49, 113, 105, 122, 84, 120, 10,
            32, 47, 98, 122, 77, 56, 57, 66, 105, 72, 75, 116, 104, 86, 76, 101, 105, 48, 56, 97,
            82, 90, 104, 75, 115, 89, 111, 85, 80, 43, 69, 57, 111, 74, 86, 113, 74, 75, 99, 90,
            99, 103, 71, 115, 54, 118, 105, 89, 85, 53, 109, 76, 99, 109, 72, 111, 69, 108, 74,
            103, 110, 68, 83, 104, 84, 10, 32, 83, 66, 77, 79, 89, 53, 98, 106, 101, 107, 109, 86,
            101, 69, 84, 82, 82, 55, 89, 121, 53, 74, 77, 89, 86, 54, 89, 122, 81, 55, 49, 67, 83,
            75, 110, 85, 66, 51, 81, 121, 57, 106, 101, 67, 57, 98, 73, 85, 119, 102, 72, 116, 43,
            114, 68, 74, 85, 48, 55, 100, 87, 90, 55, 73, 10, 32, 113, 65, 76, 88, 78, 108, 90, 85,
            67, 72, 122, 100, 107, 99, 119, 118, 72, 105, 104, 77, 122, 55, 113, 79, 82, 76, 48,
            66, 57, 83, 97, 87, 88, 90, 81, 68, 112, 51, 103, 84, 50, 55, 79, 70, 83, 101, 97, 65,
            74, 68, 97, 121, 99, 70, 98, 90, 76, 53, 82, 47, 105, 72, 48, 109, 10, 32, 107, 113,
            104, 53, 53, 73, 120, 70, 97, 97, 119, 79, 53, 111, 101, 70, 50, 79, 113, 77, 69, 109,
            47, 86, 112, 54, 97, 101, 47, 110, 116, 86, 65, 47, 117, 99, 90, 111, 97, 82, 49, 49,
            81, 84, 121, 103, 89, 87, 100, 53, 65, 111, 117, 98, 87, 67, 90, 51, 43, 99, 87, 75,
            69, 81, 10, 32, 54, 104, 80, 100, 83, 85, 82, 55, 43, 105, 99, 68, 66, 122, 56, 120,
            106, 104, 120, 100, 82, 84, 113, 84, 101, 73, 66, 77, 78, 78, 68, 76, 74, 48, 103, 116,
            55, 107, 100, 76, 109, 49, 119, 53, 117, 100, 87, 99, 104, 86, 99, 56, 75, 87, 75, 90,
            112, 81, 85, 110, 111, 102, 88, 98, 10, 32, 57, 72, 66, 57, 109, 81, 89, 53, 66, 73,
            86, 78, 65, 99, 122, 116, 82, 55, 112, 116, 119, 56, 81, 80, 98, 122, 106, 47, 67, 74,
            87, 88, 86, 104, 48, 85, 102, 74, 100, 79, 117, 111, 71, 99, 87, 120, 68, 119, 76, 81,
            47, 90, 77, 86, 86, 83, 90, 84, 99, 81, 56, 101, 67, 67, 10, 32, 85, 89, 117, 51, 82,
            109, 85, 43, 111, 56, 121, 75, 82, 98, 83, 70, 112, 101, 49, 82, 84, 115, 55, 101, 122,
            86, 70, 43, 121, 108, 81, 97, 80, 107, 87, 79, 78, 100, 114, 72, 117, 75, 82, 70, 77,
            119, 43, 114, 99, 120, 43, 83, 97, 99, 56, 98, 90, 53, 86, 118, 67, 104, 99, 74, 10,
            32, 47, 106, 66, 103, 69, 88, 118, 107, 55, 99, 77, 104, 74, 72, 111, 107, 56, 47, 47,
            120, 53, 120, 114, 66, 112, 55, 68, 49, 53, 111, 118, 120, 82, 57, 107, 109, 56, 56,
            99, 122, 89, 115, 121, 50, 104, 122, 82, 117, 107, 69, 113, 113, 76, 90, 79, 85, 55,
            86, 114, 115, 111, 75, 43, 50, 10, 32, 110, 74, 57, 72, 120, 72, 86, 85, 56, 108, 66,
            97, 118, 122, 100, 85, 100, 69, 112, 79, 10, 32, 61, 43, 102, 88, 53, 10, 32, 45, 45,
            45, 45, 45, 69, 78, 68, 32, 80, 71, 80, 32, 83, 73, 71, 78, 65, 84, 85, 82, 69, 45, 45,
            45, 45, 45, 10, 32, 10, 10, 77, 101, 114, 103, 101, 32, 112, 117, 108, 108, 32, 114,
            101, 113, 117, 101, 115, 116, 32, 35, 57, 49, 32, 102, 114, 111, 109, 32, 99, 97, 105,
            116, 108, 105, 110, 115, 97, 108, 116, 47, 98, 117, 103, 102, 105, 120, 47, 53, 51, 47,
            99, 104, 101, 99, 107, 111, 117, 116, 45, 110, 101, 115, 116, 101, 100, 45, 114, 101,
            109, 111, 116, 101, 45, 98, 114, 97, 110, 99, 104, 10, 10, 73, 115, 115, 117, 101, 32,
            35, 53, 51, 32, 67, 104, 101, 99, 107, 32, 111, 117, 116, 32, 114, 101, 109, 111, 116,
            101, 32, 98, 114, 97, 110, 99, 104, 101, 115, 32, 112, 114, 111, 112, 101, 114, 108,
            121,
        ];
        let mut test_map = IndexMap::<String, Vec<String>>::new();
        let expected_result = "-----BEGIN PGP SIGNATURE-----

wsFcBAABCAAQBQJqCXkDCRC1aQ7uu5UhlAAAOKsQAES9+w5/YrHe0mSY0ejokBnc
gA02Y+ziJivL8XMZRifh+K8j97rCXwbW5URIe/+hZs90x2bkyiCgmiOAZ1RQRtnI
AJ7EVOpo60hNUw5ZNo56Nkl9zPqhpUEwaBUOC9NbNQ63v2EBDwXkf04VDN1qizTx
/bzM89BiHKthVLei08aRZhKsYoUP+E9oJVqJKcZcgGs6viYU5mLcmHoElJgnDShT
SBMOY5bjekmVeETRR7Yy5JMYV6YzQ71CSKnUB3Qy9jeC9bIUwfHt+rDJU07dWZ7I
qALXNlZUCHzdkcwvHihMz7qORL0B9SaWXZQDp3gT27OFSeaAJDaycFbZL5R/iH0m
kqh55IxFaawO5oeF2OqMEm/Vp6ae/ntVA/ucZoaR11QTygYWd5AoubWCZ3+cWKEQ
6hPdSUR7+icDBz8xjhxdRTqTeIBMNNDLJ0gt7kdLm1w5udWchVc8KWKZpQUnofXb
9HB9mQY5BIVNAcztR7ptw8QPbzj/CJWXVh0UfJdOuoGcWxDwLQ/ZMVVSZTcQ8eCC
UYu3RmU+o8yKRbSFpe1RTs7ezVF+ylQaPkWONdrHuKRFMw+rcx+Sac8bZ5VvChcJ
/jBgEXvk7cMhJHok8//x5xrBp7D15ovxR9km88czYsy2hzRukEqqLZOU7VrsoK+2
nJ9HxHVU8lBavzdUdEpO
=+fX5
-----END PGP SIGNATURE-----\n";

        kvlm_parse(&test_data, &mut test_map).unwrap();

        println!("{:?}", test_map);

        let test_output = &test_map["gpgsig"];
        assert_eq!(vec![expected_result], *test_output);
    }
}
