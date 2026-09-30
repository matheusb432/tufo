use super::*;

fn size_limits() -> WindowSizeLimits {
    WindowSizeLimits {
        full_minimum: WindowSize {
            width: 440.0,
            height: 380.0,
        },
        mini_player_minimum: WindowSize {
            width: 320.0,
            height: 168.0,
        },
        mini_player_maximum: WindowSize {
            width: 480.0,
            height: 168.0,
        },
    }
}

fn window_state_file(directory: &tempfile::TempDir) -> WindowStateFile {
    WindowStateFile::at(directory.path().join("tufo").join(WINDOW_STATE_FILE_NAME))
}

fn collapsed() -> WindowState {
    WindowState {
        drawer_pinned: false,
        player_collapsed: true,
        ..WindowState::default()
    }
}

fn mini_player() -> WindowState {
    WindowState {
        form: WindowForm::MiniPlayer,
        mini_player_pinned: false,
        full_size: WindowSize {
            width: 1200.0,
            height: 820.0,
        },
        mini_player_size: WindowSize {
            width: 400.0,
            height: 168.0,
        },
        ..WindowState::default()
    }
}

#[test]
fn a_missing_file_pins_the_drawer() {
    let directory = tempfile::tempdir().unwrap();

    let state = window_state_file(&directory).load(size_limits());

    assert!(state.drawer_pinned);
}

#[test]
fn a_saved_layout_loads_back() {
    let directory = tempfile::tempdir().unwrap();
    let file = window_state_file(&directory);

    file.save(collapsed()).unwrap();
    assert_eq!(file.load(size_limits()), collapsed());
    file.save(mini_player()).unwrap();
    assert_eq!(file.load(size_limits()), mini_player());
    file.save(WindowState::default()).unwrap();
    assert_eq!(file.load(size_limits()), WindowState::default());
}

#[test]
fn a_layout_saved_before_the_player_collapsed_keeps_the_drawer() {
    let directory = tempfile::tempdir().unwrap();
    let file = window_state_file(&directory);
    file.save(collapsed()).unwrap();
    std::fs::write(&file.path, "drawer_pinned = false\n").unwrap();

    let state = file.load(size_limits());

    assert!(!state.drawer_pinned);
    assert!(!state.player_collapsed);
}

#[test]
fn an_unreadable_file_pins_the_drawer() {
    let directory = tempfile::tempdir().unwrap();
    let file = window_state_file(&directory);
    file.save(collapsed()).unwrap();
    std::fs::write(&file.path, "drawer_pinned = maybe\n").unwrap();

    assert_eq!(file.load(size_limits()), WindowState::default());
}

#[test]
fn the_mini_player_and_the_full_window_each_return_to_their_own_size() {
    let mut state = WindowState::default();
    let full_size = WindowSize {
        width: 1100.0,
        height: 760.0,
    };
    state.record_size(full_size, size_limits());

    let mini_player_size = state.switch_form(WindowForm::MiniPlayer).unwrap();
    state.record_size(
        WindowSize {
            width: 340.0,
            height: 168.0,
        },
        size_limits(),
    );

    assert_eq!(mini_player_size, WindowState::default().mini_player_size);
    assert_eq!(state.switch_form(WindowForm::Full), Some(full_size));
    assert_eq!(
        state.switch_form(WindowForm::MiniPlayer),
        Some(WindowSize {
            width: 340.0,
            height: 168.0,
        })
    );
}

#[test]
fn switching_to_the_form_already_shown_changes_nothing() {
    let mut state = WindowState::default();

    assert_eq!(state.switch_form(WindowForm::Full), None);
    assert_eq!(state, WindowState::default());
}

#[test]
fn the_mini_player_refuses_a_full_window_size() {
    let mut state = mini_player();

    let taken_back = state.record_size(
        WindowSize {
            width: 1200.0,
            height: 820.0,
        },
        size_limits(),
    );

    assert_eq!(taken_back, Some(mini_player().mini_player_size));
    assert_eq!(state, mini_player());
}

#[test]
fn saved_sizes_outside_the_current_limits_are_repaired() {
    let directory = tempfile::tempdir().unwrap();
    let file = window_state_file(&directory);
    let state = WindowState {
        full_size: WindowSize {
            width: 1.0,
            height: 1.0,
        },
        mini_player_size: WindowSize {
            width: 720.0,
            height: 400.0,
        },
        ..mini_player()
    };
    file.save(state).unwrap();

    assert_eq!(
        file.load(size_limits()),
        WindowState {
            full_size: WindowSize {
                width: 440.0,
                height: 380.0
            },
            mini_player_size: WindowSize {
                width: 480.0,
                height: 168.0
            },
            ..state
        }
    );
}

#[test]
fn non_finite_saved_dimensions_use_the_default() {
    let directory = tempfile::tempdir().unwrap();
    let file = window_state_file(&directory);
    file.save(WindowState {
        full_size: WindowSize {
            width: f32::NAN,
            height: f32::INFINITY,
        },
        mini_player_size: WindowSize {
            width: f32::NEG_INFINITY,
            height: f32::NAN,
        },
        ..WindowState::default()
    })
    .unwrap();

    assert_eq!(file.load(size_limits()), WindowState::default());
}

#[test]
fn the_full_window_corrects_and_remembers_a_size_below_its_minimum() {
    let mut state = WindowState::default();

    let correction = state.record_size(
        WindowSize {
            width: 1.0,
            height: 1.0,
        },
        size_limits(),
    );

    assert_eq!(
        correction,
        Some(WindowSize {
            width: 440.0,
            height: 380.0
        })
    );
    state.switch_form(WindowForm::MiniPlayer);
    assert_eq!(state.switch_form(WindowForm::Full), correction);
}

#[test]
fn the_mini_player_resizes_its_width_with_a_fixed_height() {
    let mut state = mini_player();
    for width in [320.0, 480.0] {
        let size = WindowSize {
            width,
            height: 168.0,
        };
        assert_eq!(state.record_size(size, size_limits()), None);
        assert_eq!(state.size_shown(), size);
    }

    let correction = state.record_size(
        WindowSize {
            width: 1.0,
            height: 1.0,
        },
        size_limits(),
    );
    assert_eq!(
        correction,
        Some(WindowSize {
            width: 320.0,
            height: 168.0
        })
    );

    for size in [
        WindowSize {
            width: 320.0,
            height: 400.0,
        },
        WindowSize {
            width: 720.0,
            height: 168.0,
        },
    ] {
        assert_eq!(state.record_size(size, size_limits()), correction);
        assert_eq!(Some(state.size_shown()), correction);
    }
}
