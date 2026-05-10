use anyhow::{anyhow, Context};
use sha1::{Digest, Sha1};

use crate::objects::{Blob, Commit, GitObject, ObjectKind, StoredObject, Tag, Tree};

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
        let command = DeltaCommand::from(&apply_commands[idx..]);
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

impl From<&[u8]> for DeltaCommand {
    fn from(data: &[u8]) -> Self {
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
