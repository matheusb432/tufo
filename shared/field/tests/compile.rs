#[test]
fn enforces_the_external_compile_time_contract() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/visible_field.rs");
    tests.compile_fail("tests/ui/fail/missing_field.rs");
}
