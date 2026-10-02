use error_meta::ErrorMeta;

#[derive(Debug, ErrorMeta)]
enum Missing {
    Unclassified,
    AlsoUnclassified(u8),
}

fn main() {}
