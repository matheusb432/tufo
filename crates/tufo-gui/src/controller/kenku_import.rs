//! Kenku import interaction: durable status, conflict choices and source-folder selection.

use tufo_application::library::import_kenku_library::kenku_data::ReadKenkuDataError;
use tufo_client::{
    TufoClient,
    library::{ImportKenkuLibrary, KenkuCollectionChoice, KenkuImportStatus},
};
use tufo_models::{
    kenku_import::{KenkuImportId, KenkuImportState},
    path::FolderPath,
};

use super::Controller;
use crate::{i18n::tr, presentation, render};

#[derive(Default)]
pub(super) struct KenkuImportSession {
    pub(super) current: Option<KenkuImportStatus>,
    pub(super) choices: Vec<bool>,
    pending: Option<KenkuImportAttempt>,
    pub(super) starting: bool,
}

struct KenkuImportAttempt {
    id: KenkuImportId,
    token_was_imported: bool,
}

impl Controller {
    pub(crate) fn import_kenku_library(&self) {
        if self.lock().kenku.current.is_some() {
            render::open_kenku_import(&self.window);
        } else {
            self.start_kenku_import();
        }
    }

    pub(crate) fn start_kenku_import(&self) {
        self.submit_kenku_import(ImportKenkuLibrary::default());
    }

    pub(crate) fn retry_kenku_import(&self) {
        let import_id = self.lock().kenku.current.as_ref().map(|status| status.id);
        if let Some(import_id) = import_id {
            self.submit_kenku_import(ImportKenkuLibrary {
                import_id: Some(import_id),
                ..ImportKenkuLibrary::default()
            });
        }
    }

    pub(crate) fn choose_kenku_conflict(&self, index: i32, overwrite: bool) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let mut shared = self.lock();
        if let Some(choice) = shared.kenku.choices.get_mut(index) {
            *choice = overwrite;
        }
        if let Some(status) = &shared.kenku.current {
            render::kenku_import(&self.window, status.clone(), shared.kenku.choices.clone());
        }
    }

    pub(crate) fn confirm_kenku_import(&self) {
        let request = {
            let mut shared = self.lock();
            let Some(status) = &shared.kenku.current else {
                return;
            };
            if status.state != KenkuImportState::AwaitingChoices {
                return;
            }
            let request = ImportKenkuLibrary {
                import_id: Some(status.id),
                kenku_folder: None,
                choices: status
                    .conflicts
                    .iter()
                    .zip(&shared.kenku.choices)
                    .map(|(conflict, overwrite)| KenkuCollectionChoice {
                        source_id: conflict.source_id.clone(),
                        kind: conflict.kind,
                        overwrite: *overwrite,
                    })
                    .collect(),
            };
            // A re-offer can have identical labels when only the target's entries changed.
            shared.kenku.choices.fill(false);
            request
        };
        self.submit_kenku_import(request);
    }

    fn submit_kenku_import(&self, request: ImportKenkuLibrary) {
        let Some(client) = self.connected_client() else {
            return;
        };
        {
            let mut shared = self.lock();
            if shared.kenku.starting
                || shared
                    .kenku
                    .current
                    .as_ref()
                    .is_some_and(|status| status.state.is_running())
            {
                render::open_kenku_import(&self.window);
                return;
            }
            shared.kenku.starting = true;
        }
        render::starting_kenku_import(&self.window);
        self.runtime
            .spawn(self.clone().run_kenku_import(client, request));
    }

    async fn run_kenku_import(self, client: TufoClient, request: ImportKenkuLibrary) {
        let token_was_imported = self.lock().kenku.current.as_ref().is_some_and(|status| {
            request.import_id == Some(status.id) && status.report.discord_bot_token_imported
        });
        let result = client.import_kenku_library(&request).await;
        {
            let mut shared = self.lock();
            shared.kenku.starting = false;
            if let Ok(id) = &result {
                shared.kenku.pending = Some(KenkuImportAttempt {
                    id: *id,
                    token_was_imported,
                });
            }
        }
        match result {
            Ok(_) => {
                // A tiny import can finish before the acknowledgement is scheduled.
                let current = self.lock().kenku.current.clone();
                if let Some(status) = current {
                    self.receive_kenku_import(status, &client).await;
                }
            }
            Err(error) => {
                self.toast_error(presentation::error_text(&error));
                if let Some(status) = self.lock().kenku.current.clone() {
                    render::kenku_import(&self.window, status, Vec::new());
                } else {
                    render::kenku_import_start_error(
                        &self.window,
                        presentation::error_text(&error),
                    );
                }
            }
        }
    }

    pub(super) async fn follow_kenku_import(self, client: TufoClient) {
        let mut updates = match client.watch_kenku_import().await {
            Ok(updates) => updates,
            Err(error) => {
                self.toast_error(presentation::error_text(&error));
                return;
            }
        };
        while let Some(update) = updates.next().await {
            match update {
                Ok(Some(status)) => self.receive_kenku_import(status, &client).await,
                Ok(None) => {}
                Err(error) => {
                    self.toast_error(presentation::error_text(&error));
                    return;
                }
            }
        }
    }

    async fn receive_kenku_import(&self, status: KenkuImportStatus, client: &TufoClient) {
        let (initiated, restart_for_token) = {
            let mut shared = self.lock();
            if shared.kenku.current.as_ref().is_none_or(|previous| {
                previous.id != status.id || previous.conflicts != status.conflicts
            }) {
                shared.kenku.choices = vec![false; status.conflicts.len()];
            }
            shared.kenku.current = Some(status.clone());
            render::kenku_import(&self.window, status.clone(), shared.kenku.choices.clone());
            let pending = shared
                .kenku
                .pending
                .as_ref()
                .filter(|pending| pending.id == status.id);
            let initiated = pending.is_some();
            let restart_for_token = pending.is_some_and(|pending| !pending.token_was_imported);
            if initiated
                && !status.state.is_running()
                && status.state != KenkuImportState::AwaitingChoices
            {
                shared.kenku.pending = None;
            }
            (initiated, restart_for_token)
        };
        if !initiated {
            return;
        }
        if status.state == KenkuImportState::Failed
            && status.error_reason.as_deref() == Some(ReadKenkuDataError::NOT_FOUND)
        {
            if let Some(kenku_folder) = self.pick_kenku_folder().await {
                self.submit_kenku_import(ImportKenkuLibrary {
                    kenku_folder: Some(kenku_folder),
                    ..ImportKenkuLibrary::default()
                });
            }
        } else if status.state == KenkuImportState::Succeeded
            && status.report.discord_bot_token_imported
            && restart_for_token
            && let Err(error) = client.stop_server().await
        {
            self.toast_error(presentation::error_text(&error));
        }
    }

    /// `None` after telling why.
    async fn pick_kenku_folder(&self) -> Option<FolderPath> {
        let Some(folder) = rfd::AsyncFileDialog::new()
            .set_title(tr("Choose Kenku FM's data folder"))
            .pick_folder()
            .await
        else {
            self.toast(tr("No Kenku FM library imported.").to_owned());
            return None;
        };
        let kenku_folder = FolderPath::try_new(folder.path().to_path_buf()).ok();
        if kenku_folder.is_none() {
            self.toast_error(tr("Can't read that folder's path.").to_owned());
        }
        kenku_folder
    }
}
