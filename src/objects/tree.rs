use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context};

use crate::{
    index::IndexEntry,
    objects::{GitObject, ObjectKind, StoredObject},
    repo::Repository,
};

/// An individual entry in a repository tree object.
///
/// The object ID field points to either a tree object or blob object.
#[derive(Clone, Debug)]
pub struct TreeNode {
    /// The item's file mode
    pub mode: u32,

    ordering_name: String,

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
        let ordering_name = if mode != 0o40000 {
            path.to_string()
        } else {
            path.to_string() + "/"
        };
        let object_id = hex::encode(&data[(space_pos + null_pos + 2)..(space_pos + null_pos + 22)]);
        Ok(TreeNodeParsingResult {
            consumed: space_pos + null_pos + 22,
            node: TreeNode {
                mode,
                ordering_name,
                object_id,
            },
        })
    }

    /// Create a [`TreeNode`] from an [`IndexEntry`]
    pub fn from_index_entry(ixe: &IndexEntry) -> Self {
        Self {
            mode: ixe.mode(),
            ordering_name: ixe.object_file_name().to_string(),
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
            ordering_name: dir_name.to_string() + "/",
            object_id: object_id.to_string(),
        }
    }

    /// Get the filename or directory name of this node.
    pub fn name(&self) -> &str {
        if self.mode == 0o40000 {
            self.ordering_name
                .strip_suffix("/")
                .unwrap_or_else(|| &self.ordering_name)
        } else {
            &self.ordering_name
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
        self.ordering_name.cmp(&other.ordering_name)
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
            let path = path.as_ref().join(entry.name());
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
            buf.extend(entry.name().bytes());
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
