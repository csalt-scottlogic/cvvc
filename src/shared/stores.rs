use std::collections::HashMap;

use crate::shared::objects::{GitObject, RawObject};

pub mod file_store;

pub trait ObjectStore {
    fn _new_with_config(config: &HashMap<String, String>) -> Result<Self, anyhow::Error>
    where
        Self: Sized;
    fn create(&self) -> Result<(), anyhow::Error>;
    fn _is_writeable(&self) -> bool;
    fn search_objects(&self, partial_object_id: &str) -> Result<Vec<String>, anyhow::Error>;
    fn has_object(&self, object_id: &str) -> Result<bool, anyhow::Error>;
    fn read_object(&self, object_id: &str) -> Result<Option<RawObject>, anyhow::Error>;
    fn write_raw_object(&self, obj: &RawObject) -> Result<String, anyhow::Error>;

    fn write_object(&self, obj: &impl GitObject) -> Result<String, anyhow::Error> {
        self.write_raw_object(&RawObject::from_git_object(obj))
    }
}
