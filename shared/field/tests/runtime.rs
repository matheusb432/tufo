use field::field;

struct User {
    name: String,
    age: u32,
}

#[test]
fn returns_verified_field_names_to_an_external_consumer() {
    let user = User {
        name: String::new(),
        age: 42,
    };

    assert!(user.name.is_empty());
    assert_eq!(user.age, 42);
    assert_eq!(field!(name @ User), "name");
    assert_eq!(field!(age @ User), "age");
}
