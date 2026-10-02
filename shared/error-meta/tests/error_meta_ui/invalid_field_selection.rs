use error_meta::ErrorMeta;

struct Failure;

#[derive(ErrorMeta)]
enum InvalidSelection {
    #[meta(failure(field = missing))]
    Missing { reason: Failure },
    #[meta(transparent(field = 2))]
    OutOfBounds(Failure, String),
    #[meta(failure(field = reason))]
    NamedOnTuple(Failure),
    #[meta(failure(field = 0))]
    TupleOnNamed { reason: Failure },
    #[meta(failure(field = 0))]
    Unit,
    #[meta(failure(field = reason, field = reason))]
    Duplicate { reason: Failure },
    #[meta(transparent(unknown = reason))]
    Unknown { reason: Failure },
    #[meta(failure())]
    Empty(Failure),
}

fn main() {}
