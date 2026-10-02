//! `ErrorMeta` behavior as seen by a downstream crate.

mod support;

use error_meta::ErrorMeta;
use support::failure::{Classification, Classified, ErrorClass, Failure, PushFailure, Resource};

mod facade {
    pub use crate::support::failure;
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
#[meta(crate = crate::support)]
enum InnerError {
    #[error("inner capacity")]
    #[meta(private(ResourceExhausted))]
    Capacity,
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
#[meta(crate = crate::support)]
enum OperationError {
    #[error(transparent)]
    #[meta(failure)]
    Refused(#[from] PushFailure),
    #[error("state lock poisoned")]
    #[meta(private(Internal))]
    State,
    #[error("store failed: {reason}")]
    #[meta(private(Unavailable))]
    Store { reason: String },
    #[error(transparent)]
    #[meta(transparent)]
    Inner(#[from] InnerError),
    #[error(transparent)]
    #[meta(failure)]
    Named { failure: Failure },
    #[error("operation vanished")]
    #[meta(failure = Failure::Gone { resource: Resource::PushOperation })]
    Vanished,
    #[error("review expired at {0}")]
    #[meta(failure = PushFailure::ReviewExpired)]
    Expired(u64),
}

#[test]
fn variants_classify_as_declared() {
    assert_eq!(
        OperationError::Refused(PushFailure::Detached).classify(),
        Classification::Public(Failure::Push(PushFailure::Detached))
    );
    assert_eq!(
        OperationError::State.classify(),
        Classification::Private(ErrorClass::Internal)
    );
    assert_eq!(
        OperationError::Store {
            reason: "disk".into()
        }
        .classify(),
        Classification::Private(ErrorClass::Unavailable)
    );
    assert_eq!(
        OperationError::Inner(InnerError::Capacity).classify(),
        Classification::Private(ErrorClass::ResourceExhausted)
    );
    assert_eq!(
        OperationError::Named {
            failure: Failure::Changed
        }
        .classify(),
        Classification::Public(Failure::Changed)
    );
    assert_eq!(
        OperationError::Vanished.classify(),
        Classification::Public(Failure::Gone {
            resource: Resource::PushOperation
        })
    );
    assert_eq!(
        OperationError::Expired(7).classify(),
        Classification::Public(Failure::Push(PushFailure::ReviewExpired))
    );
}

#[derive(ErrorMeta)]
#[meta(crate = crate::support)]
enum GenericError<'a, Reason, Inner, Context: ?Sized, const SIZE: usize>
where
    Context: 'a,
{
    #[meta(failure)]
    Reason(Reason),
    #[meta(transparent)]
    Inner { error: Inner },
    #[meta(private(Internal))]
    Context {
        _context: &'a Context,
        _bytes: [u8; SIZE],
    },
    #[meta(failure = PushFailure::ReviewExpired,)]
    Expired { _context: &'a Context },
}

#[test]
fn generics_preserve_bounds_and_ignore_private_fields() {
    struct Context;
    type Error<'a> = GenericError<'a, PushFailure, InnerError, Context, 2>;

    assert_eq!(
        Error::Reason(PushFailure::Detached).classify(),
        Classification::Public(Failure::Push(PushFailure::Detached))
    );
    assert_eq!(
        Error::Inner {
            error: InnerError::Capacity
        }
        .classify(),
        Classification::Private(ErrorClass::ResourceExhausted)
    );
    assert_eq!(
        Error::Context {
            _context: &Context,
            _bytes: [0; 2]
        }
        .classify(),
        Classification::Private(ErrorClass::Internal)
    );
    assert_eq!(
        Error::Expired { _context: &Context }.classify(),
        Classification::Public(Failure::Push(PushFailure::ReviewExpired))
    );
}

#[test]
fn delegation_bounds_apply_to_associated_field_types() {
    trait ReasonProvider {
        type Reason;
    }

    struct Provider;
    impl ReasonProvider for Provider {
        type Reason = PushFailure;
    }

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    enum AssociatedError<Source = Provider>
    where
        Source: ReasonProvider,
    {
        #[meta(failure)]
        Reason(Source::Reason),
    }

    assert_eq!(
        AssociatedError::<Provider>::Reason(PushFailure::Detached).classify(),
        Classification::Public(Failure::Push(PushFailure::Detached))
    );
}

#[derive(ErrorMeta)]
#[meta(crate = crate::support)]
enum SelectedError<Reason, Inner, Context> {
    #[meta(failure(field = reason))]
    ReasonNamed { reason: Reason, _context: Context },
    #[meta(failure(field = 1))]
    ReasonTuple(Context, Reason),
    #[meta(transparent(field = error))]
    InnerNamed { error: Inner, _context: Context },
    #[meta(transparent(field = 1))]
    InnerTuple(Context, Inner),
}

#[test]
fn selected_fields_delegate_without_classifying_context() {
    struct Context;
    type Error = SelectedError<PushFailure, InnerError, Context>;

    for error in [
        Error::ReasonNamed {
            reason: PushFailure::Detached,
            _context: Context,
        },
        Error::ReasonTuple(Context, PushFailure::Detached),
    ] {
        assert_eq!(
            error.classify(),
            Classification::Public(Failure::Push(PushFailure::Detached))
        );
    }
    for error in [
        Error::InnerNamed {
            error: InnerError::Capacity,
            _context: Context,
        },
        Error::InnerTuple(Context, InnerError::Capacity),
    ] {
        assert_eq!(
            error.classify(),
            Classification::Private(ErrorClass::ResourceExhausted)
        );
    }
}

#[test]
fn failure_expressions_accept_into_and_run_on_every_call() {
    use std::cell::Cell;

    struct Reason;
    #[expect(
        clippy::from_over_into,
        reason = "The derive must accept Into without a matching From implementation."
    )]
    impl Into<Failure> for Reason {
        fn into(self) -> Failure {
            Failure::Changed
        }
    }

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    enum ExpressionError {
        #[meta(failure = self.reason())]
        Changed(Cell<usize>),
    }

    impl ExpressionError {
        fn reason(&self) -> Reason {
            let Self::Changed(calls) = self;
            calls.set(calls.get() + 1);
            Reason
        }
    }

    let error = ExpressionError::Changed(Cell::new(0));
    assert_eq!(error.classify(), Classification::Public(Failure::Changed));
    assert_eq!(error.classify(), Classification::Public(Failure::Changed));
    let ExpressionError::Changed(calls) = error;
    assert_eq!(calls.get(), 2);
}

#[test]
fn explicit_facade_paths_support_reexports_and_relative_modules() {
    mod models {
        pub use crate::support::failure;
    }

    #[derive(ErrorMeta)]
    #[meta(crate = models)]
    enum LocalFacadeError {
        #[meta(private(Internal))]
        Internal,
    }

    #[derive(ErrorMeta)]
    #[meta(crate = self::facade)]
    enum ReexportedError {
        #[meta(private(Unavailable))]
        Unavailable,
    }

    assert_eq!(
        LocalFacadeError::Internal.classify(),
        Classification::Private(ErrorClass::Internal)
    );
    assert_eq!(
        ReexportedError::Unavailable.classify(),
        Classification::Private(ErrorClass::Unavailable)
    );
}

#[test]
fn failure_expressions_can_read_self() {
    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    enum RequestError {
        #[meta(failure = Failure::InvalidRequest { field: self.field().to_owned() })]
        Invalid { field: String },
    }

    impl RequestError {
        fn field(&self) -> &str {
            match self {
                Self::Invalid { field } => field,
            }
        }
    }

    assert_eq!(
        RequestError::Invalid {
            field: "identity".into()
        }
        .classify(),
        Classification::Public(Failure::InvalidRequest {
            field: "identity".into()
        })
    );
}

#[test]
fn structs_classify_and_delegate_without_context_bounds() {
    struct Context;

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(private(Internal))]
    struct Private<C>(C);

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(failure)]
    struct Public<Reason>(Reason);

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(transparent(field = source))]
    struct Wrapper<Source, C> {
        source: Source,
        _context: C,
    }

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(failure(field = 1))]
    struct Tuple<C, Reason>(C, Reason);

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(private(Unavailable))]
    struct Unit;

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(failure = Failure::InvalidRequest { field: self.field.clone() })]
    struct Request {
        field: String,
    }

    let expected = Classification::Public(Failure::Push(PushFailure::Detached));
    assert_eq!(
        Private(Context).classify(),
        Classification::Private(ErrorClass::Internal)
    );
    assert_eq!(Public(PushFailure::Detached).classify(), expected);
    assert_eq!(
        Wrapper {
            source: Public(PushFailure::Detached),
            _context: Context
        }
        .classify(),
        expected
    );
    assert_eq!(Tuple(Context, PushFailure::Detached).classify(), expected);
    assert_eq!(
        Unit.classify(),
        Classification::Private(ErrorClass::Unavailable)
    );
    assert_eq!(
        Request {
            field: "name".into()
        }
        .classify(),
        Classification::Public(Failure::InvalidRequest {
            field: "name".into()
        })
    );
}

#[test]
fn enum_classification_defaults_allow_explicit_overrides() {
    struct Context;

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(private(Internal))]
    enum DefaultError<C> {
        Unit,
        Context(C),
        Named {
            _context: C,
        },
        #[meta(failure)]
        Public(PushFailure),
        #[meta(transparent)]
        Inner(InnerError),
        #[meta(private(Unavailable))]
        Offline,
    }

    type Error = DefaultError<Context>;
    for error in [
        Error::Unit,
        Error::Context(Context),
        Error::Named { _context: Context },
    ] {
        assert_eq!(
            error.classify(),
            Classification::Private(ErrorClass::Internal)
        );
    }
    assert_eq!(
        Error::Public(PushFailure::Detached).classify(),
        Classification::Public(Failure::Push(PushFailure::Detached))
    );
    assert_eq!(
        Error::Inner(InnerError::Capacity).classify(),
        Classification::Private(ErrorClass::ResourceExhausted)
    );
    assert_eq!(
        Error::Offline.classify(),
        Classification::Private(ErrorClass::Unavailable)
    );
}

#[test]
fn enum_delegation_defaults_add_bounds_only_to_selected_fields() {
    struct Context;

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(failure)]
    enum PublicDefault<Reason, C> {
        Tuple(Reason),
        Named {
            reason: Reason,
        },
        #[meta(private(Internal))]
        Opaque(C),
    }

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(transparent(field = source))]
    enum TransparentDefault<Source, C> {
        First {
            source: Source,
            _context: C,
        },
        Second {
            source: Source,
            _label: &'static str,
        },
    }

    let expected = Classification::Public(Failure::Push(PushFailure::Detached));
    for error in [
        PublicDefault::<_, Context>::Tuple(PushFailure::Detached),
        PublicDefault::Named {
            reason: PushFailure::Detached,
        },
    ] {
        assert_eq!(error.classify(), expected);
    }
    assert_eq!(
        PublicDefault::<PushFailure, _>::Opaque(Context).classify(),
        Classification::Private(ErrorClass::Internal)
    );
    for error in [
        TransparentDefault::First {
            source: InnerError::Capacity,
            _context: Context,
        },
        TransparentDefault::Second {
            source: InnerError::Capacity,
            _label: "context",
        },
    ] {
        assert_eq!(
            error.classify(),
            Classification::Private(ErrorClass::ResourceExhausted)
        );
    }
}

#[test]
fn enum_failure_expression_defaults_evaluate_self_on_every_call() {
    use std::cell::Cell;

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(failure = self.reason())]
    enum DefaultExpression {
        First(Cell<usize>),
        Second { calls: Cell<usize> },
    }

    impl DefaultExpression {
        fn reason(&self) -> PushFailure {
            let calls = match self {
                Self::First(calls) | Self::Second { calls } => calls,
            };
            calls.set(calls.get() + 1);
            PushFailure::ReviewExpired
        }
    }

    for error in [
        DefaultExpression::First(Cell::new(0)),
        DefaultExpression::Second {
            calls: Cell::new(0),
        },
    ] {
        let expected = Classification::Public(Failure::Push(PushFailure::ReviewExpired));
        assert_eq!(error.classify(), expected);
        assert_eq!(error.classify(), expected);
        let calls = match error {
            DefaultExpression::First(calls) | DefaultExpression::Second { calls } => calls,
        };
        assert_eq!(calls.get(), 2);
    }
}

#[test]
fn qualified_private_classes_support_custom_facade_variants_and_constants() {
    struct Policy;
    impl Policy {
        const CLASS: ErrorClass = ErrorClass::Throttled;
    }

    #[derive(ErrorMeta)]
    #[meta(crate = crate::support)]
    #[meta(private(Policy::CLASS))]
    enum Custom {
        Default,
        #[meta(private(crate::support::failure::ErrorClass::Throttled))]
        Qualified,
    }

    assert_eq!(
        Custom::Default.classify(),
        Classification::Private(ErrorClass::Throttled)
    );
    assert_eq!(
        Custom::Qualified.classify(),
        Classification::Private(ErrorClass::Throttled)
    );
}
