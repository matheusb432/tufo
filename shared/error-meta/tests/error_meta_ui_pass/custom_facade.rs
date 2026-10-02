use error_meta::ErrorMeta;

mod failure {
    pub enum ErrorClass {
        Internal,
    }

    pub enum Classification {
        Private(ErrorClass),
    }

    pub trait Classified {
        fn classify(&self) -> Classification;
    }
}

#[derive(ErrorMeta)]
#[meta(crate = crate)]
enum Error<'a, Context: ?Sized, const SIZE: usize>
where
    Context: 'a,
{
    #[meta(private(Internal))]
    Private {
        _context: &'a Context,
        _bytes: [u8; SIZE],
    },
}

fn main() {
    struct Context;

    let error = Error::Private {
        _context: &Context,
        _bytes: [0; 2],
    };
    let failure::Classification::Private(failure::ErrorClass::Internal) =
        failure::Classified::classify(&error);
}
