//! The looping sounds left playing, which a login launch resumes.

use crate::library::CollectionEntryId;

/// A looping entry started on the local device and not stopped since. Every row shares `paused`.
#[derive(Debug, toasty::Model)]
#[table = "saved_loops"]
pub struct SavedLoop {
    #[key]
    pub collection_entry_id: CollectionEntryId,
    pub paused: bool,
}
