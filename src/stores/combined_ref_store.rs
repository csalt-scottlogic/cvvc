use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use crate::stores::{
    packed_ref_store::PackedRefStore, ref_file_store::RefFileStore, BranchSpec, RefSpec, RefStore,
    RefTarget, TargetedRef,
};

/// A [`RefStore`] implementation which provides a facade over both a loose ref store and, optionally, a packed ref store.
///
/// Create and update operations write to the loose ref store.  Read operations read from both, but the content of the
/// loose ref store takes priority if a ref is present in both stores but has different targets.
pub struct CombinedRefStore {
    loose_store: RefFileStore,
    packed_store: Option<PackedRefStore>,
}

impl CombinedRefStore {
    /// Create a new combined ref store, from a loose ref store path, and an optional packed ref file path.
    pub fn new<P: AsRef<Path>, Q: AsRef<Path>>(
        loose_store_path: P,
        packed_store_path: Option<Q>,
    ) -> Result<Self, anyhow::Error> {
        let packed_store = match packed_store_path {
            Some(p) => Some(PackedRefStore::new_from_file(p)?),
            None => None,
        };
        Ok(Self {
            loose_store: RefFileStore::new(loose_store_path),
            packed_store,
        })
    }
}

impl RefStore for CombinedRefStore {
    fn create(&self) -> Result<(), anyhow::Error> {
        self.loose_store.create()
    }

    fn is_existing_ref(&self, r: &RefSpec) -> Result<bool, anyhow::Error> {
        if self.loose_store.is_existing_ref(r)? {
            return Ok(true);
        }
        if let Some(packed_store) = &self.packed_store {
            packed_store.is_existing_ref(r)
        } else {
            Ok(false)
        }
    }

    fn local_branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error> {
        let mut results = HashSet::<BranchSpec>::new();
        for b in self.loose_store.local_branches()? {
            results.insert(b);
        }
        if let Some(packed_store) = &self.packed_store {
            for b in packed_store.local_branches()? {
                results.insert(b);
            }
        }
        Ok(results.into_iter().collect())
    }

    fn tags(&self) -> Result<Vec<RefSpec>, anyhow::Error> {
        let mut results = HashSet::<RefSpec>::new();
        for t in self.loose_store.tags()? {
            results.insert(t);
        }
        if let Some(packed_store) = &self.packed_store {
            for t in packed_store.tags()? {
                results.insert(t);
            }
        }
        Ok(results.into_iter().collect())
    }

    fn all_refs(&self) -> Result<Vec<RefSpec>, anyhow::Error> {
        let mut results = HashSet::<RefSpec>::new();
        for r in self.loose_store.all_refs()? {
            results.insert(r);
        }
        if let Some(packed_store) = &self.packed_store {
            for r in packed_store.all_refs()? {
                results.insert(r);
            }
        }
        Ok(results.into_iter().collect())
    }

    fn all_ref_targets(&self) -> Result<Vec<TargetedRef>, anyhow::Error> {
        let mut results = HashMap::<RefSpec, RefTarget>::new();
        for r in self.loose_store.all_ref_targets()? {
            results.insert(r.spec, r.target);
        }
        if let Some(packed_store) = &self.packed_store {
            for r in packed_store.all_ref_targets()? {
                results.insert(r.spec, r.target);
            }
        }
        Ok(results
            .into_iter()
            .map(|(k, v)| TargetedRef { target: v, spec: k })
            .collect())
    }

    fn resolve_target(&self, r: &super::RefSpec) -> Result<Option<String>, anyhow::Error> {
        let result = self.loose_store.resolve_target(r)?;
        if result.is_some() {
            return Ok(result);
        }
        if let Some(packed_store) = &self.packed_store {
            packed_store.resolve_target(r)
        } else {
            Ok(None)
        }
    }

    fn search_remotes_for_branch(&self, name: &str) -> Result<Vec<BranchSpec>, anyhow::Error> {
        let mut results = HashSet::<BranchSpec>::new();
        for b in self.loose_store.search_remotes_for_branch(name)? {
            results.insert(b);
        }
        if let Some(packed_store) = &self.packed_store {
            for b in packed_store.search_remotes_for_branch(name)? {
                results.insert(b);
            }
        }
        Ok(results.into_iter().collect())
    }

    fn create_ref(&self, r: &RefSpec, object_id: &str) -> Result<(), anyhow::Error> {
        self.loose_store.create_ref(r, object_id)
    }

    fn update_branch(&self, branch: &BranchSpec, commit_id: &str) -> Result<(), anyhow::Error> {
        self.loose_store.update_branch(branch, commit_id)
    }
}
