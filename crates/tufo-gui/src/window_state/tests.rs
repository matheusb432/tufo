use super::*;

fn window_state_file(directory: &tempfile::TempDir) -> WindowStateFile {
    WindowStateFile::at(directory.path().join("tufo").join(WINDOW_STATE_FILE_NAME))
}

const COLLAPSED: WindowState = WindowState {
    drawer_pinned: false,
    player_collapsed: true,
};

#[test]
fn a_missing_file_pins_the_drawer() {
    let directory = tempfile::tempdir().unwrap();

    let state = window_state_file(&directory).load();

    assert!(state.drawer_pinned);
}

#[test]
fn a_saved_layout_loads_back() {
    let directory = tempfile::tempdir().unwrap();
    let file = window_state_file(&directory);

    file.save(COLLAPSED).unwrap();
    assert_eq!(file.load(), COLLAPSED);
    file.save(WindowState::default()).unwrap();
    assert_eq!(file.load(), WindowState::default());
}

#[test]
fn a_layout_saved_before_the_player_collapsed_keeps_the_drawer() {
    let directory = tempfile::tempdir().unwrap();
    let file = window_state_file(&directory);
    file.save(COLLAPSED).unwrap();
    std::fs::write(&file.path, "drawer_pinned = false\n").unwrap();

    let state = file.load();

    assert!(!state.drawer_pinned);
    assert!(!state.player_collapsed);
}

#[test]
fn an_unreadable_file_pins_the_drawer() {
    let directory = tempfile::tempdir().unwrap();
    let file = window_state_file(&directory);
    file.save(COLLAPSED).unwrap();
    std::fs::write(&file.path, "drawer_pinned = maybe\n").unwrap();

    assert_eq!(file.load(), WindowState::default());
}
