use error_meta::ErrorMeta;

#[derive(Debug, ErrorMeta)]
enum Invalid {
    #[meta(private(Teapot))]
    UnknownClass,
    #[meta(failure)]
    FailureWithoutField,
    #[meta(transparent)]
    TransparentWithTwoFields(u8, u8),
    #[meta(public)]
    UnknownKind,
    #[meta(private(Internal))]
    #[meta(private(Internal))]
    Duplicate,
}

fn main() {}
