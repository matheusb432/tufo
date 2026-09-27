//! Matching stored files to the files on disk, then settling each match into a change.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use tufo_models::{
    library::{AudioFileId, FileStamp},
    playback::AudioFilePath,
};

use super::{
    FileChange, FileRef, KnownFile, NewFile, ReconcileChanges, ReconcileInput, ScanOutcome,
    authoritative_roots, is_under_any, titles::audio_file_title, unique_by,
};
use crate::library::library_files::{AudioFacts, ScannedFile};

/// Stored files matched to what's on disk, before their audio is read.
#[derive(Debug)]
pub(super) struct MatchedFiles {
    scanned: BTreeMap<PathBuf, ScannedEntry>,
    found: HashMap<AudioFileId, FileMatch>,
    /// Stored files nothing matched that are authoritatively absent from their path.
    absent: Vec<AudioFileId>,
    /// Scanned paths no stored file claimed, outside hidden folders.
    candidates: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct ScannedEntry {
    file: ScannedFile,
    /// Whether every folder this file was scanned in belongs to a hidden collection.
    hidden: bool,
}

/// Where a stored file was found; `changed` means its content may differ from what's stored.
#[derive(Debug, Clone)]
struct FileMatch {
    path: AudioFilePath,
    stamp: FileStamp,
    changed: bool,
}

/// What an authoritative look at a stored file's own path showed.
#[derive(Debug, Clone)]
enum Seen {
    Present(FileStamp),
    Absent,
}

pub(super) fn match_files(input: &ReconcileInput) -> MatchedFiles {
    let scanned = index_scanned_files(input);
    let roots = authoritative_roots(&input.library_folders);
    let observed = input
        .files
        .iter()
        .filter_map(|file| {
            let seen = look_at(file, &scanned, &input.stat_results, &roots)?;
            Some((file, seen))
        })
        .collect();
    let mut matcher = Matcher {
        scanned: &scanned,
        claimed: BTreeSet::new(),
        found: HashMap::new(),
    };
    let unmatched = matcher.match_in_place(observed);
    let unmatched = matcher.match_moves(unmatched);
    let absent = matcher.match_rewrites(unmatched);
    let candidates = matcher.unclaimed().map(|(path, _)| path.clone()).collect();
    let found = matcher.found;
    MatchedFiles {
        scanned,
        found,
        absent,
        candidates,
    }
}

/// Every scanned audio file by path. A file inside two library folders appears once, hidden only
/// when every collection of its folder is.
fn index_scanned_files(input: &ReconcileInput) -> BTreeMap<PathBuf, ScannedEntry> {
    let hidden_folders = input
        .collections
        .iter()
        .filter(|collection| collection.hidden)
        .map(|collection| {
            (
                collection.library_folder_id,
                collection.folder_path.as_ref().as_path(),
            )
        })
        .collect::<BTreeSet<_>>();
    let scanned_files = input
        .library_folders
        .iter()
        .filter_map(|library_folder| match &library_folder.outcome {
            ScanOutcome::Scanned(scan) => Some((library_folder.id, scan)),
            ScanOutcome::Gone | ScanOutcome::Unreadable => None,
        })
        .flat_map(|(library_folder_id, scan)| {
            scan.folders
                .iter()
                .map(move |folder| (library_folder_id, folder))
        })
        .flat_map(|(library_folder_id, folder)| {
            let hidden =
                hidden_folders.contains(&(library_folder_id, folder.path.as_ref().as_path()));
            folder.audio_files.iter().map(move |file| (file, hidden))
        });
    let mut scanned = BTreeMap::new();
    for (file, hidden) in scanned_files {
        scanned
            .entry(file.path.as_ref().clone())
            .and_modify(|entry: &mut ScannedEntry| entry.hidden &= hidden)
            .or_insert_with(|| ScannedEntry {
                file: file.clone(),
                hidden,
            });
    }
    scanned
}

/// What's at the stored file's own path, or `None` when nothing authoritative says.
fn look_at(
    file: &KnownFile,
    scanned: &BTreeMap<PathBuf, ScannedEntry>,
    stat_results: &HashMap<AudioFileId, Option<FileStamp>>,
    roots: &[&Path],
) -> Option<Seen> {
    if let Some(entry) = scanned.get(file.path.as_ref()) {
        return Some(Seen::Present(entry.file.stamp.clone()));
    }
    if is_under_any(file.path.as_ref(), roots) {
        return Some(Seen::Absent);
    }
    match stat_results.get(&file.id)? {
        Some(stamp) => Some(Seen::Present(stamp.clone())),
        None => Some(Seen::Absent),
    }
}

struct Matcher<'a> {
    scanned: &'a BTreeMap<PathBuf, ScannedEntry>,
    claimed: BTreeSet<PathBuf>,
    found: HashMap<AudioFileId, FileMatch>,
}

type Observed<'f> = Vec<(&'f KnownFile, Seen)>;

impl Matcher<'_> {
    fn claim(&mut self, id: AudioFileId, path: AudioFilePath, stamp: FileStamp, changed: bool) {
        self.claimed.insert(path.as_ref().clone());
        self.found.insert(
            id,
            FileMatch {
                path,
                stamp,
                changed,
            },
        );
    }

    fn unclaimed(&self) -> impl Iterator<Item = (&PathBuf, &ScannedEntry)> {
        self.scanned
            .iter()
            .filter(|(path, entry)| !self.claimed.contains(*path) && !entry.hidden)
    }

    /// The same file identity at the stored path is the file itself.
    fn match_in_place<'f>(&mut self, observed: Observed<'f>) -> Observed<'f> {
        observed
            .into_iter()
            .filter_map(|(file, seen)| match seen {
                Seen::Present(stamp) if stamp.identity == file.stamp.identity => {
                    let changed = stamp != file.stamp;
                    self.claim(file.id, file.path.clone(), stamp, changed);
                    None
                }
                seen => Some((file, seen)),
            })
            .collect()
    }

    /// Renames and moves within a volume keep the file identity. Identities shared by several
    /// stored or scanned files are hard links, which say nothing about where a file went.
    fn match_moves<'f>(&mut self, unmatched: Observed<'f>) -> Observed<'f> {
        let scanned = self.scanned;
        let scanned_by_identity = unique_by(
            self.unclaimed()
                .map(|(path, entry)| (entry.file.stamp.identity.clone(), path.clone())),
        );
        let stored_by_identity = unique_by(
            unmatched
                .iter()
                .map(|(file, _)| (file.stamp.identity.clone(), file.id)),
        );
        unmatched
            .into_iter()
            .filter_map(|(file, seen)| {
                let moved_to = stored_by_identity
                    .contains_key(&file.stamp.identity)
                    .then(|| scanned_by_identity.get(&file.stamp.identity))
                    .flatten();
                let Some(path) = moved_to else {
                    return Some((file, seen));
                };
                let moved = &scanned[path].file;
                let changed = !same_content(&moved.stamp, &file.stamp);
                self.claim(file.id, moved.path.clone(), moved.stamp.clone(), changed);
                None
            })
            .collect()
    }

    /// Another file at the stored path, which nothing else claimed, is the file rewritten in
    /// place. What's left is absent.
    fn match_rewrites(&mut self, unmatched: Observed<'_>) -> Vec<AudioFileId> {
        unmatched
            .into_iter()
            .filter_map(|(file, seen)| match seen {
                Seen::Present(stamp) if !self.claimed.contains(file.path.as_ref()) => {
                    self.claim(file.id, file.path.clone(), stamp, true);
                    None
                }
                Seen::Present(_) | Seen::Absent => Some(file.id),
            })
            .collect()
    }
}

fn same_content(left: &FileStamp, right: &FileStamp) -> bool {
    left.size_bytes == right.size_bytes && left.modified_ns == right.modified_ns
}

/// The file changes, and where each present file is once they apply.
#[derive(Debug, Default)]
pub(super) struct SettledFiles {
    pub(super) changes: ReconcileChanges,
    pub(super) present: BTreeMap<PathBuf, FileRef>,
}

impl MatchedFiles {
    pub(super) fn inspections_needed(&self) -> Vec<AudioFilePath> {
        let changed = self
            .found
            .values()
            .filter(|found| found.changed)
            .map(|found| found.path.clone());
        let candidates = self
            .candidates
            .iter()
            .map(|path| self.scanned[path].file.path.clone());
        changed.chain(candidates).collect()
    }

    /// Turns the matches into changes: found files update, absent files relink by fingerprint
    /// or go missing, and the remaining readable candidates are new.
    pub(super) fn settle(
        self,
        stored: &[KnownFile],
        inspections: &HashMap<PathBuf, Option<AudioFacts>>,
    ) -> SettledFiles {
        let facts_of = |path: &Path| inspections.get(path).cloned().flatten();
        let stored_by_id = stored
            .iter()
            .map(|file| (file.id, file))
            .collect::<HashMap<_, _>>();
        let mut settled = SettledFiles::default();

        let mut found = self.found.into_iter().collect::<Vec<_>>();
        found.sort_by(|left, right| left.1.path.as_ref().cmp(right.1.path.as_ref()));
        for (id, found) in found {
            let facts = found
                .changed
                .then(|| facts_of(found.path.as_ref()))
                .flatten();
            settled.settle_found(stored_by_id[&id], found, facts);
        }

        let mut readable = BTreeMap::new();
        for path in self.candidates {
            match facts_of(&path) {
                Some(facts) => {
                    readable.insert(path, facts);
                }
                None => settled
                    .changes
                    .unreadable_files
                    .push(self.scanned[&path].file.path.clone()),
            }
        }

        let mut absent = self
            .absent
            .iter()
            .map(|id| stored_by_id[id])
            .collect::<Vec<_>>();
        absent.sort_by(|left, right| left.path.as_ref().cmp(right.path.as_ref()));
        for file in absent {
            match take_relink_candidate(&mut readable, file) {
                Some((path, facts)) => settled.relink(file, &self.scanned[&path].file, facts),
                None => settled.mark_missing(file),
            }
        }
        for (path, facts) in readable {
            settled.add_new(&self.scanned[&path].file, facts);
        }
        settled
    }
}

/// The readable candidate with the absent file's fingerprint, preferring one with the same file
/// name.
fn take_relink_candidate(
    readable: &mut BTreeMap<PathBuf, AudioFacts>,
    file: &KnownFile,
) -> Option<(PathBuf, AudioFacts)> {
    let path = readable
        .iter()
        .filter(|(_, facts)| facts.fingerprint == file.fingerprint)
        .map(|(path, _)| path)
        .min_by_key(|path| path.file_name() != file.path.as_ref().file_name())?
        .clone();
    readable.remove_entry(&path)
}

impl SettledFiles {
    fn settle_found(&mut self, stored: &KnownFile, found: FileMatch, facts: Option<AudioFacts>) {
        self.present
            .insert(found.path.as_ref().clone(), FileRef::Existing(stored.id));
        let moved = found.path != stored.path;
        if !moved && facts.is_none() {
            if stored.missing {
                self.changes
                    .file_changes
                    .push(FileChange::Found { id: stored.id });
            }
            return;
        }
        // A file whose new content couldn't be read keeps the stored stamp, so the next
        // reconciliation reads it again.
        let stamp = if found.changed && facts.is_none() {
            stored.stamp.clone()
        } else {
            found.stamp
        };
        self.changes.file_changes.push(FileChange::Updated {
            id: stored.id,
            path: found.path,
            stamp,
            facts,
        });
    }

    fn relink(&mut self, stored: &KnownFile, scanned: &ScannedFile, facts: AudioFacts) {
        self.present
            .insert(scanned.path.as_ref().clone(), FileRef::Existing(stored.id));
        self.changes.file_changes.push(FileChange::Updated {
            id: stored.id,
            path: scanned.path.clone(),
            stamp: scanned.stamp.clone(),
            facts: Some(facts),
        });
    }

    fn mark_missing(&mut self, stored: &KnownFile) {
        if !stored.missing {
            self.changes
                .file_changes
                .push(FileChange::Missing { id: stored.id });
        }
    }

    fn add_new(&mut self, scanned: &ScannedFile, facts: AudioFacts) {
        self.present.insert(
            scanned.path.as_ref().clone(),
            FileRef::New(self.changes.new_files.len()),
        );
        self.changes.new_files.push(NewFile {
            path: scanned.path.clone(),
            stamp: scanned.stamp.clone(),
            facts,
            title: audio_file_title(&scanned.path),
        });
    }
}
