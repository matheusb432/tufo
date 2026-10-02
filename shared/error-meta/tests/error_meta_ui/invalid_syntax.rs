use error_meta::ErrorMeta;

#[derive(ErrorMeta)]
enum InvalidSyntax {
    #[meta()]
    Empty,
    #[meta(private(Internal), transparent)]
    Multiple,
    #[meta(failure =)]
    MissingReason,
    #[meta(private(Internal, Unavailable))]
    MultipleClasses,
}

fn main() {}
