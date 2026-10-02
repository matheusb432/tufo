use error_meta::ErrorMeta;

#[derive(ErrorMeta)]
#[meta(crate = gtl_models)]
#[meta(crate = gtl_models)]
enum DuplicateFacade {
    #[meta(private(Internal))]
    Private,
}

#[derive(ErrorMeta)]
#[meta(crate = gtl_models, crate = gtl_models)]
enum DuplicateOption {
    #[meta(private(Internal))]
    Private,
}

#[derive(ErrorMeta)]
#[meta(crate =)]
enum MissingPath {
    #[meta(private(Internal))]
    Private,
}

#[derive(ErrorMeta)]
enum VariantFacade {
    #[meta(crate = gtl_models)]
    Private,
}

fn main() {}
