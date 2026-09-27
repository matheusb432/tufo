use field::field;

struct Record {
    value: u32,
}

fn main() {
    let _ = field!(missing @ Record);
}
