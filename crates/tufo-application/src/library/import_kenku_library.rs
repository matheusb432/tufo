//! Saved, resumable Kenku FM imports with explicit collection conflict choices.

mod apply;
mod conflicts;
mod files;
pub(super) mod journal;
pub mod kenku_data;
mod kenku_library;
mod kenku_titles;
mod plan;

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use jiff::Timestamp;
use tufo_models::{
    kenku_import::{ImportProgress, KenkuImport, KenkuImportId, KenkuImportState},
    playback::queue::RepeatMode,
};
use tufo_wire::{library::ImportKenkuLibrary, player::UpdatePlayerSettings};

pub use self::kenku_library::{
    KenkuBackground, KenkuCollection, KenkuItem, KenkuLibrary, KenkuPlayerSettings, KenkuSource,
};
use self::{
    journal::Details,
    kenku_data::{KenkuReader, ReadKenkuDataError},
};
use super::{Library, LibraryStorageError, library_files::LibraryFiles};
use crate::{
    discord::bot_credentials::BotCredentials,
    error::{ApplicationError as _, ErrorMeta},
    playback::{
        player::Player,
        update_player_settings::{self, UpdatePlayerSettingsError},
    },
};

/// A GUI disconnect does not cancel work; every server attempt has its own upper bound.
pub const IMPORT_TIMEOUT: Duration = Duration::from_mins(5);
pub const INTERRUPTED: &str = "library.kenku_import.interrupted";
pub const TIMED_OUT: &str = "library.kenku_import.timed_out";

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum ImportKenkuLibraryError {
    #[error(transparent)]
    #[meta(transparent)]
    Read(#[from] ReadKenkuDataError),
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
    #[error(transparent)]
    #[meta(transparent)]
    PlayerSettings(#[from] UpdatePlayerSettingsError),
    #[error("another Kenku FM import is in progress")]
    #[meta(class = FailedPrecondition, reason = Self::BUSY)]
    Busy,
    #[error("the saved Kenku FM import does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("choose Keep or Overwrite for each conflicting collection")]
    #[meta(class = InvalidArgument, reason = Self::CHOICES)]
    Choices,
}

impl ImportKenkuLibraryError {
    pub const BUSY: &'static str = "library.kenku_import.busy";
    pub const NOT_FOUND: &'static str = "library.kenku_import.not_found";
    pub const CHOICES: &'static str = "library.kenku_import.choices";
}

/// Starts or resumes one durable import and returns immediately with its ID. The watch stream
/// sends progress, conflicts and the saved result. Repeated starts while running reuse the job.
pub async fn execute<F, P, R, C>(
    mut request: ImportKenkuLibrary,
    library: Arc<Library<F>>,
    player: Arc<P>,
    reader: R,
    credentials: C,
) -> Result<KenkuImportId, ImportKenkuLibraryError>
where
    F: LibraryFiles + 'static,
    P: Player + 'static,
    R: KenkuReader + 'static,
    C: BotCredentials + 'static,
{
    let _admission = library.kenku_imports.admission.lock().await;
    if library.kenku_imports.stopping.load(Ordering::Acquire) {
        return Err(ImportKenkuLibraryError::Busy);
    }
    let current = library.kenku_imports.watch().borrow().clone();
    if let Some(current) = &current
        && (current.state.is_running() || current.state == KenkuImportState::AwaitingChoices)
        && request.import_id.is_some_and(|id| id != current.id)
    {
        return Err(ImportKenkuLibraryError::Busy);
    }
    if let Some(current) = current
        && (current.state.is_running()
            || (current.state == KenkuImportState::AwaitingChoices && request.import_id.is_none()))
    {
        if let Some(folder) = &request.kenku_folder {
            let stored = KenkuImport::filter_by_id(current.id)
                .first()
                .exec(&mut library.database())
                .await
                .map_err(LibraryStorageError::from)?
                .ok_or(ImportKenkuLibraryError::NotFound)?;
            if stored.source_folder.as_ref() != Some(folder) {
                return Err(ImportKenkuLibraryError::Busy);
            }
        }
        return Ok(current.id);
    }
    let mut import = prepare_import(&mut request, &library).await?;
    let id = import.id;
    if import.state == KenkuImportState::AwaitingChoices {
        return Ok(id);
    }
    let mut database = library.database();
    import.error_reason = None;
    import.state = KenkuImportState::Reading;
    import.progress = ImportProgress::START;
    journal::save(&mut database, &import).await?;
    library.kenku_imports.publish(&import)?;
    let worker_library = Arc::clone(&library);
    let worker = tokio::spawn(async move {
        let outcome = tokio::time::timeout(
            IMPORT_TIMEOUT,
            run(
                &request.choices,
                &worker_library,
                player.as_ref(),
                &reader,
                &credentials,
                &mut import,
            ),
        )
        .await;
        let reason = match outcome {
            Ok(Ok(())) => return,
            Ok(Err(error)) => {
                tracing::warn!(import_id = %id, %error, "Kenku FM import failed");
                error.metadata().reason
            }
            Err(_) => TIMED_OUT,
        };
        save_failure(&worker_library, id, reason).await;
    });
    *library.kenku_imports.worker.lock().await = Some(worker);
    Ok(id)
}

async fn prepare_import<F: LibraryFiles>(
    request: &mut ImportKenkuLibrary,
    library: &Library<F>,
) -> Result<KenkuImport, ImportKenkuLibraryError> {
    let mut database = library.database();
    if let Some(id) = request.import_id {
        let mut import = KenkuImport::filter_by_id(id)
            .first()
            .exec(&mut database)
            .await
            .map_err(LibraryStorageError::from)?
            .ok_or(ImportKenkuLibraryError::NotFound)?;
        if request.kenku_folder.is_some() && request.kenku_folder != import.source_folder {
            return Err(ImportKenkuLibraryError::Choices);
        }
        let mut details = Details::read(&import)?;
        if request.choices.is_empty() && import.state != KenkuImportState::AwaitingChoices {
            request.choices = details.choices.iter().map(Into::into).collect();
        }
        if request.choices.is_empty() && !details.conflicts.is_empty() {
            import.error_reason = None;
            journal::advance(
                &mut database,
                &library.kenku_imports,
                &mut import,
                KenkuImportState::AwaitingChoices,
                15,
            )
            .await?;
            return Ok(import);
        }
        conflicts::validate_choices(&details, &request.choices)?;
        details.choices = request.choices.iter().map(Into::into).collect();
        import.details = details.encode()?;
        import.state = KenkuImportState::Reading;
        return Ok(import);
    }
    if !request.choices.is_empty() {
        return Err(ImportKenkuLibraryError::Choices);
    }
    Ok(toasty::create!(KenkuImport {
        source_folder: request.kenku_folder.clone(),
        state: KenkuImportState::Reading,
        progress: ImportProgress::START,
        details: Details::default().encode()?,
        error_reason: None,
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    })
    .exec(&mut database)
    .await
    .map_err(LibraryStorageError::from)?)
}

async fn save_failure<F: LibraryFiles>(library: &Library<F>, id: KenkuImportId, reason: &str) {
    // Reload after cancellation: the collection transaction may have committed, and its
    // journal outcome must survive even if later settings or publication failed.
    let mut database = library.database();
    match KenkuImport::filter_by_id(id)
        .first()
        .exec(&mut database)
        .await
    {
        Ok(Some(mut stored)) => {
            stored.state = KenkuImportState::Failed;
            stored.error_reason = Some(reason.to_owned());
            if let Err(error) = journal::save(&mut database, &stored)
                .await
                .and_then(|()| library.kenku_imports.publish(&stored))
            {
                tracing::error!(%error, "cannot save Kenku FM import failure");
            }
            let _change = library.begin_change().await;
            if let Err(error) = library.publish().await {
                tracing::error!(%error, "cannot publish the library after an import failure");
            }
        }
        other => tracing::error!(?other, "cannot reload the failed Kenku FM import"),
    }
}

/// Stops a managed worker before shutdown. A later launch offers a retry of interrupted work.
pub async fn interrupt<F: LibraryFiles>(library: &Library<F>) -> Result<(), LibraryStorageError> {
    library
        .kenku_imports
        .stopping
        .store(true, Ordering::Release);
    let _admission = library.kenku_imports.admission.lock().await;
    if let Some(worker) = library.kenku_imports.worker.lock().await.take() {
        worker.abort();
        let _ = worker.await;
    }
    let mut database = library.database();
    let current = library.kenku_imports.watch().borrow().clone();
    if let Some(current) = current
        && current.state.is_running()
        && let Some(mut import) = KenkuImport::filter_by_id(current.id)
            .first()
            .exec(&mut database)
            .await?
    {
        import.state = KenkuImportState::Interrupted;
        import.error_reason = Some(INTERRUPTED.to_owned());
        journal::save(&mut database, &import).await?;
        library.kenku_imports.publish(&import)?;
    }
    Ok(())
}

async fn run<F: LibraryFiles>(
    choices: &[tufo_wire::library::KenkuCollectionChoice],
    library: &Library<F>,
    player: &impl Player,
    reader: &impl KenkuReader,
    credentials: &impl BotCredentials,
    import: &mut KenkuImport,
) -> Result<(), ImportKenkuLibraryError> {
    let data = reader.read(import.source_folder.clone()).await?;
    let mut database = library.database();
    journal::advance(
        &mut database,
        &library.kenku_imports,
        import,
        KenkuImportState::Reading,
        5,
    )
    .await?;
    let mut details = Details::read(import)?;
    let Some(mut report) = apply::collections(
        data.library.collections,
        choices,
        library,
        import,
        &mut details,
    )
    .await?
    else {
        return Ok(());
    };
    let songs_queued = !player.watch_player().borrow().queued_entry_ids.is_empty();
    let update = settings_update(data.library.player_settings, songs_queued);
    if update != UpdatePlayerSettings::default() {
        update_player_settings::execute(update, player, library.database()).await?;
    }
    if let Some(token) = data.discord_bot_token {
        report.discord_bot_token_imported |= credentials
            .save_unless_present(token)
            .await
            .map_err(|error| LibraryStorageError(error.into()))?;
    }
    details.report = (&report).into();
    let encoded = details.encode()?;
    if encoded != import.details {
        import.details = encoded;
        journal::save(&mut database, import).await?;
    }
    journal::advance(
        &mut database,
        &library.kenku_imports,
        import,
        KenkuImportState::Succeeded,
        100,
    )
    .await?;
    Ok(())
}

/// Songs queued now keep Kenku FM's repeat-one out, since repeating one track refuses them.
fn settings_update(kenku: KenkuPlayerSettings, songs_queued: bool) -> UpdatePlayerSettings {
    let KenkuPlayerSettings {
        volume,
        muted,
        shuffle,
        repeat,
    } = kenku;
    UpdatePlayerSettings {
        volume,
        effects_volume: None,
        muted,
        shuffle,
        repeat: repeat.filter(|repeat| !(songs_queued && *repeat == RepeatMode::Track)),
        crossfade: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_songs_keep_kenkus_repeat_one_out_of_the_settings() {
        let repeating_track = KenkuPlayerSettings {
            volume: None,
            muted: Some(true),
            shuffle: None,
            repeat: Some(RepeatMode::Track),
        };
        let repeating_playlist = KenkuPlayerSettings {
            repeat: Some(RepeatMode::Playlist),
            ..repeating_track
        };

        assert_eq!(settings_update(repeating_track, true).repeat, None);
        assert_eq!(settings_update(repeating_track, true).muted, Some(true));
        assert_eq!(
            settings_update(repeating_track, false).repeat,
            Some(RepeatMode::Track)
        );
        assert_eq!(
            settings_update(repeating_playlist, true).repeat,
            Some(RepeatMode::Playlist)
        );
    }
}
