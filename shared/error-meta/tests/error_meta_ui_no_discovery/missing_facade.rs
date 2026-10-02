use error_meta::ErrorMeta;

#[derive(ErrorMeta)]
enum MissingFacade {
    #[meta(private(Internal))]
    Internal,
}

fn main() {}
