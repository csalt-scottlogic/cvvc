use anyhow::{anyhow, Context};
use chrono::{DateTime, TimeZone};
use indexmap::IndexMap;
use sha1::{Digest, Sha1};
use std::{cmp::Ordering, fmt::Display, fs, io::Read, path::Path};

use crate::{
    helpers::{self, timestamped_name},
    index::IndexEntry,
    objects::errors::InvalidObjectIdError,
    repo::Repository,
};

/// Object-related error structs.
pub mod errors;

/// Metadata describing a [`RawObject`] or [`RawObjectData`].
pub struct ObjectMetadata {
    /// The type of object.
    pub kind: ObjectKind,

    /// The length of the object's serialised data.
    pub size: usize,
}

impl ObjectMetadata {
    /// Create a new [`ObjectMetadata`] instance.
    pub fn new(kind: ObjectKind, size: usize) -> Self {
        Self { kind, size }
    }

    /// Create a new [`ObjectMetadata`] instance by combining this instance with another,
    /// consuming this instance.
    ///
    /// This method takes the [`ObjectMetadata::size`] from the current instance and the
    /// [`ObjectMetadata::kind`] from the base instance.
    pub fn combine(self, base: &ObjectMetadata) -> Self {
        Self::new(base.kind.clone(), self.size)
    }
}

impl TryFrom<&[u8]> for ObjectMetadata {
    type Error = anyhow::Error;

    /// Convert a sequence of bytes to an [`ObjectMetadata`] instance.  The
    /// sequence of bytes must contain the entire object that the metadata
    /// applies to, as well as the header which is decoded to produce the return object.
    ///
    /// The start of data is expected to be in the format used as the object header by
    /// the loose object store.  It consists of:
    /// - a 3-5 byte sequence containing the object type in ASCII
    /// - an ASCII space (charpoint 32)
    /// - the length of the object's data, as an ASCII string, in base 10.
    /// - a zero byte.
    ///
    /// The length of the object does not include this header.
    ///
    /// # Errors
    ///
    /// This function returns an error if:
    /// - the data does not contain a space character
    /// - the data does not contain a zero byte
    /// - the data before the first space character is not a valid object type tag
    ///   (one of `blob`, `commit`, `tree` or `tag`)
    /// - the data between the first space character and the first zero byte is not
    ///   a valid base-10 number when interpreted as ASCII or as UTF-8
    /// - the length field's value is greater than [`usize::MAX`]
    /// - the length of the remainder of the data, following the first zero byte,
    ///   does not match the value of the length field.
    fn try_from(data: &[u8]) -> Result<Self, Self::Error> {
        let type_end_index = data.iter().position(|&x| x == 0x20).ok_or(anyhow!(
            "malformed object: end of object type code not found"
        ))?;
        let len_start_index = type_end_index + 1;
        let len_end_index = data
            .iter()
            .skip(len_start_index)
            .position(|&x| x == 0)
            .ok_or(anyhow!("malformed object: end of object length not found"))?
            + len_start_index;
        let data_start_index = len_end_index + 1;
        let object_kind = ObjectKind::try_from(&data[..type_end_index])?;
        let object_len = std::str::from_utf8(&data[len_start_index..len_end_index])?
            .parse::<usize>()
            .context(format!(
                "Could not parse object length!  Object length string was {}",
                std::str::from_utf8(&data[len_start_index..len_end_index])?
            ))?;
        let actual_len = data.len() - data_start_index;
        if object_len != actual_len {
            return Err(anyhow!(
                "malformed object: expected length {object_len}, actual length {actual_len}"
            ));
        }
        Ok(Self {
            kind: object_kind,
            size: actual_len,
        })
    }
}

/// The data comprising an "unidentified" raw object, without its ID.
///
/// The data may consist of a "named delta".  In this case, the data is a series of diff commands
/// to be applied to the data of a base object, identified by its ID, and the [`RawObjectData::combine()`]
/// method may be used to reconstruct the complete data.
///
/// Otherwise, the data may be readily converted into a [`RawObject`] using [`RawObject::from_raw_object_data()`].
pub struct RawObjectData {
    data: Vec<u8>,
    metadata: ObjectMetadata,
}

impl RawObjectData {
    /// Create a new [`RawObjectData`] instance.  
    pub fn new(data: &[u8], metadata: ObjectMetadata) -> Self {
        Self {
            data: data.to_vec(),
            metadata,
        }
    }

    /// Create a new [`RawObjectData`] instance from data prefixed by an object header containing the metadata.
    ///
    /// This method is used to construct a [`RawObjectData`] instance from a decompressed loose object file.
    pub fn from_data_with_header(data: &[u8]) -> Result<Self, anyhow::Error> {
        let metadata =
            ObjectMetadata::try_from(data).with_context(|| "failed to load object data")?;
        let data_start_offset = data.len() - metadata.size;
        Ok(Self {
            data: data[data_start_offset..].to_vec(),
            metadata,
        })
    }

    /// Get the raw object metadata.
    pub fn metadata(&self) -> &ObjectMetadata {
        &self.metadata
    }

    /// Combine a delta object with a base object to reconstitute the former.
    ///
    /// This method assumes that [`self`] is a named delta object and that the `base_object` parameter
    /// is its base, but does not verify this.  The caller is responsible for ensuring that what they are doing
    /// makes sense, and doing otherwise is very likely to cause runtime errors due to attempted reads outside slice bounds.
    pub fn combine(self, base_object: &RawObject) -> Self {
        let combined_data = combine_object_delta_data(&base_object.content.data, &self.data);
        Self::new(
            &combined_data,
            self.metadata.combine(&base_object.content.metadata),
        )
    }
}

/// Combine an object's data with a sequence of diff commands, to produce the data of a second object.
///
/// This function does not verify that its input is sensible.  Because of this, if this function is called with
/// arbitrary data, it will likely produce a runtime panic due to the code attempting to read outside slice bounds.
pub fn combine_object_delta_data(base_data: &[u8], apply_commands: &[u8]) -> Vec<u8> {
    let mut result = Vec::<u8>::new();
    let mut idx = 0;

    // The commands start with two sizes, the size of the base and the size of the output.
    // We could read these for verification, if I was a Good Girl, but that can be a job for later.
    // Instead, we need to find the byte following the second byte less than 128 and start working from there.
    let mut non_continuation = 0;
    while non_continuation < 2 {
        if apply_commands[idx] < 0x80 {
            non_continuation += 1;
        }
        idx += 1;
    }

    while idx < apply_commands.len() {
        let command = DeltaCommand::from_bytes(&apply_commands[idx..]);
        match command.kind {
            DeltaCommandType::Add(sz) => {
                result.extend_from_slice(&apply_commands[(idx + 1)..(idx + 1 + sz)])
            }
            DeltaCommandType::Copy { offset, size } => {
                result.extend_from_slice(&base_data[offset..(offset + size)])
            }
        }
        idx += command.len;
    }
    result
}

enum DeltaCommandType {
    Copy { offset: usize, size: usize },
    Add(usize),
}

struct DeltaCommand {
    len: usize,
    kind: DeltaCommandType,
}

impl DeltaCommand {
    fn from_bytes(data: &[u8]) -> Self {
        if data[0] < 0x80 {
            let size = data[0] & 0x7f;
            Self {
                len: size as usize + 1,
                kind: DeltaCommandType::Add(size as usize),
            }
        } else {
            let bits = data[0] & 0x7f;
            if bits == 0 {
                Self {
                    len: 1,
                    kind: DeltaCommandType::Copy {
                        offset: 0,
                        size: 0x10000,
                    },
                }
            } else {
                let mut offset = 0usize;
                let mut size = 0usize;
                let mut bit = 1u8;
                let mut idx = 1;
                for i in 0..4 {
                    if bits & bit != 0 {
                        offset |= (data[idx] as usize) << (i * 8);
                        idx += 1;
                    }
                    bit <<= 1;
                }
                for i in 4..7 {
                    if bits & bit != 0 {
                        size |= (data[idx] as usize) << ((i - 4) * 8);
                        idx += 1;
                    }
                    bit <<= 1;
                }
                if size == 0 {
                    size = 0x10000;
                }
                Self {
                    len: bits.count_ones() as usize + 1,
                    kind: DeltaCommandType::Copy { offset, size },
                }
            }
        }
    }
}

/// A serialised object in memory, together with its ID and type.
///
/// This struct represents a partially-parsed object which has either just been loaded from
/// an object store, or is ready to be written to an object store.
///
/// Git stores object metadata differently for loose objects and packed objects.  However,
/// the object ID is derived from the object preceded by the loose object metadata format,
/// not as preceded by the packed object metadata format.   Moreover, when stored, a loose
/// object and its metadata are compressed as a whole, header-first; whereas for a packed
/// object only the object data proper is compressed.
///
/// To handle this cleanly in CVVC, the `RawObject` struct abstracts over both formats.  The
/// object data proper is referred to as "headerless content", and the (uncompressed) loose object
/// format, with header followed by object data, is referred to as "with-header content".   The
/// former can be accessed by the [`RawObject::content_headerless`] method; the latter by the
/// [`RawObject::content_with_header`] method.  The equivalent construction functions are
/// [`RawObject::from_headerless_data`], which requires a separate metadata parameter, and
/// [`RawObject::from_data_with_header`], which requires an object ID parameter but parses the
/// object metadata from the header.
pub struct RawObject {
    content: RawObjectData,
    object_id: String,
}

impl RawObject {
    /// Create a [`RawObject`] from with-header content data.
    ///
    /// This function will return an error if any of the reasons given in [`ObjectMetadata::try_from`]
    /// apply to the data.
    ///
    /// This function does not verify that the object ID is correct.
    pub fn from_data_with_header(data: &[u8], object_id: &str) -> Result<Self, anyhow::Error> {
        let metadata = ObjectMetadata::try_from(data)
            .with_context(|| format!("failed to load {}", object_id))?;
        let data_start_offset = data.len() - metadata.size;
        Ok(Self {
            content: RawObjectData {
                data: data[data_start_offset..].to_vec(),
                metadata,
            },
            object_id: object_id.to_string(),
        })
    }

    /// Create a [`RawObject`] from headerless content data and separate metadata, assuming the object ID is already known.
    ///
    /// This function does not verify the passed-in object ID.
    pub fn from_headerless_data(data: &[u8], object_id: &str, metadata: ObjectMetadata) -> Self {
        Self {
            content: RawObjectData {
                data: data.to_vec(),
                metadata,
            },
            object_id: object_id.to_string(),
        }
    }

    /// Create a [`RawObject`] from data loaded without an object ID.
    ///
    /// The object ID will be computed by hashing the data, prepending the appropriate header first.
    ///
    /// # Errors
    ///
    /// If the data consists of diff commands for a "named delta" object, this function will return an error,
    /// as the object ID cannot be computed.  The `data` should be combined with its base object first using
    /// the [`RawObjectData::combine()`] method.
    pub fn from_raw_object_data(data: RawObjectData) -> Result<Self, anyhow::Error> {
        if matches!(data.metadata.kind, ObjectKind::Delta(_)) {
            return Err(anyhow!("cannot construct raw object from delta data"));
        }
        let mut headery_data = Self::construct_header(&data.metadata.kind, data.metadata.size);
        headery_data.append(&mut data.data.to_vec());
        let mut hasher = Sha1::new();
        hasher.update(&headery_data);
        let object_id = hex::encode(hasher.finalize());

        Ok(Self {
            content: data,
            object_id,
        })
    }

    fn construct_header(kind: &ObjectKind, size: usize) -> Vec<u8> {
        let mut header = kind.bytes().to_vec();
        header.extend(b" ");
        header.extend(size.to_string().into_bytes());
        header.extend(b"\x00");
        header
    }

    /// Create a [`RawObject`] from an existing in-memory object.
    ///
    /// The object will be serialised, and its ID will be computed.
    /// The data in the [`RawObject`] is copied.
    pub fn from_git_object(obj: &impl GitObject) -> Self {
        let mut data = Vec::<u8>::new();
        obj.serialise(&mut data);
        let size = data.len();
        let mut content = Self::construct_header(&obj.kind(), size);
        content.extend(&data);

        let mut hasher = Sha1::new();
        hasher.update(&content);
        let object_id = hex::encode(hasher.finalize());
        Self {
            content: RawObjectData {
                data,
                metadata: ObjectMetadata {
                    kind: obj.kind(),
                    size,
                },
            },
            object_id,
        }
    }

    /// Get the headerless content of a [`RawObject`]
    pub fn content_headerless(&self) -> &[u8] {
        &self.content.data
    }

    /// Get the content of a [`RawObject`] with the metadata header prepended.
    ///
    /// The data returned is identical to the uncompressed content of a loose object file on disk.
    pub fn content_with_header(&self) -> Vec<u8> {
        let mut content =
            Self::construct_header(&self.content.metadata.kind, self.content.metadata.size);
        content.extend(self.content.data.iter());
        content
    }

    /// Get the object's ID.
    pub fn object_id(&self) -> &str {
        &self.object_id
    }

    /// Get the object's metadata.
    pub fn metadata(&self) -> &ObjectMetadata {
        &self.content.metadata
    }

    pub fn to_stored_object(&self) -> Result<StoredObject, anyhow::Error> {
        match self.metadata().kind {
            ObjectKind::Blob => Ok(StoredObject::Blob(Blob::deserialise(
                self.content_headerless(),
            )?)),
            ObjectKind::Commit => Ok(StoredObject::Commit(Commit::deserialise(
                self.content_headerless(),
            )?)),
            ObjectKind::Tree => Ok(StoredObject::Tree(Tree::deserialise(
                self.content_headerless(),
            )?)),
            ObjectKind::Tag => Ok(StoredObject::Tag(Tag::deserialise(
                self.content_headerless(),
            )?)),
            _ => Err(anyhow!("Delta objects cannot be parsed")),
        }
    }
}

/// The legal types of repository object.
#[derive(Clone, PartialEq)]
pub enum ObjectKind {
    Blob,
    Commit,
    Tree,
    Tag,
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
pub enum StoredObject {
    Blob(Blob),
    Commit(Commit),
    Tree(Tree),
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
pub struct Tag {
    map: IndexMap<String, Vec<String>>,

    /// The tag's tagging message.
    pub message: String,
}

impl Tag {
    /// Create a repository tag object, with a default tagging message.
    pub fn create<Tz>(
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
#[derive(Clone)]
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
    /// them to the filesystem.
    ///
    /// If successful, this method returns a vector of all of the object IDs which were written
    /// to the filesystem.
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
    pub fn checkout(&self, repo: &Repository, path: &Path) -> Result<Vec<String>, anyhow::Error> {
        let mut objects_checked_out = Vec::<String>::new();
        for entry in &self.entries {
            let obj = repo.read_object(&entry.object_id)?;
            let Some(obj) = obj else {
                return Err(anyhow!("Object {} not found", entry.object_id));
            };
            let path = path.join(&entry.name);
            match obj {
                StoredObject::Tree(tree) => {
                    fs::create_dir(&path)?;
                    let mut subdir_checked_out = tree.checkout(repo, &path)?;
                    objects_checked_out.append(&mut subdir_checked_out);
                }
                StoredObject::Blob(blob) => {
                    fs::write(path, blob.data)?;
                    objects_checked_out.push(entry.object_id.to_string());
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
        next_with = data[next_with..].iter().position(|x| *x == with)?;
        if data[next_with + 1] != without {
            break;
        }
    }
    Some(next_with)
}
