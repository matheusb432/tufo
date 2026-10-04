//! The loop editor's requests and previews, and song loop export and import.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use tufo_client::{
    TufoClient,
    library::LibrarySnapshot,
    song_loops::{
        ImportSongLoops, PreviewMode, PreviewSongLoop, SavedSongLoop, SongLoopImportOutcome,
        SongLoopImportResult, SongWaveform, UpdateSongLoop,
    },
};
use tufo_models::library::{
    AudioFileId,
    song_loop::{LoopRegion, SeamPreviewSide, SourceFrames},
};

use super::{Controller, audio_file_id};
use crate::{
    files,
    i18n::{tr, tr_args, tr_count},
    loop_editor::{
        LoopDraft, LoopEditorFrame, Marker, PreviewKind, WaveformView, format_milliseconds,
        format_time, parse_milliseconds, parse_time, preview_frame, seam_preview_frame,
    },
    presentation, render,
};

const PLAYHEAD_STEP_COARSE: Duration = Duration::from_secs(1);
const PLAYHEAD_STEP_FINE: Duration = Duration::from_millis(10);
const SEAM_PREVIEW_SIDE: Duration = Duration::from_secs(4);
const MARKER_VIEW_LENGTH: Duration = Duration::from_secs(4);
const SONG_LOOP_DOCUMENT_BYTES_MAX: u64 = 16 * 1024 * 1024;
const SONG_LOOP_DOCUMENT_NAME: &str = "song-loops.json";
const JSON_EXTENSIONS: [&str; 1] = ["json"];

#[derive(Default)]
pub(super) struct SongLoopSession {
    editor: Option<LoopEditor>,
    /// Rises with each editor and preview, so replies for earlier ones are dropped.
    last_generation: u64,
    import: Option<PendingImport>,
}

struct LoopEditor {
    generation: u64,
    audio_file_id: AudioFileId,
    song_title: String,
    saved: Option<SavedSongLoop>,
    song: Option<LoadedSong>,
    error: Option<String>,
    saving: bool,
    preview: Option<RunningPreview>,
    waveform_revision: i32,
}

struct LoadedSong {
    waveform: Arc<SongWaveform>,
    draft: LoopDraft,
    playhead: SourceFrames,
    view: WaveformView,
}

struct RunningPreview {
    generation: u64,
    kind: PreviewKind,
    region: LoopRegion,
    start: SourceFrames,
    /// Absent until the server starts playing.
    started_at: Option<SystemTime>,
    seam_cycle: Option<Duration>,
    /// Aborting it drops the stream, which stops the preview.
    task: tokio::task::JoinHandle<()>,
}

struct PendingImport {
    document: String,
    results: Vec<SongLoopImportResult>,
    replace: bool,
    busy: bool,
}

impl LoopEditor {
    fn frame(&self) -> LoopEditorFrame {
        let notice = if self.saved.is_some_and(|saved| saved.needs_review) {
            tr("The file changed since this loop was saved. Check the markers, then save to use the loop again.").to_owned()
        } else {
            String::new()
        };
        let mut frame = LoopEditorFrame {
            song_title: self.song_title.clone(),
            loading: self.song.is_none(),
            notice,
            error: self.error.clone().unwrap_or_default(),
            has_saved_loop: self.saved.is_some(),
            can_save: self.song.is_some() && !self.saving,
            waveform_revision: self.waveform_revision,
            preview: self
                .preview
                .as_ref()
                .map_or(PreviewKind::Stopped, |preview| preview.kind),
            view_visible: 1.0,
            ..LoopEditorFrame::default()
        };
        let Some(song) = &self.song else {
            return frame;
        };
        let timing = song.draft.timing();
        let markers = song.draft.markers();
        let view = song.view;
        frame.playback_start_text = format_time(markers.playback_start, timing);
        frame.loop_start_text = format_time(markers.loop_start, timing);
        frame.loop_end_text = format_time(markers.loop_end, timing);
        frame.crossfade_text = format_milliseconds(markers.crossfade, timing);
        frame.playhead_text = format_time(song.playhead, timing);
        frame.playback_start_x = view.fraction_of(markers.playback_start);
        frame.loop_start_x = view.fraction_of(markers.loop_start);
        frame.loop_end_x = view.fraction_of(markers.loop_end);
        frame.crossfade_start_x =
            view.fraction_of(markers.loop_end.saturating_sub(markers.crossfade));
        frame.playhead_x = view.fraction_of(song.playhead);
        frame.view_start = view.start_fraction();
        frame.view_visible = view.visible_fraction();
        frame
    }

    fn stop_preview(&mut self) {
        if let Some(preview) = self.preview.take() {
            preview.task.abort();
        }
    }
}

/// The song's title and saved loop, from the songs or any collection listing it.
fn song_details(
    snapshot: &LibrarySnapshot,
    id: AudioFileId,
) -> Option<(String, Option<SavedSongLoop>)> {
    let from_songs = snapshot
        .songs
        .iter()
        .find(|song| song.audio_file_id == id)
        .map(|song| (song.title.as_str().to_owned(), song.saved_loop));
    from_songs.or_else(|| {
        snapshot
            .collections
            .iter()
            .flat_map(|collection| &collection.entries)
            .find(|entry| entry.audio_file_id == id)
            .map(|entry| (entry.title.as_str().to_owned(), entry.saved_loop))
    })
}

fn outcome_text(outcome: SongLoopImportOutcome) -> &'static str {
    match outcome {
        SongLoopImportOutcome::Saved => tr("New loop"),
        SongLoopImportOutcome::Replaced => tr("Replaces the saved loop"),
        SongLoopImportOutcome::Unchanged => tr("Already saved"),
        SongLoopImportOutcome::Conflict => tr("Has another loop"),
        SongLoopImportOutcome::NoMatch => tr("No matching song"),
        SongLoopImportOutcome::IdentityMismatch => tr("Different audio"),
        SongLoopImportOutcome::LengthMismatch => tr("Different length"),
        SongLoopImportOutcome::InvalidMarkers => tr("Invalid markers"),
        SongLoopImportOutcome::Unreadable => tr("Unreadable file"),
    }
}

fn outcome_failed(outcome: SongLoopImportOutcome) -> bool {
    matches!(
        outcome,
        SongLoopImportOutcome::NoMatch
            | SongLoopImportOutcome::IdentityMismatch
            | SongLoopImportOutcome::LengthMismatch
            | SongLoopImportOutcome::InvalidMarkers
            | SongLoopImportOutcome::Unreadable
    )
}

fn count(results: &[SongLoopImportResult], outcome: SongLoopImportOutcome) -> usize {
    results
        .iter()
        .filter(|result| result.outcome == outcome)
        .count()
}

impl PendingImport {
    fn saves(&self) -> usize {
        let conflicts = if self.replace {
            count(&self.results, SongLoopImportOutcome::Conflict)
        } else {
            0
        };
        count(&self.results, SongLoopImportOutcome::Saved)
            + count(&self.results, SongLoopImportOutcome::Replaced)
            + conflicts
    }

    fn summary(&self) -> String {
        if self.busy {
            return tr("Checking each song’s audio…").to_owned();
        }
        let failed = self
            .results
            .iter()
            .filter(|result| outcome_failed(result.outcome))
            .count();
        let mut sentences = vec![tr_count(
            "{n} loop will be saved.",
            "{n} loops will be saved.",
            self.saves(),
        )];
        if failed > 0 {
            sentences.push(tr_count(
                "{n} loop matches no song and will be skipped.",
                "{n} loops match no song and will be skipped.",
                failed,
            ));
        }
        sentences.join(" ")
    }
}

impl Controller {
    pub(crate) fn edit_song_loop(&self, song_id: &str) {
        let Some(id) = audio_file_id(song_id) else {
            return;
        };
        let generation = {
            let mut shared = self.lock();
            let Some((song_title, saved)) = song_details(&shared.snapshot, id) else {
                return;
            };
            let session = &mut shared.song_loops;
            if let Some(editor) = &mut session.editor {
                editor.stop_preview();
            }
            session.last_generation += 1;
            session.editor = Some(LoopEditor {
                generation: session.last_generation,
                audio_file_id: id,
                song_title,
                saved,
                song: None,
                error: None,
                saving: false,
                preview: None,
                waveform_revision: 0,
            });
            session.last_generation
        };
        self.render_loop_editor();
        let Some(client) = self.client() else {
            self.set_loop_editor_error(generation, tr("tufo-server is not connected.").to_owned());
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            let loaded = client.get_song_waveform(id).await;
            controller.finish_loading_song(
                generation,
                loaded.map_err(|error| presentation::error_text(&error)),
            );
        });
    }

    fn finish_loading_song(&self, generation: u64, loaded: Result<SongWaveform, String>) {
        {
            let mut shared = self.lock();
            let Some(editor) = shared
                .song_loops
                .editor
                .as_mut()
                .filter(|editor| editor.generation == generation)
            else {
                return;
            };
            match loaded {
                Ok(waveform) => {
                    let draft = LoopDraft::new(
                        waveform.timing,
                        editor.saved.map(|saved| saved.region.markers()),
                    );
                    editor.song = Some(LoadedSong {
                        playhead: draft.markers().playback_start,
                        view: WaveformView::whole(waveform.timing.frame_count),
                        waveform: Arc::new(waveform),
                        draft,
                    });
                    editor.waveform_revision += 1;
                }
                Err(error) => editor.error = Some(error),
            }
        }
        self.render_loop_editor();
    }

    fn set_loop_editor_error(&self, generation: u64, error: String) {
        if let Some(editor) = self
            .lock()
            .song_loops
            .editor
            .as_mut()
            .filter(|editor| editor.generation == generation)
        {
            editor.error = Some(error);
            editor.saving = false;
        }
        self.render_loop_editor();
    }

    pub(crate) fn close_loop_editor(&self) {
        if let Some(mut editor) = self.lock().song_loops.editor.take() {
            editor.stop_preview();
        }
    }

    pub(crate) fn drag_loop_marker(&self, marker: Marker, fraction: f32) {
        self.edit_loaded_song(false, |song| {
            let frame = song.view.frame_at(fraction);
            song.draft.move_marker(marker, frame);
            Ok(())
        });
    }

    pub(crate) fn type_loop_marker(&self, marker: Marker, text: &str) {
        self.edit_loaded_song(false, |song| {
            let frame = parse_time(text, song.draft.timing())
                .ok_or_else(|| tr("Enter a time like 1:23.456.").to_owned())?;
            if frame != song.draft.frame_of(marker) {
                song.draft.move_marker(marker, frame);
            }
            Ok(())
        });
    }

    pub(crate) fn type_loop_crossfade(&self, text: &str) {
        self.edit_loaded_song(false, |song| {
            let frames = parse_milliseconds(text, song.draft.timing())
                .ok_or_else(|| tr("Enter the crossfade in milliseconds.").to_owned())?;
            song.draft.set_crossfade(frames);
            Ok(())
        });
    }

    pub(crate) fn set_loop_playhead(&self, fraction: f32) {
        self.edit_loaded_song(false, |song| {
            song.playhead = song.view.frame_at(fraction);
            Ok(())
        });
        self.restart_playhead_preview();
    }

    pub(crate) fn step_loop_playhead(&self, forward: bool, fine: bool) {
        let step = if fine {
            PLAYHEAD_STEP_FINE
        } else {
            PLAYHEAD_STEP_COARSE
        };
        self.edit_loaded_song(true, |song| {
            let step = song.draft.timing().frames_in(step);
            song.playhead = if forward {
                song.playhead
                    .saturating_add(step)
                    .min(song.draft.timing().frame_count)
            } else {
                song.playhead.saturating_sub(step)
            };
            if !song.view.contains(song.playhead) {
                song.view.center_on(song.playhead);
            }
            Ok(())
        });
        self.restart_playhead_preview();
    }

    pub(crate) fn place_loop_marker_at_playhead(&self, marker: Marker) {
        self.edit_loaded_song(false, |song| {
            song.draft.move_marker(marker, song.playhead);
            Ok(())
        });
    }

    pub(crate) fn show_loop_marker(&self, marker: Marker) {
        self.edit_loaded_song(true, |song| {
            song.playhead = song.draft.frame_of(marker);
            let length = song.draft.timing().frames_in(MARKER_VIEW_LENGTH);
            song.view.show_around(song.playhead, length);
            Ok(())
        });
    }

    pub(crate) fn zoom_loop_view(&self, zoom_in: bool) {
        self.edit_loaded_song(true, |song| {
            song.view.zoom(zoom_in, song.playhead);
            Ok(())
        });
    }

    pub(crate) fn scroll_loop_view(&self, fraction: f32) {
        self.edit_loaded_song(true, |song| {
            song.view.scroll_to(fraction);
            Ok(())
        });
    }

    /// Applies `edit` to the loaded song, showing its error; `moves_view` redraws the waveform.
    fn edit_loaded_song(
        &self,
        moves_view: bool,
        edit: impl FnOnce(&mut LoadedSong) -> Result<(), String>,
    ) {
        {
            let mut shared = self.lock();
            let Some(editor) = shared.song_loops.editor.as_mut() else {
                return;
            };
            let Some(song) = editor.song.as_mut() else {
                return;
            };
            editor.error = edit(song).err();
            if moves_view {
                editor.waveform_revision += 1;
            }
        }
        self.render_loop_editor();
    }

    pub(crate) fn toggle_loop_preview(&self, kind: PreviewKind) {
        let running = self
            .lock()
            .song_loops
            .editor
            .as_ref()
            .and_then(|editor| editor.preview.as_ref().map(|preview| preview.kind));
        if running == Some(kind) || kind == PreviewKind::Stopped {
            self.stop_loop_preview();
        } else {
            self.start_loop_preview(kind);
        }
    }

    fn restart_playhead_preview(&self) {
        let playing_from_playhead = self
            .lock()
            .song_loops
            .editor
            .as_ref()
            .and_then(|editor| editor.preview.as_ref())
            .is_some_and(|preview| preview.kind == PreviewKind::FromPlayhead);
        if playing_from_playhead {
            self.start_loop_preview(PreviewKind::FromPlayhead);
        }
    }

    fn stop_loop_preview(&self) {
        if let Some(editor) = self.lock().song_loops.editor.as_mut() {
            editor.stop_preview();
        }
        self.render_loop_editor();
    }

    fn start_loop_preview(&self, kind: PreviewKind) {
        let Some(client) = self.client() else {
            return;
        };
        {
            let mut shared = self.lock();
            let session = &mut shared.song_loops;
            session.last_generation += 1;
            let generation = session.last_generation;
            let Some(editor) = session.editor.as_mut() else {
                return;
            };
            let Some(song) = editor.song.as_ref() else {
                return;
            };
            let Ok(region) = song.draft.region() else {
                editor.error = Some(
                    tr("Place Start, Loop, and End in that order inside the song.").to_owned(),
                );
                drop(shared);
                self.render_loop_editor();
                return;
            };
            let mode = match kind {
                PreviewKind::Seam => match SeamPreviewSide::try_new(SEAM_PREVIEW_SIDE) {
                    Ok(side) => PreviewMode::Seam(side),
                    Err(_) => return,
                },
                PreviewKind::FromPlayhead | PreviewKind::Stopped => {
                    PreviewMode::FromFrame(song.playhead)
                }
            };
            let request = PreviewSongLoop {
                audio_file_id: editor.audio_file_id,
                source_fingerprint: song.waveform.fingerprint.clone(),
                draft: region,
                mode,
            };
            let start = song.playhead;
            let controller = self.clone();
            let task = self.runtime.spawn(async move {
                controller
                    .run_loop_preview(generation, client, request)
                    .await;
            });
            editor.stop_preview();
            editor.error = None;
            editor.preview = Some(RunningPreview {
                generation,
                kind,
                region,
                start,
                started_at: None,
                seam_cycle: None,
                task,
            });
        }
        self.render_loop_editor();
    }

    async fn run_loop_preview(
        &self,
        generation: u64,
        client: TufoClient,
        request: PreviewSongLoop,
    ) {
        let mut preview = match client.preview_song_loop(&request).await {
            Ok(preview) => preview,
            Err(error) => {
                self.end_loop_preview(generation, Some(presentation::error_text(&error)));
                return;
            }
        };
        if let Some(running) = self
            .lock()
            .song_loops
            .editor
            .as_mut()
            .and_then(|editor| editor.preview.as_mut())
            .filter(|running| running.generation == generation)
        {
            running.started_at = Some(preview.started.started_at);
            running.seam_cycle = preview.started.seam_cycle;
        }
        preview.ended().await;
        self.end_loop_preview(generation, None);
    }

    fn end_loop_preview(&self, generation: u64, error: Option<String>) {
        {
            let mut shared = self.lock();
            let Some(editor) = shared.song_loops.editor.as_mut() else {
                return;
            };
            if editor
                .preview
                .as_ref()
                .is_none_or(|preview| preview.generation != generation)
            {
                return;
            }
            editor.preview = None;
            if error.is_some() {
                editor.error = error;
            }
        }
        self.render_loop_editor();
    }

    /// Moves the playhead with a playing preview. Runs on the UI thread.
    pub(crate) fn tick_loop_editor(&self) {
        {
            let mut shared = self.lock();
            let Some(editor) = shared.song_loops.editor.as_mut() else {
                return;
            };
            let (Some(preview), Some(song)) = (editor.preview.as_ref(), editor.song.as_mut())
            else {
                return;
            };
            let Some(started_at) = preview.started_at else {
                return;
            };
            let elapsed = SystemTime::now()
                .duration_since(started_at)
                .unwrap_or_default();
            song.playhead = match (preview.kind, preview.seam_cycle) {
                (PreviewKind::Seam, Some(cycle)) => seam_preview_frame(
                    preview.region,
                    song.draft.timing().frames_in(SEAM_PREVIEW_SIDE),
                    cycle,
                    elapsed,
                ),
                _ => preview_frame(preview.region, preview.start, elapsed),
            };
            if preview.kind == PreviewKind::FromPlayhead && !song.view.contains(song.playhead) {
                song.view.center_on(song.playhead);
                editor.waveform_revision += 1;
            }
        }
        self.render_loop_editor();
    }

    pub(crate) fn save_song_loop(&self) {
        let request = {
            let mut shared = self.lock();
            let Some(editor) = shared.song_loops.editor.as_mut() else {
                return;
            };
            if editor.error.is_some() {
                return;
            }
            let Some(song) = editor.song.as_ref() else {
                return;
            };
            if song.draft.region().is_err() {
                editor.error = Some(
                    tr("Place Start, Loop, and End in that order inside the song.").to_owned(),
                );
                drop(shared);
                self.render_loop_editor();
                return;
            }
            let markers = song.draft.markers();
            editor.saving = true;
            let request = UpdateSongLoop {
                audio_file_id: editor.audio_file_id,
                source_fingerprint: song.waveform.fingerprint.clone(),
                playback_start: Some(markers.playback_start),
                loop_start: Some(markers.loop_start),
                loop_end: Some(markers.loop_end),
                crossfade: Some(markers.crossfade),
            };
            (editor.generation, editor.song_title.clone(), request)
        };
        self.render_loop_editor();
        let (generation, title, update) = request;
        self.finish_loop_request(
            generation,
            move |client| async move { client.update_song_loop(&update).await },
            tr_args("Saved the loop for {0}.", &[&title]),
        );
    }

    pub(crate) fn delete_song_loop(&self) {
        let request = self.lock().song_loops.editor.as_mut().map(|editor| {
            editor.saving = true;
            (
                editor.generation,
                editor.audio_file_id,
                editor.song_title.clone(),
            )
        });
        let Some((generation, id, title)) = request else {
            return;
        };
        self.render_loop_editor();
        self.finish_loop_request(
            generation,
            move |client| async move { client.delete_song_loop(id).await },
            tr_args("Removed the loop from {0}.", &[&title]),
        );
    }

    /// Closes the editor once `command` succeeds, or shows its failure there.
    fn finish_loop_request<Command, Request>(&self, generation: u64, command: Command, done: String)
    where
        Command: FnOnce(TufoClient) -> Request + Send + 'static,
        Request: std::future::Future<Output = Result<(), tufo_client::ClientError>> + Send,
    {
        let Some(client) = self.client() else {
            self.set_loop_editor_error(generation, tr("tufo-server is not connected.").to_owned());
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            match command(client).await {
                Ok(()) => controller.close_finished_loop_editor(generation, done),
                Err(error) => {
                    controller.set_loop_editor_error(generation, presentation::error_text(&error));
                }
            }
        });
    }

    fn close_finished_loop_editor(&self, generation: u64, done: String) {
        let finished = self
            .lock()
            .song_loops
            .editor
            .as_ref()
            .is_some_and(|editor| editor.generation == generation);
        if finished {
            self.close_loop_editor();
            render::loop_editor(&self.window, None);
        }
        self.toast(done);
    }

    fn render_loop_editor(&self) {
        let frame = self
            .lock()
            .song_loops
            .editor
            .as_ref()
            .map(LoopEditor::frame);
        render::loop_editor(&self.window, frame);
    }

    /// The visible peaks at the window's scale, or an empty image before the song loads.
    pub(crate) fn loop_waveform_image(
        &self,
        size: (u32, u32),
        color: slint::Color,
    ) -> slint::Image {
        let shared = self.lock();
        let Some(song) = shared
            .song_loops
            .editor
            .as_ref()
            .and_then(|editor| editor.song.as_ref())
        else {
            return slint::Image::default();
        };
        let pixel = slint::Rgba8Pixel::new(color.red(), color.green(), color.blue(), color.alpha());
        slint::Image::from_rgba8_premultiplied(crate::loop_editor::render_waveform(
            &song.waveform.peaks,
            song.waveform.bucket_frames.get(),
            song.view,
            size,
            premultiplied(pixel),
        ))
    }

    // Export and import.

    pub(crate) fn export_song_loops(&self) {
        let Some(client) = self.client() else {
            self.toast_error(tr("tufo-server is not connected.").to_owned());
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(tr("Export song loops"))
                .set_file_name(SONG_LOOP_DOCUMENT_NAME)
                .add_filter(tr("Song loops"), &JSON_EXTENSIONS)
                .save_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            let document = match client.export_song_loops().await {
                Ok(document) => document,
                Err(error) => {
                    controller.toast_error(presentation::error_text(&error));
                    return;
                }
            };
            match files::write_atomically(&path, &document) {
                Ok(()) => controller.toast(tr_args(
                    "Exported the song loops to {0}.",
                    &[&path.display().to_string()],
                )),
                Err(_) => controller.toast_error(tr("Could not write that file.").to_owned()),
            }
        });
    }

    pub(crate) fn import_song_loops(&self) {
        let Some(client) = self.client() else {
            self.toast_error(tr("tufo-server is not connected.").to_owned());
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(tr("Import song loops"))
                .add_filter(tr("Song loops"), &JSON_EXTENSIONS)
                .pick_file()
                .await
            else {
                return;
            };
            let Ok(document) = read_document(file.path().to_path_buf()) else {
                controller.toast_error(tr("Could not read that file.").to_owned());
                return;
            };
            controller.lock().song_loops.import = Some(PendingImport {
                document: document.clone(),
                results: Vec::new(),
                replace: false,
                busy: true,
            });
            controller.render_song_loop_import();
            let checked = client
                .import_song_loops(&ImportSongLoops {
                    document,
                    dry_run: true,
                    replace_existing: false,
                })
                .await;
            controller.show_song_loop_check(checked);
        });
    }

    fn show_song_loop_check(
        &self,
        checked: Result<Vec<SongLoopImportResult>, tufo_client::ClientError>,
    ) {
        match checked {
            Ok(results) => {
                if let Some(import) = self.lock().song_loops.import.as_mut() {
                    import.results = results;
                    import.busy = false;
                }
                self.render_song_loop_import();
            }
            Err(error) => {
                self.lock().song_loops.import = None;
                render::song_loop_import(&self.window, None);
                self.toast_error(presentation::error_text(&error));
            }
        }
    }

    pub(crate) fn set_song_loop_import_replace(&self, replace: bool) {
        if let Some(import) = self.lock().song_loops.import.as_mut() {
            import.replace = replace;
        }
        self.render_song_loop_import();
    }

    pub(crate) fn confirm_song_loop_import(&self) {
        let request = self.lock().song_loops.import.as_mut().map(|import| {
            import.busy = true;
            ImportSongLoops {
                document: import.document.clone(),
                dry_run: false,
                replace_existing: import.replace,
            }
        });
        let Some(request) = request else {
            return;
        };
        self.render_song_loop_import();
        let Some(client) = self.client() else {
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            let imported = client.import_song_loops(&request).await;
            controller.lock().song_loops.import = None;
            render::song_loop_import(&controller.window, None);
            match imported {
                Ok(results) => {
                    let saved = count(&results, SongLoopImportOutcome::Saved)
                        + count(&results, SongLoopImportOutcome::Replaced);
                    controller.toast(tr_count(
                        "Saved {n} song loop.",
                        "Saved {n} song loops.",
                        saved,
                    ));
                }
                Err(error) => controller.toast_error(presentation::error_text(&error)),
            }
        });
    }

    pub(crate) fn close_song_loop_import(&self) {
        self.lock().song_loops.import = None;
    }

    fn render_song_loop_import(&self) {
        let view =
            self.lock()
                .song_loops
                .import
                .as_ref()
                .map(|import| presentation::SongLoopImportView {
                    busy: import.busy,
                    summary: import.summary(),
                    rows: import
                        .results
                        .iter()
                        .map(|result| presentation::SongLoopImportRowView {
                            path: result.path.clone(),
                            outcome: outcome_text(result.outcome).to_owned(),
                            failed: outcome_failed(result.outcome),
                            conflict: result.outcome == SongLoopImportOutcome::Conflict,
                        })
                        .collect(),
                    conflicts: count(&import.results, SongLoopImportOutcome::Conflict),
                    replace: import.replace,
                    can_import: import.saves() > 0,
                });
        render::song_loop_import(&self.window, view);
    }
}

fn read_document(path: PathBuf) -> std::io::Result<String> {
    use std::io::Read as _;

    let mut document = String::new();
    std::fs::File::open(path)?
        .take(SONG_LOOP_DOCUMENT_BYTES_MAX)
        .read_to_string(&mut document)?;
    Ok(document)
}

fn premultiplied(pixel: slint::Rgba8Pixel) -> slint::Rgba8Pixel {
    let scale = |channel: u8| {
        u8::try_from(u16::from(channel) * u16::from(pixel.a) / 255).unwrap_or(u8::MAX)
    };
    slint::Rgba8Pixel::new(scale(pixel.r), scale(pixel.g), scale(pixel.b), pixel.a)
}
