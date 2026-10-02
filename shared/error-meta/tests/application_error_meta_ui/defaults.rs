use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(class = Internal)]
#[meta(reason = "duplicate")]
enum DuplicateDefaults {
    #[meta(class = Internal, reason = "explicit")]
    Explicit,
}

#[derive(ApplicationErrorMeta)]
#[meta(class = Internal)]
enum MissingReason {
    Inherited,
}

#[derive(ApplicationErrorMeta)]
#[meta(reason = "default")]
enum MissingClass {
    Inherited,
}

#[derive(ApplicationErrorMeta)]
#[meta(transparent)]
enum TransparentDefault {
    Unit,
    Multiple(u8, u8),
    #[meta(reason = "cannot inherit a class from transparent")]
    IncompleteOverride,
}

#[derive(ApplicationErrorMeta)]
#[meta(transparent(field = source))]
enum MissingSelection {
    Inherited { context: u8 },
}

#[derive(ApplicationErrorMeta)]
#[meta(class = Unknown)]
enum UnusedInvalidDefault {
    #[meta(class = Internal, reason = "explicit")]
    Explicit,
}

fn main() {}
