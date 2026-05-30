use anyhow::{anyhow, Context};
use sha1::{Digest, Sha1};

use crate::objects::{Blob, Commit, GitObject, ObjectKind, StoredObject, Tag, Tree};

/// Metadata describing a [`RawObject`] or [`RawObjectData`].
#[derive(Debug, PartialEq)]
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
#[derive(Debug)]
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
    /// This method assumes that `self` is a delta object and that the `base_object` parameter
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

#[derive(Debug, PartialEq)]
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
#[derive(Debug)]
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

    /// Attempt to convert this raw object to a stored object by parsing its data
    ///
    /// This method will error if the object is a raw delta object.
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

impl TryFrom<RawObjectData> for RawObject {
    type Error = anyhow::Error;

    /// Create a [`RawObject`] from data loaded without an object ID.
    ///
    /// The object ID will be computed by hashing the data, prepending the appropriate header first.
    ///
    /// # Errors
    ///
    /// If the data consists of diff commands for a "named delta" object, this function will return an error,
    /// as the object ID cannot be computed.  The `data` should be combined with its base object first using
    /// the [`RawObjectData::combine()`] method.
    fn try_from(value: RawObjectData) -> Result<Self, Self::Error> {
        if matches!(value.metadata.kind, ObjectKind::Delta(_)) {
            return Err(anyhow!("cannot construct raw object from delta data"));
        }
        let mut headery_data = Self::construct_header(&value.metadata.kind, value.metadata.size);
        headery_data.append(&mut value.data.to_vec());
        let mut hasher = Sha1::new();
        hasher.update(&headery_data);
        let object_id = hex::encode(hasher.finalize());

        Ok(Self {
            content: value,
            object_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use crate::objects::{Blob, Commit, ObjectKind, StoredObject, Tag, Tree, TreeNode};

    use super::{DeltaCommand, DeltaCommandType, ObjectMetadata, RawObject, RawObjectData};

    #[test]
    fn object_metadata_new() {
        let kind = ObjectKind::Tag;
        let size = 42usize;

        let test_output = ObjectMetadata::new(kind, size);

        assert_eq!(ObjectKind::Tag, test_output.kind);
        assert_eq!(size, test_output.size);
    }

    #[test]
    fn object_metadata_combine_takes_base_kind() {
        let test_object = ObjectMetadata::new(ObjectKind::Tag, 42);
        let test_input = ObjectMetadata::new(ObjectKind::Blob, 4472);

        let test_output = test_object.combine(&test_input);

        assert_eq!(ObjectKind::Blob, test_output.kind);
    }

    #[test]
    fn object_metadata_combine_takes_own_size() {
        let test_object = ObjectMetadata::new(ObjectKind::Tag, 42);
        let test_input = ObjectMetadata::new(ObjectKind::Blob, 4472);

        let test_output = test_object.combine(&test_input);

        assert_eq!(42, test_output.size);
    }

    #[test]
    fn object_metadata_try_from_succeeds_with_valid_blob() {
        let test_input = [98u8, 108, 111, 98, 32, 53, 0, 67, 117, 110, 116, 115];

        let test_output = ObjectMetadata::try_from(&test_input[..]).unwrap();

        assert_eq!(ObjectKind::Blob, test_output.kind);
        assert_eq!(5, test_output.size);
    }

    #[test]
    fn object_metadata_try_from_succeeds_with_valid_commit_header() {
        let test_input = [
            99u8, 111, 109, 109, 105, 116, 32, 53, 0, 67, 117, 110, 116, 115,
        ];

        let test_output = ObjectMetadata::try_from(&test_input[..]).unwrap();

        assert_eq!(ObjectKind::Commit, test_output.kind);
        assert_eq!(5, test_output.size);
    }

    #[test]
    fn object_metadata_try_from_succeeds_with_valid_tree_header() {
        let test_input = [116u8, 114, 101, 101, 32, 53, 0, 67, 117, 110, 116, 115];

        let test_output = ObjectMetadata::try_from(&test_input[..]).unwrap();

        assert_eq!(ObjectKind::Tree, test_output.kind);
        assert_eq!(5, test_output.size);
    }

    #[test]
    fn object_metadata_try_from_succeeds_with_valid_tag_header() {
        let test_input = [116u8, 97, 103, 32, 53, 0, 67, 117, 110, 116, 115];

        let test_output = ObjectMetadata::try_from(&test_input[..]).unwrap();

        assert_eq!(ObjectKind::Tag, test_output.kind);
        assert_eq!(5, test_output.size);
    }

    #[test]
    fn object_metadata_try_from_fails_with_invalid_object_type() {
        let test_input = [98u8, 117, 109, 32, 53, 0, 67, 117, 110, 116, 115];

        ObjectMetadata::try_from(&test_input[..]).unwrap_err();
    }

    #[test]
    fn object_metadata_try_from_fails_without_separator_between_type_and_length() {
        let test_input = [98u8, 108, 111, 98, 53, 0, 67, 117, 110, 116, 115];

        ObjectMetadata::try_from(&test_input[..]).unwrap_err();
    }

    #[test]
    fn object_metadata_try_from_fails_without_separator_between_length_and_data() {
        let test_input = [98u8, 108, 111, 98, 32, 53, 67, 117, 110, 116, 115];

        ObjectMetadata::try_from(&test_input[..]).unwrap_err();
    }

    #[test]
    fn object_metadata_try_from_fails_with_less_data_than_declared() {
        let test_input = [98u8, 108, 111, 98, 32, 54, 0, 67, 117, 110, 116, 115];

        ObjectMetadata::try_from(&test_input[..]).unwrap_err();
    }

    #[test]
    fn object_metadata_try_from_fails_with_more_data_than_declared() {
        let test_input = [98u8, 108, 111, 98, 32, 56, 0, 67, 117, 110, 116, 115];

        ObjectMetadata::try_from(&test_input[..]).unwrap_err();
    }

    #[test]
    fn raw_object_data_new() {
        let test_data = b"Biscuits\n";
        let test_metadata = ObjectMetadata::new(ObjectKind::Blob, 9);

        let test_output = RawObjectData::new(test_data, test_metadata);

        assert_eq!(test_output.data, b"Biscuits\n");
        assert_eq!(ObjectKind::Blob, test_output.metadata.kind);
        assert_eq!(9, test_output.metadata.size);
    }

    #[test]
    fn raw_object_data_from_data_with_header_succeeds() {
        let test_data = [
            98u8, 108, 111, 98, 32, 56, 0, 66, 105, 115, 99, 117, 105, 116, 115,
        ];

        let test_output = RawObjectData::from_data_with_header(&test_data).unwrap();

        assert_eq!(test_output.data, b"Biscuits");
        assert_eq!(ObjectKind::Blob, test_output.metadata.kind);
        assert_eq!(8, test_output.metadata.size);
    }

    #[test]
    fn raw_object_data_from_data_with_header_fails_with_invalid_object_type() {
        let test_data = [
            98u8, 108, 105, 98, 32, 56, 0, 66, 105, 115, 99, 117, 105, 116, 115,
        ];

        RawObjectData::from_data_with_header(&test_data).unwrap_err();
    }

    #[test]
    fn raw_object_data_from_data_with_header_fails_without_separator_between_type_and_length() {
        let test_data = [
            98u8, 108, 111, 98, 56, 0, 66, 105, 115, 99, 117, 105, 116, 115,
        ];

        RawObjectData::from_data_with_header(&test_data).unwrap_err();
    }

    #[test]
    fn raw_object_data_from_data_with_header_fails_without_separator_between_length_and_data() {
        let test_data = [
            98u8, 108, 111, 98, 32, 56, 66, 105, 115, 99, 117, 105, 116, 115,
        ];

        RawObjectData::from_data_with_header(&test_data).unwrap_err();
    }

    #[test]
    fn raw_object_data_from_data_with_header_fails_with_less_data_than_declared() {
        let test_data = [
            98u8, 108, 111, 98, 32, 49, 56, 0, 66, 105, 115, 99, 117, 105, 116, 115,
        ];

        RawObjectData::from_data_with_header(&test_data).unwrap_err();
    }

    #[test]
    fn raw_object_data_from_data_with_header_fails_with_more_data_than_declared() {
        let test_data = [
            98u8, 108, 111, 98, 32, 55, 0, 66, 105, 115, 99, 117, 105, 116, 115,
        ];

        RawObjectData::from_data_with_header(&test_data).unwrap_err();
    }

    #[test]
    fn raw_object_data_metadata() {
        let test_data = b"Biscuits\n";
        let test_metadata = ObjectMetadata::new(ObjectKind::Blob, 9);
        let test_object = RawObjectData::new(test_data, test_metadata);

        let test_output = test_object.metadata();

        assert_eq!(&test_object.metadata, test_output);
    }

    #[test]
    fn raw_object_combine() {
        let base_object_data = [
            98u8, 108, 111, 98, 32, 56, 0, 66, 105, 115, 99, 117, 105, 116, 115,
        ];
        let base_object =
            RawObject::from_data_with_header(&base_object_data, "00000000000000000000").unwrap();
        let test_object_data = [8u8, 7, 0x91, 3, 4, 2, 98, 97, 0x91, 6, 2];
        let test_object = RawObjectData::new(
            &test_object_data,
            ObjectMetadata {
                kind: ObjectKind::Delta("00000000000000000000".to_string()),
                size: test_object_data.len(),
            },
        );

        let test_output = test_object.combine(&base_object);

        assert_eq!(test_output.data, b"cuitbats");
        assert_eq!(test_output.metadata.kind, ObjectKind::Blob);
        assert_eq!(test_output.metadata.size, test_object_data.len());
    }

    #[test]
    fn delta_command_from_byte_slice_decodes_add_command() {
        let test_input = [0x69u8];

        let test_output = DeltaCommand::from(&test_input[..]);

        assert_eq!(DeltaCommandType::Add(0x69), test_output.kind);
        assert_eq!(0x6a, test_output.len);
    }

    #[test]
    fn delta_command_from_byte_slice_decodes_special_case_copy_command() {
        let test_input = [0x80u8];

        let test_output = DeltaCommand::from(&test_input[..]);

        assert_eq!(
            DeltaCommandType::Copy {
                offset: 0,
                size: 0x10000
            },
            test_output.kind
        );
        assert_eq!(1, test_output.len);
    }

    #[test]
    fn delta_command_from_byte_slice_decodes_copy_command_with_offset() {
        let test_input = [0x86u8, 0x72, 0x8a];

        let test_output = DeltaCommand::from(&test_input[..]);

        assert_eq!(
            DeltaCommandType::Copy {
                offset: 0x8a7200,
                size: 0x10000
            },
            test_output.kind
        );
        assert_eq!(3, test_output.len);
    }

    #[test]
    fn delta_command_from_byte_slice_decodes_copy_command_with_offset_and_size() {
        let test_input = [0xf6u8, 0x72, 0x8a, 0x17, 0xff, 0xea];

        let test_output = DeltaCommand::from(&test_input[..]);

        assert_eq!(
            DeltaCommandType::Copy {
                offset: 0x8a7200,
                size: 0xeaff17
            },
            test_output.kind
        );
        assert_eq!(6, test_output.len);
    }

    #[test]
    fn combine_object_delta_data_add_only() {
        let test_base_data = b"Biscuits";
        let test_command_data = [8u8, 5, 5, 67, 97, 107, 101, 115];

        let test_output = super::combine_object_delta_data(test_base_data, &test_command_data);

        assert_eq!(test_output, b"Cakes");
    }

    #[test]
    fn combine_object_delta_data_copy_only() {
        let test_base_data = b"Biscuits";
        let test_command_data = [8u8, 8, 0x90, 8];

        let test_output = super::combine_object_delta_data(test_base_data, &test_command_data);

        assert_eq!(test_output, b"Biscuits");
    }

    #[test]
    fn combine_object_delta_data_mixed_ops() {
        let test_base_data = b"Biscuits";
        let test_command_data = [8u8, 7, 0x91, 3, 4, 2, 98, 97, 0x91, 6, 2];

        let test_output = super::combine_object_delta_data(test_base_data, &test_command_data);

        assert_eq!(test_output, b"cuitbats");
    }

    #[test]
    fn raw_object_from_data_with_header() {
        let test_data = [
            98u8, 108, 111, 98, 32, 56, 0, 66, 105, 115, 99, 117, 105, 116, 115,
        ];
        let test_id = "8216d33b7e88ed6bf2cb4eea14b3020f54325484";

        let test_output = RawObject::from_data_with_header(&test_data, test_id).unwrap();

        assert_eq!(test_output.content.data, b"Biscuits");
        assert_eq!(test_output.content.metadata.kind, ObjectKind::Blob);
        assert_eq!(test_output.content.metadata.size, 8);
        assert_eq!(test_output.object_id, test_id);
    }

    #[test]
    fn raw_object_from_headerless_data() {
        let test_data = [66u8, 105, 115, 99, 117, 105, 116, 115];
        let test_metadata = ObjectMetadata::new(ObjectKind::Blob, 8);
        let test_id = "8216d33b7e88ed6bf2cb4eea14b3020f54325484";

        let test_output = RawObject::from_headerless_data(&test_data, test_id, test_metadata);

        assert_eq!(test_output.content.data, b"Biscuits");
        assert_eq!(test_output.content.metadata.kind, ObjectKind::Blob);
        assert_eq!(test_output.content.metadata.size, 8);
        assert_eq!(test_output.object_id, test_id);
    }

    #[test]
    fn raw_object_from_raw_object_data() {
        let test_data = [66u8, 105, 115, 99, 117, 105, 116, 115];
        let test_metadata = ObjectMetadata::new(ObjectKind::Blob, 8);
        let test_id = "8216d33b7e88ed6bf2cb4eea14b3020f54325484";
        let test_input = RawObjectData::new(&test_data, test_metadata);

        let test_output = RawObject::try_from(test_input).unwrap();

        assert_eq!(test_output.content.data, b"Biscuits");
        assert_eq!(test_output.content.metadata.kind, ObjectKind::Blob);
        assert_eq!(test_output.content.metadata.size, 8);
        assert_eq!(test_output.object_id, test_id);
    }

    #[test]
    fn raw_object_from_raw_object_data_fails_on_delta_objects() {
        let test_data = [8u8, 7, 0x91, 3, 4, 2, 98, 97, 0x91, 6, 2];
        let test_metadata = ObjectMetadata::new(
            ObjectKind::Delta("8216d33b7e88ed6bf2cb4eea14b3020f54325484".to_string()),
            11,
        );
        let test_input = RawObjectData::new(&test_data, test_metadata);

        RawObject::try_from(test_input).unwrap_err();
    }

    #[test]
    fn raw_object_from_git_object_succeeds_for_blob() {
        let test_data = b"Biscuits".to_vec();
        let test_input = Blob::new_from_read(&mut test_data.as_slice()).unwrap();
        let expected_test_id = "8216d33b7e88ed6bf2cb4eea14b3020f54325484";

        let test_output = RawObject::from_git_object(&test_input);

        assert_eq!(test_output.content.data, b"Biscuits");
        assert_eq!(test_output.content.metadata.kind, ObjectKind::Blob);
        assert_eq!(test_output.content.metadata.size, 8);
        assert_eq!(test_output.object_id, expected_test_id);
    }

    #[test]
    fn raw_object_from_git_object_succeeds_for_commit() {
        let test_input = Commit::new(
            "88223311aaeeccff772288223311aaeeccff7722",
            None,
            "Caitlin <cait@example.com>",
            "Caitlin <cait@example.com>",
            &DateTime::parse_from_rfc3339("2026-05-18T21:13:02+01:00").unwrap(),
            "Commit message",
        );
        let expected_data = [
            116u8, 114, 101, 101, 32, 56, 56, 50, 50, 51, 51, 49, 49, 97, 97, 101, 101, 99, 99,
            102, 102, 55, 55, 50, 50, 56, 56, 50, 50, 51, 51, 49, 49, 97, 97, 101, 101, 99, 99,
            102, 102, 55, 55, 50, 50, 10, 97, 117, 116, 104, 111, 114, 32, 67, 97, 105, 116, 108,
            105, 110, 32, 60, 99, 97, 105, 116, 64, 101, 120, 97, 109, 112, 108, 101, 46, 99, 111,
            109, 62, 32, 49, 55, 55, 57, 49, 51, 53, 49, 56, 50, 32, 43, 48, 49, 48, 48, 10, 99,
            111, 109, 109, 105, 116, 116, 101, 114, 32, 67, 97, 105, 116, 108, 105, 110, 32, 60,
            99, 97, 105, 116, 64, 101, 120, 97, 109, 112, 108, 101, 46, 99, 111, 109, 62, 32, 49,
            55, 55, 57, 49, 51, 53, 49, 56, 50, 32, 43, 48, 49, 48, 48, 10, 10, 67, 111, 109, 109,
            105, 116, 32, 109, 101, 115, 115, 97, 103, 101, 10,
        ];
        let expected_id = "2977356f97c83af114be964f85721dc7271a7811";

        let test_output = RawObject::from_git_object(&test_input);

        assert_eq!(test_output.content.metadata.kind, ObjectKind::Commit);
        assert_eq!(test_output.content.data, expected_data);
        assert_eq!(test_output.content.metadata.size, 167);
        assert_eq!(test_output.object_id, expected_id);
    }

    #[test]
    fn raw_object_from_git_object_succeeds_for_tree() {
        let mut test_input = Tree::new();
        let mut test_input_entries = vec![TreeNode::from_subtree(
            "src",
            "88223311aaeeccff772288223311aaeeccff7722",
        )];
        test_input.add_entries(&mut test_input_entries);
        let expected_data = [
            52, 48, 48, 48, 48, 32, 115, 114, 99, 0, 136, 34, 51, 17, 170, 238, 204, 255, 119, 34,
            136, 34, 51, 17, 170, 238, 204, 255, 119, 34,
        ];
        let expected_id = "9d298423b3547d85ed7b9344ffad4e0c73eb1da2";

        let test_output = RawObject::from_git_object(&test_input);

        assert_eq!(test_output.content.metadata.kind, ObjectKind::Tree);
        assert_eq!(test_output.content.data, expected_data);
        assert_eq!(test_output.content.metadata.size, 30);
        assert_eq!(test_output.object_id, expected_id);
    }

    #[test]
    fn raw_object_from_git_object_succeeds_for_tag() {
        let test_input = Tag::new(
            "2977356f97c83af114be964f85721dc7271a7811",
            "test-tag",
            Some("Tag message"),
            "Caitlin <cait@example.com>",
            &DateTime::parse_from_rfc3339("2026-05-19T21:06:41+01:00").unwrap(),
        );
        let expected_data = [
            111, 98, 106, 101, 99, 116, 32, 50, 57, 55, 55, 51, 53, 54, 102, 57, 55, 99, 56, 51,
            97, 102, 49, 49, 52, 98, 101, 57, 54, 52, 102, 56, 53, 55, 50, 49, 100, 99, 55, 50, 55,
            49, 97, 55, 56, 49, 49, 10, 116, 121, 112, 101, 32, 99, 111, 109, 109, 105, 116, 10,
            116, 97, 103, 32, 116, 101, 115, 116, 45, 116, 97, 103, 10, 116, 97, 103, 103, 101,
            114, 32, 67, 97, 105, 116, 108, 105, 110, 32, 60, 99, 97, 105, 116, 64, 101, 120, 97,
            109, 112, 108, 101, 46, 99, 111, 109, 62, 32, 49, 55, 55, 57, 50, 50, 49, 50, 48, 49,
            32, 43, 48, 49, 48, 48, 10, 10, 84, 97, 103, 32, 109, 101, 115, 115, 97, 103, 101, 10,
        ];
        let expected_id = "887f3ca1f58a93dd3cc7b19c0c5da705d627d9e6";

        let test_output = RawObject::from_git_object(&test_input);

        assert_eq!(test_output.content.metadata.kind, ObjectKind::Tag);
        assert_eq!(test_output.content.data, expected_data);
        assert_eq!(test_output.content.metadata.size, 137);
        assert_eq!(test_output.object_id, expected_id);
    }

    #[test]
    fn raw_object_content_headerless() {
        let test_data = b"Biscuits";
        let test_object = RawObject {
            content: RawObjectData {
                data: test_data.to_vec(),
                metadata: ObjectMetadata {
                    kind: ObjectKind::Blob,
                    size: test_data.len(),
                },
            },
            object_id: "8216d33b7e88ed6bf2cb4eea14b3020f54325484".to_string(),
        };

        let test_output = test_object.content_headerless();

        assert_eq!(test_data, test_output);
    }

    #[test]
    fn raw_object_content_with_header() {
        let test_data = b"Biscuits";
        let test_object = RawObject {
            content: RawObjectData {
                data: test_data.to_vec(),
                metadata: ObjectMetadata {
                    kind: ObjectKind::Blob,
                    size: test_data.len(),
                },
            },
            object_id: "8216d33b7e88ed6bf2cb4eea14b3020f54325484".to_string(),
        };
        let expected_result = [
            98u8, 108, 111, 98, 32, 56, 0, 66, 105, 115, 99, 117, 105, 116, 115,
        ];

        let test_output = test_object.content_with_header();

        assert_eq!(test_output, expected_result);
    }

    #[test]
    fn raw_object_object_id() {
        let test_data = b"Biscuits";
        let test_object = RawObject {
            content: RawObjectData {
                data: test_data.to_vec(),
                metadata: ObjectMetadata {
                    kind: ObjectKind::Blob,
                    size: test_data.len(),
                },
            },
            object_id: "8216d33b7e88ed6bf2cb4eea14b3020f54325484".to_string(),
        };

        let test_output = test_object.object_id();

        assert_eq!("8216d33b7e88ed6bf2cb4eea14b3020f54325484", test_output);
    }

    #[test]
    fn raw_object_metadata() {
        let test_data = b"Biscuits";
        let test_object = RawObject {
            content: RawObjectData {
                data: test_data.to_vec(),
                metadata: ObjectMetadata {
                    kind: ObjectKind::Blob,
                    size: test_data.len(),
                },
            },
            object_id: "8216d33b7e88ed6bf2cb4eea14b3020f54325484".to_string(),
        };

        let test_output = test_object.metadata();

        assert_eq!(ObjectKind::Blob, test_output.kind);
        assert_eq!(8, test_output.size);
    }

    #[test]
    fn raw_object_to_stored_object_succeeds_for_valid_blob() {
        let test_object = RawObject::from_headerless_data(
            b"Biscuits",
            "8216d33b7e88ed6bf2cb4eea14b3020f54325484",
            ObjectMetadata {
                kind: ObjectKind::Blob,
                size: 8,
            },
        );

        let test_output = test_object.to_stored_object().unwrap();

        let StoredObject::Blob(test_output) = test_output else {
            panic!();
        };
        assert_eq!(test_output.data, b"Biscuits");
    }

    #[test]
    fn raw_object_to_stored_object_succeeds_for_valid_commit() {
        let test_object_data = [
            116u8, 114, 101, 101, 32, 56, 56, 50, 50, 51, 51, 49, 49, 97, 97, 101, 101, 99, 99,
            102, 102, 55, 55, 50, 50, 56, 56, 50, 50, 51, 51, 49, 49, 97, 97, 101, 101, 99, 99,
            102, 102, 55, 55, 50, 50, 10, 97, 117, 116, 104, 111, 114, 32, 67, 97, 105, 116, 108,
            105, 110, 32, 60, 99, 97, 105, 116, 64, 101, 120, 97, 109, 112, 108, 101, 46, 99, 111,
            109, 62, 32, 49, 55, 55, 57, 49, 51, 53, 49, 56, 50, 32, 43, 48, 49, 48, 48, 10, 99,
            111, 109, 109, 105, 116, 116, 101, 114, 32, 67, 97, 105, 116, 108, 105, 110, 32, 60,
            99, 97, 105, 116, 64, 101, 120, 97, 109, 112, 108, 101, 46, 99, 111, 109, 62, 32, 49,
            55, 55, 57, 49, 51, 53, 49, 56, 50, 32, 43, 48, 49, 48, 48, 10, 10, 67, 111, 109, 109,
            105, 116, 32, 109, 101, 115, 115, 97, 103, 101, 10,
        ];
        let test_object = RawObject::from_headerless_data(
            &test_object_data,
            "2977356f97c83af114be964f85721dc7271a7811",
            ObjectMetadata {
                kind: ObjectKind::Commit,
                size: 167,
            },
        );

        let test_output = test_object.to_stored_object().unwrap();

        let StoredObject::Commit(test_output) = test_output else {
            panic!();
        };
        assert_eq!(test_output.message, "Commit message\n");
    }

    #[test]
    fn raw_object_to_stored_object_succeeds_for_valid_tag() {
        let test_object_data = [
            111, 98, 106, 101, 99, 116, 32, 50, 57, 55, 55, 51, 53, 54, 102, 57, 55, 99, 56, 51,
            97, 102, 49, 49, 52, 98, 101, 57, 54, 52, 102, 56, 53, 55, 50, 49, 100, 99, 55, 50, 55,
            49, 97, 55, 56, 49, 49, 10, 116, 121, 112, 101, 32, 99, 111, 109, 109, 105, 116, 10,
            116, 97, 103, 32, 116, 101, 115, 116, 45, 116, 97, 103, 10, 116, 97, 103, 103, 101,
            114, 32, 67, 97, 105, 116, 108, 105, 110, 32, 60, 99, 97, 105, 116, 64, 101, 120, 97,
            109, 112, 108, 101, 46, 99, 111, 109, 62, 32, 49, 55, 55, 57, 50, 50, 49, 50, 48, 49,
            32, 43, 48, 49, 48, 48, 10, 10, 84, 97, 103, 32, 109, 101, 115, 115, 97, 103, 101, 10,
        ];
        let test_object = RawObject::from_headerless_data(
            &test_object_data,
            "887f3ca1f58a93dd3cc7b19c0c5da705d627d9e6",
            ObjectMetadata {
                kind: ObjectKind::Tag,
                size: 137,
            },
        );

        let test_output = test_object.to_stored_object().unwrap();

        let StoredObject::Tag(test_output) = test_output else {
            panic!();
        };
        assert_eq!("Tag message\n", test_output.message);
    }

    #[test]
    fn raw_object_to_stored_object_succeeds_for_valid_tree() {
        let test_object_data = [
            52, 48, 48, 48, 48, 32, 115, 114, 99, 0, 136, 34, 51, 17, 170, 238, 204, 255, 119, 34,
            136, 34, 51, 17, 170, 238, 204, 255, 119, 34,
        ];
        let test_object = RawObject::from_headerless_data(
            &test_object_data,
            "9d298423b3547d85ed7b9344ffad4e0c73eb1da2",
            ObjectMetadata {
                kind: ObjectKind::Tree,
                size: 30,
            },
        );

        let test_output = test_object.to_stored_object().unwrap();

        let StoredObject::Tree(test_output) = test_output else {
            panic!();
        };
        assert_eq!(test_output.entries().len(), 1);
        assert_eq!(test_output.entries().first().unwrap().mode, 0o40000);
        assert_eq!(test_output.entries().first().unwrap().name(), "src");
        assert_eq!(
            test_output.entries().first().unwrap().object_id,
            "88223311aaeeccff772288223311aaeeccff7722"
        );
    }

    #[test]
    fn raw_object_to_stored_object_fails_for_delta() {
        let test_object = RawObject::from_headerless_data(
            &[4, 86, 32, 129, 20],
            "01020304050607080910a1a2a3a4a5a6a7a8a9a0",
            ObjectMetadata {
                kind: ObjectKind::Delta("11121314151617181910c1c2c3c4c5c6c7c8c9c0".to_string()),
                size: 5,
            },
        );

        test_object.to_stored_object().unwrap_err();
    }
}
