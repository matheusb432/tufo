use error_meta::ErrorMeta;

#[derive(ErrorMeta)]
union Unsupported {
    value: u8,
}

fn main() {}
