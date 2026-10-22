//! Shared admission and residency for owned decoded audio, prepared away from the mixer.

use std::{
    io,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

const AUDIO_BYTES_MAX: usize = 128 * 1024 * 1024;
const RECLAIM_INTERVAL: Duration = Duration::from_millis(25);
const RETAINED_ALLOCATIONS_MAX: usize = 1_024;

#[derive(Debug, Clone, Default)]
pub struct PreparationCancellation(Arc<AtomicBool>);

impl PreparationCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AudioMemorySnapshot {
    pub bytes_used: usize,
    pub bytes_max: usize,
    pub locked_bytes: usize,
    pub lock_failures: u64,
    pub lock_failed_bytes: usize,
    pub allocations: usize,
    pub locking_supported: bool,
}

#[derive(Debug, Clone)]
pub struct AudioMemory(Arc<MemoryState>);

#[derive(Debug)]
struct MemoryState {
    bytes_max: usize,
    bytes_used: AtomicUsize,
    locked_bytes: AtomicUsize,
    lock_failures: AtomicU64,
    lock_failed_bytes: AtomicUsize,
    allocations: AtomicUsize,
}

impl AudioMemory {
    #[must_use]
    pub fn new(bytes_max: usize) -> Self {
        Self(Arc::new(MemoryState {
            bytes_max,
            bytes_used: AtomicUsize::new(0),
            locked_bytes: AtomicUsize::new(0),
            lock_failures: AtomicU64::new(0),
            lock_failed_bytes: AtomicUsize::new(0),
            allocations: AtomicUsize::new(0),
        }))
    }

    #[must_use]
    pub fn shared() -> Self {
        static MEMORY: OnceLock<AudioMemory> = OnceLock::new();
        MEMORY.get_or_init(|| Self::new(AUDIO_BYTES_MAX)).clone()
    }

    #[must_use]
    pub fn snapshot(&self) -> AudioMemorySnapshot {
        AudioMemorySnapshot {
            bytes_used: self.0.bytes_used.load(Ordering::Relaxed),
            bytes_max: self.0.bytes_max,
            locked_bytes: self.0.locked_bytes.load(Ordering::Relaxed),
            lock_failures: self.0.lock_failures.load(Ordering::Relaxed),
            lock_failed_bytes: self.0.lock_failed_bytes.load(Ordering::Relaxed),
            allocations: self.0.allocations.load(Ordering::Relaxed),
            locking_supported: cfg!(target_os = "linux"),
        }
    }

    #[must_use]
    pub fn available_bytes(&self) -> usize {
        self.0
            .bytes_max
            .saturating_sub(self.0.bytes_used.load(Ordering::Relaxed))
    }

    pub(crate) fn reserve(&self, bytes: usize) -> Option<MemoryReservation> {
        let bytes = allocation_bytes(bytes)?;
        self.0
            .bytes_used
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.0.bytes_max)
            })
            .ok()?;
        self.0.allocations.fetch_add(1, Ordering::Relaxed);
        Some(MemoryReservation {
            memory: self.clone(),
            bytes,
        })
    }
}

pub(crate) struct MemoryReservation {
    memory: AudioMemory,
    bytes: usize,
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.memory
            .0
            .bytes_used
            .fetch_sub(self.bytes, Ordering::AcqRel);
        self.memory.0.allocations.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Charges both boundary pages before the allocation's address is known.
pub(crate) fn allocation_bytes(bytes: usize) -> Option<usize> {
    if bytes == 0 {
        return Some(0);
    }
    let page = page_bytes();
    bytes
        .checked_add(page - 1)?
        .checked_add(page - 1)
        .map(|bytes| bytes / page * page)
}

fn page_bytes() -> usize {
    #[cfg(target_os = "linux")]
    {
        static PAGE_BYTES: OnceLock<usize> = OnceLock::new();
        *PAGE_BYTES.get_or_init(|| {
            // SAFETY: sysconf has no pointer arguments or ownership requirements.
            usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
                .ok()
                .filter(|bytes| *bytes > 0)
                .unwrap_or(4_096)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        4_096
    }
}

pub(crate) struct ResidentMemory {
    #[cfg(target_os = "linux")]
    start: usize,
    #[cfg(target_os = "linux")]
    end: usize,
}

impl ResidentMemory {
    /// # Safety
    /// Keep the slice's allocation fixed and alive until this guard drops.
    pub(crate) unsafe fn prepare<T>(slice: &[T], memory: &AudioMemory) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::prepare_with_lock(slice, memory, lock_pages, unlock_pages)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (slice, memory);
            Self {}
        }
    }

    #[cfg(target_os = "linux")]
    fn prepare_with_lock<T>(
        slice: &[T],
        memory: &AudioMemory,
        lock: impl FnOnce(usize, usize) -> io::Result<()>,
        rollback: impl FnMut(usize, usize),
    ) -> Self {
        let page = page_bytes();
        let address = slice.as_ptr() as usize;
        let start = address / page * page;
        let end = address.saturating_add(size_of_val(slice)).div_ceil(page) * page;
        if slice.is_empty() {
            return Self { start: 0, end: 0 };
        }
        let mut pages = resident_pages()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = lock(start, end - start);
        let locked = result.is_ok();
        if let Err(error) = result {
            memory.0.lock_failures.fetch_add(1, Ordering::Relaxed);
            memory
                .0
                .lock_failed_bytes
                .fetch_add(end - start, Ordering::Relaxed);
            tracing::warn!(%error, bytes = end - start, "audio buffer residency could not be locked");
            // A failed mlock can leave part of its range locked. Existing locked owners must
            // survive rollback, including allocator boundary pages shared by two buffers.
            unlock_page_runs(
                start,
                end,
                |address| !pages.get(&address).is_some_and(|page| page.locked),
                rollback,
            );
        }
        for address in (start..end).step_by(page) {
            let entry = pages.entry(address).or_insert_with(|| ResidentPage {
                owners: 0,
                memory: memory.clone(),
                locked: false,
            });
            entry.owners += 1;
            if locked && !entry.locked {
                entry.locked = true;
                entry
                    .memory
                    .0
                    .locked_bytes
                    .fetch_add(page, Ordering::Relaxed);
            }
        }
        Self { start, end }
    }
}

#[cfg(target_os = "linux")]
struct ResidentPage {
    owners: usize,
    memory: AudioMemory,
    locked: bool,
}

#[cfg(target_os = "linux")]
fn resident_pages() -> &'static std::sync::Mutex<std::collections::BTreeMap<usize, ResidentPage>> {
    static PAGES: OnceLock<std::sync::Mutex<std::collections::BTreeMap<usize, ResidentPage>>> =
        OnceLock::new();
    PAGES.get_or_init(std::sync::Mutex::default)
}

impl Drop for ResidentMemory {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let mut pages = resident_pages()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            unlock_page_runs(
                self.start,
                self.end,
                |address| release_page(&mut pages, address),
                unlock_pages,
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn release_page(
    pages: &mut std::collections::BTreeMap<usize, ResidentPage>,
    address: usize,
) -> bool {
    let Some(entry) = pages.get_mut(&address) else {
        return false;
    };
    entry.owners -= 1;
    if entry.owners != 0 {
        return false;
    }
    let Some(entry) = pages.remove(&address) else {
        return false;
    };
    if !entry.locked {
        return false;
    }
    entry
        .memory
        .0
        .locked_bytes
        .fetch_sub(page_bytes(), Ordering::Relaxed);
    true
}

#[cfg(target_os = "linux")]
fn lock_pages(start: usize, bytes: usize) -> io::Result<()> {
    // SAFETY: preparation keeps the allocation alive under its residency guard, and the
    // registry tracks every overlapping page owner until its last release.
    if unsafe { libc::mlock(start as *const libc::c_void, bytes) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn unlock_page_runs(
    start: usize,
    end: usize,
    mut release: impl FnMut(usize) -> bool,
    mut unlock: impl FnMut(usize, usize),
) {
    let page = page_bytes();
    let mut unlock_start = None;
    let mut unlock_end = 0;
    for address in (start..end).step_by(page) {
        if release(address) {
            unlock_start.get_or_insert(address);
            unlock_end = address + page;
        } else if let Some(start) = unlock_start.take() {
            unlock(start, unlock_end);
        }
    }
    if let Some(start) = unlock_start {
        unlock(start, unlock_end);
    }
}

#[cfg(target_os = "linux")]
fn unlock_pages(start: usize, end: usize) {
    // SAFETY: the allocation remains alive, and none of these pages has a registered owner
    // relying on a successful lock after the registry update or failed-lock rollback.
    if unsafe { libc::munlock(start as *const libc::c_void, end - start) } != 0 {
        tracing::warn!(error = %io::Error::last_os_error(), "audio buffer residency could not be unlocked");
    }
}

type RetainedAllocation = Arc<dyn Send + Sync>;
struct Reclaimer {
    allocations: Arc<std::sync::Mutex<Vec<RetainedAllocation>>>,
}

impl Reclaimer {
    fn retain(&self, allocation: RetainedAllocation) -> io::Result<()> {
        let mut retained = self
            .allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        retained.retain(|allocation| Arc::strong_count(allocation) > 1);
        if retained.len() == RETAINED_ALLOCATIONS_MAX {
            return Err(io::Error::other("audio allocation keeper is full"));
        }
        retained.push(allocation);
        Ok(())
    }
}

fn reclaimer() -> io::Result<&'static Reclaimer> {
    static RECLAIMER: OnceLock<Result<Reclaimer, Arc<io::Error>>> = OnceLock::new();
    RECLAIMER
        .get_or_init(|| {
            let allocations = Arc::new(std::sync::Mutex::new(Vec::<RetainedAllocation>::new()));
            let worker_allocations = Arc::clone(&allocations);
            thread::Builder::new()
                .name("tufo-audio-memory".into())
                .spawn(move || {
                    loop {
                        worker_allocations
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .retain(|allocation| Arc::strong_count(allocation) > 1);
                        thread::sleep(RECLAIM_INTERVAL);
                    }
                })
                .map(|_| Reclaimer { allocations })
                .map_err(Arc::new)
        })
        .as_ref()
        .map_err(|error| io::Error::other(Arc::clone(error)))
}

/// Keeps the last owner off the mixer, with a bounded number of retained allocations.
pub(crate) fn retain_allocation<T: Send + Sync + 'static>(allocation: Arc<T>) -> io::Result<()> {
    reclaimer()?.retain(allocation)
}

/// Called only by control/preparation workers, never the audio callback.
pub(crate) fn reclaim_unused() {
    if let Ok(reclaimer) = reclaimer() {
        reclaimer
            .allocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|allocation| Arc::strong_count(allocation) > 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_reservations_cannot_overbook_and_release_their_capacity() {
        let bytes = allocation_bytes(8_000).unwrap();
        let memory = AudioMemory::new(bytes * 2);
        let first = memory.reserve(8_000).unwrap();
        let second = memory.reserve(8_000).unwrap();
        assert!(memory.reserve(1).is_none());
        drop(first);
        assert!(memory.reserve(8_000).is_some());
        drop(second);
        assert_eq!(memory.snapshot().bytes_used, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlapping_residency_guards_keep_the_page_until_the_last_owner() {
        let page = page_bytes();
        let frames = vec![0_u8; page * 3];
        let offset = (page - frames.as_ptr() as usize % page) % page;
        let slice = &frames[offset..offset + page];
        let memory = AudioMemory::new(page * 3);
        // SAFETY: both guards drop while frames remains fixed and alive.
        let first = unsafe { ResidentMemory::prepare(slice, &memory) };
        // SAFETY: the same fixed allocation also outlives this overlapping guard.
        let second = unsafe { ResidentMemory::prepare(slice, &memory) };
        let address = slice.as_ptr() as usize;
        assert_eq!(resident_pages().lock().unwrap()[&address].owners, 2);
        drop(first);
        assert_eq!(resident_pages().lock().unwrap()[&address].owners, 1);
        drop(second);
        assert!(!resident_pages().lock().unwrap().contains_key(&address));
        assert_eq!(memory.snapshot().locked_bytes, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn denied_residency_is_counted_and_does_not_claim_locked_pages() {
        let page = page_bytes();
        let frames = vec![0_u8; page * 3];
        let offset = (page - frames.as_ptr() as usize % page) % page;
        let slice = &frames[offset..offset + page];
        let memory = AudioMemory::new(page * 3);
        let resident = ResidentMemory::prepare_with_lock(
            slice,
            &memory,
            |_, _| Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            |_, _| {},
        );
        let snapshot = memory.snapshot();
        assert_eq!(snapshot.lock_failures, 1);
        assert_eq!(snapshot.lock_failed_bytes, page);
        assert_eq!(snapshot.locked_bytes, 0);
        drop(resident);
        assert!(
            !resident_pages()
                .lock()
                .unwrap()
                .contains_key(&(slice.as_ptr() as usize))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_partial_lock_failure_rolls_back_new_pages_and_preserves_overlapping_owners() {
        use std::{cell::RefCell, collections::BTreeSet};

        let page = page_bytes();
        let frames = vec![0_u8; page * 5];
        let offset = (page - frames.as_ptr() as usize % page) % page;
        let slice = &frames[offset..offset + page * 3];
        let start = slice.as_ptr() as usize;
        let memory = AudioMemory::new(page * 5);
        let locked = RefCell::new(BTreeSet::new());
        let original = ResidentMemory::prepare_with_lock(
            &slice[page..page * 2],
            &memory,
            |address, _| {
                locked.borrow_mut().insert(address);
                Ok(())
            },
            |_, _| {},
        );
        let mut rolled_back = Vec::new();

        let partial = ResidentMemory::prepare_with_lock(
            slice,
            &memory,
            |address, bytes| {
                locked
                    .borrow_mut()
                    .extend((address..address + bytes).step_by(page));
                Err(io::Error::from_raw_os_error(libc::EAGAIN))
            },
            |start, end| {
                rolled_back.push((start, end));
                for address in (start..end).step_by(page) {
                    locked.borrow_mut().remove(&address);
                }
            },
        );

        assert_eq!(
            rolled_back,
            [(start, start + page), (start + page * 2, start + page * 3)]
        );
        assert_eq!(*locked.borrow(), BTreeSet::from([start + page]));
        assert_eq!(memory.snapshot().locked_bytes, page);
        assert_eq!(memory.snapshot().lock_failures, 1);
        drop(partial);
        assert_eq!(resident_pages().lock().unwrap()[&(start + page)].owners, 1);
        assert_eq!(memory.snapshot().locked_bytes, page);
        drop(original);
        assert_eq!(memory.snapshot().locked_bytes, 0);
    }

    #[test]
    fn retained_allocations_have_a_hard_count_limit() {
        let reclaimer = Reclaimer {
            allocations: Arc::default(),
        };
        let mut owners = Vec::new();
        for _ in 0..RETAINED_ALLOCATIONS_MAX {
            let allocation = Arc::new(());
            reclaimer.retain(allocation.clone()).unwrap();
            owners.push(allocation);
        }
        assert!(reclaimer.retain(Arc::new(())).is_err());
        drop(owners);
        assert!(reclaimer.retain(Arc::new(())).is_ok());
    }

    #[test]
    fn the_housekeeping_worker_frees_a_decoded_allocation_after_its_reader_drops() {
        struct RecordedDrop(std::sync::mpsc::Sender<Option<String>>);
        impl Drop for RecordedDrop {
            fn drop(&mut self) {
                self.0
                    .send(thread::current().name().map(str::to_owned))
                    .unwrap();
            }
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let allocation = Arc::new(RecordedDrop(sender));
        retain_allocation(Arc::clone(&allocation)).unwrap();
        drop(allocation);
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .as_deref(),
            Some("tufo-audio-memory")
        );
    }
}
