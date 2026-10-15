//! Playing music queues and sound effects on the one active output.

pub mod add_music_layer;
pub mod add_to_queue;
pub mod close_browser;
pub mod control_browser;
pub mod control_music;
pub mod control_sounds;
pub mod get_diagnostics;
pub mod open_browser;
pub mod play_collection;
pub mod play_entry_sound;
pub mod play_queue;
pub mod play_sound;
pub mod player;
mod playlist_song;
pub mod preview_song_loop;
pub mod recover_local_output;
pub mod remove_from_queue;
pub mod resume_saved_loops;
mod saved_loops;
pub mod select_output;
pub mod stand_by;
pub mod stop_music_layer;
pub mod stop_sound;
pub mod update_music_layer;
pub mod update_player_settings;
pub mod update_sound;
pub mod watch_player;
