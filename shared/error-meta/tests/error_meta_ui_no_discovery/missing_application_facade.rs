use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(class = Internal, reason = "missing.facade")]
struct MissingFacade;

fn main() {}
