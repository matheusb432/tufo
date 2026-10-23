//! The looping sounds left playing, which a login launch resumes.

use std::collections::BTreeSet;

use crate::library::CollectionEntryId;

/// The latest local effects session, retained while the player stands by.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SoundResumeState {
    pub entry_ids: BTreeSet<CollectionEntryId>,
    pub paused: bool,
}

/// One loop in the latest local effects session. Every row shares `paused`.
#[derive(Debug, toasty::Model)]
#[table = "saved_loops"]
pub struct SavedLoop {
    #[key]
    pub collection_entry_id: CollectionEntryId,
    pub paused: bool,
}
