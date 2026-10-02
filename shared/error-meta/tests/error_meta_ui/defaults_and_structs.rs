use error_meta::ErrorMeta;

#[derive(ErrorMeta)]
#[meta(private(Internal))]
#[meta(private(Unavailable))]
enum DuplicateDefault {
    #[meta(private(Internal))]
    Explicit,
}

#[derive(ErrorMeta)]
#[meta(transparent)]
enum InvalidDelegation {
    Unit,
    Multiple(u8, u8),
}

#[derive(ErrorMeta)]
#[meta(transparent(field = source))]
enum InvalidSelection {
    Missing { context: u8 },
    Tuple(u8),
}

#[derive(ErrorMeta)]
struct Missing;

#[derive(ErrorMeta)]
#[meta(failure)]
struct NoField;

#[derive(ErrorMeta)]
#[meta(transparent(field = missing))]
struct WrongField {
    source: u8,
}

#[derive(ErrorMeta)]
#[meta(private(Internal))]
struct FieldMetadata(#[meta(transparent)] u8);

#[derive(ErrorMeta)]
#[meta(private(Unknown))]
enum UnusedInvalidDefault {
    #[meta(private(Internal))]
    Explicit,
}

fn main() {}
