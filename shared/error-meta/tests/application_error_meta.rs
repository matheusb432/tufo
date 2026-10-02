mod application_support;

use std::{fmt::Debug, marker::PhantomData};

use application_support::error::{ApplicationError, ErrorClass, ErrorMetadata};
use error_meta::ApplicationErrorMeta;

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
enum Leaf {
    #[error("missing")]
    #[meta(class = NotFound, reason = Self::MISSING,)]
    Missing,
    #[error("unreadable")]
    #[meta(reason = "audio.unreadable", class = FailedPrecondition)]
    Unreadable(#[source] std::io::Error),
    #[error("invalid")]
    #[meta(class = InvalidArgument, reason = "invalid")]
    Invalid,
    #[error("unavailable")]
    #[meta(class = Unavailable, reason = "unavailable")]
    Unavailable,
}

impl Leaf {
    const MISSING: &'static str = "audio.missing";
}

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[error("storage failed")]
#[meta(class = Internal, reason = Self::REASON)]
struct Storage(#[source] std::io::Error);

impl Storage {
    const REASON: &'static str = "storage.failed";
}

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
enum Wrapper<E, Context> {
    #[error(transparent)]
    #[meta(transparent)]
    Plain(E),
    #[error("{label}: {source}")]
    #[meta(transparent(field = source))]
    Named {
        label: &'static str,
        source: E,
        context: PhantomData<Context>,
    },
    #[error("{0}: {1}")]
    #[meta(transparent(field = 1))]
    Tuple(&'static str, #[source] E, PhantomData<Context>),
}

#[derive(Debug)]
struct Context;

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(transparent)]
#[error(transparent)]
struct TupleWrapper<E>(E);

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(transparent(field = source))]
#[error("{label}: {source}")]
struct NamedWrapper<E, Context> {
    label: &'static str,
    source: E,
    context: PhantomData<Context>,
}

trait Provider {
    type Error;
}

impl Provider for Context {
    type Error = Leaf;
}

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(transparent(field = 0))]
#[error("{0}")]
struct Associated<T>(T::Error)
where
    T: Provider,
    T::Error: Debug;

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(class = Internal, reason = "unit")]
#[error("unit")]
struct Unit;

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(class = Internal, reason = "named")]
#[error("named")]
struct FixedNamed<C> {
    context: PhantomData<C>,
}

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(transparent(field = source))]
#[error("{label}: {source}")]
struct Borrowed<'a, E, const N: usize>
where
    E: 'a,
{
    source: E,
    label: &'a str,
    context: [u8; N],
}

#[test]
fn fixed_metadata_ignores_opaque_causes_and_keeps_sources() {
    use std::error::Error;

    let unreadable = Leaf::Unreadable(std::io::Error::other("cause"));
    assert_eq!(
        unreadable.metadata(),
        ErrorMetadata {
            class: ErrorClass::FailedPrecondition,
            reason: "audio.unreadable"
        }
    );
    assert_eq!(unreadable.to_string(), "unreadable");
    assert_eq!(
        unreadable.source().map(ToString::to_string),
        Some("cause".to_owned())
    );
    let storage = Storage(std::io::Error::other("database"));
    assert_eq!(
        storage.metadata(),
        ErrorMetadata {
            class: ErrorClass::Internal,
            reason: Storage::REASON
        }
    );
    assert_eq!(
        storage.source().map(ToString::to_string),
        Some("database".to_owned())
    );
    assert_eq!(
        Leaf::Missing.metadata(),
        ErrorMetadata {
            class: ErrorClass::NotFound,
            reason: Leaf::MISSING
        }
    );
    assert_eq!(Leaf::Invalid.metadata().class, ErrorClass::InvalidArgument);
    assert_eq!(Leaf::Unavailable.metadata().class, ErrorClass::Unavailable);
    assert_eq!(Unit.metadata().reason, "unit");
    assert_eq!(
        FixedNamed::<Context> {
            context: PhantomData
        }
        .metadata()
        .reason,
        "named"
    );
}

#[test]
fn delegation_preserves_metadata_and_ignores_context_bounds() {
    let expected = Leaf::Missing.metadata();
    let wrappers: [Wrapper<Leaf, Context>; 3] = [
        Wrapper::Plain(Leaf::Missing),
        Wrapper::Named {
            label: "named",
            source: Leaf::Missing,
            context: PhantomData,
        },
        Wrapper::Tuple("tuple", Leaf::Missing, PhantomData),
    ];
    for wrapper in wrappers {
        assert_eq!(wrapper.metadata(), expected);
    }
    assert_eq!(TupleWrapper(Leaf::Missing).metadata(), expected);
    assert_eq!(
        NamedWrapper::<_, Context> {
            label: "context",
            source: TupleWrapper(Leaf::Missing),
            context: PhantomData
        }
        .metadata(),
        expected
    );
    assert_eq!(Associated::<Context>(Leaf::Missing).metadata(), expected);
    let label = String::from("borrowed");
    assert_eq!(
        Borrowed {
            source: Leaf::Missing,
            label: &label,
            context: [0; 3]
        }
        .metadata(),
        expected
    );
}

#[test]
fn enum_class_defaults_merge_with_variant_reasons_and_overrides() {
    #[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
    #[meta(crate = crate::application_support)]
    #[meta(class = FailedPrecondition)]
    enum DefaultClass {
        #[error("busy")]
        #[meta(reason = "operation.busy")]
        Busy,
        #[error("missing")]
        #[meta(class = NotFound, reason = "operation.missing")]
        Missing,
        #[error(transparent)]
        #[meta(transparent)]
        Storage(Storage),
    }

    assert_eq!(
        DefaultClass::Busy.metadata(),
        ErrorMetadata {
            class: ErrorClass::FailedPrecondition,
            reason: "operation.busy"
        }
    );
    assert_eq!(
        DefaultClass::Missing.metadata(),
        ErrorMetadata {
            class: ErrorClass::NotFound,
            reason: "operation.missing"
        }
    );
    assert_eq!(
        DefaultClass::Storage(Storage(std::io::Error::other("cause"))).metadata(),
        ErrorMetadata {
            class: ErrorClass::Internal,
            reason: Storage::REASON
        }
    );
}

#[test]
fn enum_fixed_defaults_fill_only_omitted_metadata() {
    #[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
    #[meta(crate = crate::application_support)]
    #[meta(class = Internal, reason = "operation.failed")]
    enum FixedDefault {
        #[error("default")]
        Default,
        #[error("opaque: {0}")]
        Opaque(#[source] std::io::Error),
        #[error("reason override")]
        #[meta(reason = "operation.unreadable")]
        Reason,
        #[error("class override")]
        #[meta(class = Unavailable)]
        Class,
        #[error(transparent)]
        #[meta(transparent)]
        Inner(Leaf),
    }

    for error in [
        FixedDefault::Default,
        FixedDefault::Opaque(std::io::Error::other("cause")),
    ] {
        assert_eq!(
            error.metadata(),
            ErrorMetadata {
                class: ErrorClass::Internal,
                reason: "operation.failed"
            }
        );
    }
    assert_eq!(
        FixedDefault::Reason.metadata(),
        ErrorMetadata {
            class: ErrorClass::Internal,
            reason: "operation.unreadable"
        }
    );
    assert_eq!(
        FixedDefault::Class.metadata(),
        ErrorMetadata {
            class: ErrorClass::Unavailable,
            reason: "operation.failed"
        }
    );
    assert_eq!(
        FixedDefault::Inner(Leaf::Missing).metadata(),
        Leaf::Missing.metadata()
    );
}

#[test]
fn enum_reason_defaults_can_read_self_and_keep_explicit_classes() {
    #[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
    #[meta(crate = crate::application_support)]
    #[meta(reason = self.reason())]
    enum DefaultReason {
        #[error("first")]
        #[meta(class = Internal)]
        First,
        #[error("second")]
        #[meta(class = NotFound)]
        Second,
    }

    impl DefaultReason {
        fn reason(&self) -> &'static str {
            match self {
                Self::First => "first",
                Self::Second => "second",
            }
        }
    }

    assert_eq!(
        DefaultReason::First.metadata(),
        ErrorMetadata {
            class: ErrorClass::Internal,
            reason: "first"
        }
    );
    assert_eq!(
        DefaultReason::Second.metadata(),
        ErrorMetadata {
            class: ErrorClass::NotFound,
            reason: "second"
        }
    );
}

#[test]
fn enum_transparent_defaults_preserve_metadata_and_generic_bounds() {
    #[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
    #[meta(crate = crate::application_support)]
    #[meta(transparent(field = source))]
    enum DefaultWrapper<E, C> {
        #[error("{label}: {source}")]
        Named {
            label: &'static str,
            source: E,
            context: PhantomData<C>,
        },
        #[error("{source}")]
        Other { source: E },
        #[error("opaque: {0}")]
        #[meta(class = Internal, reason = "opaque")]
        Opaque(#[source] std::io::Error),
    }

    for error in [
        DefaultWrapper::<_, Context>::Named {
            label: "context",
            source: Leaf::Missing,
            context: PhantomData,
        },
        DefaultWrapper::Other {
            source: Leaf::Missing,
        },
    ] {
        assert_eq!(error.metadata(), Leaf::Missing.metadata());
    }
    assert_eq!(
        DefaultWrapper::<Leaf, Context>::Opaque(std::io::Error::other("cause")).metadata(),
        ErrorMetadata {
            class: ErrorClass::Internal,
            reason: "opaque"
        }
    );
}

#[test]
fn qualified_application_classes_support_custom_facade_variants_and_constants() {
    struct Policy;
    impl Policy {
        const CLASS: ErrorClass = ErrorClass::Conflict;
    }

    #[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
    #[meta(crate = crate::application_support)]
    #[meta(class = Policy::CLASS)]
    enum Custom {
        #[error("default")]
        #[meta(reason = "default")]
        Default,
        #[error("qualified")]
        #[meta(class = crate::application_support::error::ErrorClass::Conflict, reason = "qualified")]
        Qualified,
    }

    assert_eq!(
        Custom::Default.metadata(),
        ErrorMetadata {
            class: ErrorClass::Conflict,
            reason: "default"
        }
    );
    assert_eq!(
        Custom::Qualified.metadata(),
        ErrorMetadata {
            class: ErrorClass::Conflict,
            reason: "qualified"
        }
    );
}
