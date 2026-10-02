#[test]
#[cfg_attr(
    windows,
    ignore = "compiler diagnostic fixtures run on the Ubuntu development host"
)]
fn invalid_declarations_fail_to_compile_with_spanned_diagnostics() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/error_meta_ui/*.rs");
    tests.compile_fail("tests/application_error_meta_ui/*.rs");
    #[cfg(not(feature = "crate-discovery"))]
    tests.compile_fail("tests/error_meta_ui_no_discovery/*.rs");
}

#[test]
fn valid_declarations_compile_without_git_tools() {
    trybuild::TestCases::new().pass("tests/error_meta_ui_pass/*.rs");
}
