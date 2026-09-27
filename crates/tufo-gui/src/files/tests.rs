use super::*;

#[test]
fn writing_creates_the_folder_and_leaves_no_staged_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tufo").join("window.toml");

    write_atomically(&path, "first").unwrap();
    write_atomically(&path, "second").unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    assert!(!staged_path(&path).exists());
}
