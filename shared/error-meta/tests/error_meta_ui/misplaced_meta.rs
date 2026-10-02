use error_meta::ErrorMeta;

#[derive(ErrorMeta)]
#[meta(private(Internal))]
enum Misplaced {
    #[meta(private(Internal))]
    Private {
        #[meta(failure)]
        reason: String,
        #[meta(unknown)]
        context: String,
    },
    #[meta(private(Internal))]
    Tuple(#[meta(transparent)] String),
}

fn main() {}
