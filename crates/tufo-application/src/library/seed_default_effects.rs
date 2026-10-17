//! Seeding the Effects soundboard with the default effects, once per library.

use jiff::Timestamp;
use toasty::{Db, db::Executor};
use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    library::{
        AudioFile, BackgroundPreset, BuiltinTitle, CollectionId, CollectionKind, SoundSettings,
        Title,
        default_effects::{DEFAULT_EFFECTS_SEED_ID, DefaultEffect, DefaultEffectsSeed},
    },
    playback::AudioFilePath,
};
use tufo_wire::library::Background;

use super::{
    Library, LibraryStorageError, library_files::LibraryFiles, manual_files, reconcile::NewFile,
    store,
};

const EFFECTS_TITLE: &str = "Effects";
const EFFECTS_BACKGROUND: BackgroundPreset = BackgroundPreset::Forest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultEffectFile {
    pub effect: DefaultEffect,
    pub path: AudioFilePath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedDefaultEffectsOutcome {
    AlreadySeeded,
    Seeded,
    NothingReadable { unreadable: Vec<AudioFilePath> },
}

pub async fn execute<F: LibraryFiles>(
    files: Vec<DefaultEffectFile>,
    library: &Library<F>,
) -> Result<SeedDefaultEffectsOutcome, LibraryStorageError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    if is_seeded(&mut database).await? {
        return Ok(SeedDefaultEffectsOutcome::AlreadySeeded);
    }
    let paths = files.iter().map(|file| file.path.clone()).collect();
    let read = manual_files::read_files(library.files(), paths).await;
    let readable_effects = read
        .readable
        .into_iter()
        .filter_map(|new_file| {
            let effect = files.iter().find(|file| file.path == new_file.path)?.effect;
            Some((effect, new_file))
        })
        .collect::<Vec<_>>();
    if readable_effects.is_empty() {
        return Ok(SeedDefaultEffectsOutcome::NothingReadable {
            unreadable: read.unreadable,
        });
    }
    if !read.unreadable.is_empty() {
        tracing::warn!(unreadable = ?read.unreadable, "cannot read some default effects");
    }

    let now = Timestamp::now();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await?;
    let soundboard = store::create_manual_collection(
        &mut transaction,
        CollectionKind::Soundboard,
        Title::from_name_lossy(EFFECTS_TITLE),
        Background::Preset(EFFECTS_BACKGROUND),
        Some(BuiltinTitle::Effects),
    )
    .await?;
    let mut positions = store::EntryPositionsAtEnd::default();
    for (effect, new_file) in &readable_effects {
        add_effect(
            &mut transaction,
            &mut positions,
            soundboard.id,
            *effect,
            new_file,
            now,
        )
        .await?;
    }
    record_seed(&mut transaction, now).await?;
    transaction.commit().await?;
    library.publish().await?;
    Ok(SeedDefaultEffectsOutcome::Seeded)
}

async fn add_effect(
    executor: &mut dyn Executor,
    positions: &mut store::EntryPositionsAtEnd,
    soundboard_id: CollectionId,
    effect: DefaultEffect,
    file: &NewFile,
    now: Timestamp,
) -> Result<(), LibraryStorageError> {
    let known = AudioFile::filter_by_file_path(file.path.clone())
        .first()
        .exec(executor)
        .await?;
    let audio_file_id = match known {
        Some(known) => known.id,
        None => {
            store::create_audio_file(executor, file, now, Some(effect.builtin_title()))
                .await?
                .id
        }
    };
    let looping = SoundSettings {
        looping: true,
        ..SoundSettings::default()
    };
    store::create_entry(
        executor,
        positions,
        soundboard_id,
        audio_file_id,
        now,
        looping,
        effect.icon(),
    )
    .await?;
    Ok(())
}

async fn is_seeded(database: &mut Db) -> Result<bool, LibraryStorageError> {
    let seed = DefaultEffectsSeed::filter_by_id(DEFAULT_EFFECTS_SEED_ID)
        .first()
        .exec(database)
        .await?;
    Ok(seed.is_some())
}

async fn record_seed(
    executor: &mut dyn Executor,
    now: Timestamp,
) -> Result<(), LibraryStorageError> {
    toasty::create!(DefaultEffectsSeed {
        id: DEFAULT_EFFECTS_SEED_ID,
        seeded_at: now,
    })
    .exec(executor)
    .await?;
    Ok(())
}
