use proc_macro2::TokenStream;
use quote::quote;
use syn::{Error, Path, Result, parse::ParseStream};

#[derive(Clone)]
pub(crate) struct ErrorClass(Path);

impl ErrorClass {
    pub(crate) fn parse(
        input: ParseStream<'_>,
        classes: &[&str],
        description: &str,
    ) -> Result<Self> {
        let path: Path = input.parse()?;
        if let Some(class) = path.get_ident()
            && !classes.contains(&class.to_string().as_str())
        {
            return Err(Error::new(
                class.span(),
                format!(
                    "unknown {description} `{class}`; expected one of {}; custom classes require a qualified path",
                    classes.join(", ")
                ),
            ));
        }
        Ok(Self(path))
    }

    pub(crate) fn expression(&self, class_type: &TokenStream) -> TokenStream {
        let path = &self.0;
        if let Some(class) = path.get_ident() {
            quote!(#class_type::#class)
        } else {
            quote!(#path)
        }
    }
}
