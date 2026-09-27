use field::field;

struct Record {
    value: u32,
}

const VALUE_FIELD: &str = field!(value @ Record);

fn main() {
    assert_eq!(VALUE_FIELD, "value");
}
