use syn::{Attribute, Error, Member, Path, Result, meta::ParseNestedMeta};

use super::RawMetadata;
use crate::error_class::ErrorClass;

const ERROR_CLASSES: [&str; 5] = [
    "InvalidArgument",
    "NotFound",
    "FailedPrecondition",
    "Unavailable",
    "Internal",
];

impl RawMetadata {
    pub(super) fn parse(attribute: &Attribute) -> Result<Self> {
        let mut class = None;
        let mut reason = None;
        let mut transparent = None;
        attribute.parse_nested_meta(|nested| {
            if nested.path.is_ident("class") {
                if class.is_some() || transparent.is_some() {
                    return Err(nested.error("duplicate or conflicting class metadata"));
                }
                class = Some(ErrorClass::parse(
                    nested.value()?,
                    &ERROR_CLASSES,
                    "application error class",
                )?);
            } else if nested.path.is_ident("reason") {
                if reason.is_some() || transparent.is_some() {
                    return Err(nested.error("duplicate or conflicting reason metadata"));
                }
                reason = Some(Box::new(nested.value()?.parse()?));
            } else if nested.path.is_ident("transparent") {
                if transparent.is_some() || class.is_some() || reason.is_some() {
                    return Err(nested.error("duplicate or conflicting transparent metadata"));
                }
                transparent = Some(field_selection(&nested)?);
            } else {
                return Err(nested.error("expected class, reason, or transparent metadata"));
            }
            Ok(())
        })?;
        if let Some(selection) = transparent {
            return Ok(Self::Transparent(selection));
        }
        match (class, reason) {
            (None, None) => Err(Error::new_spanned(
                attribute,
                "fixed metadata requires both class and reason",
            )),
            (class, reason) => Ok(Self::Fixed { class, reason }),
        }
    }
}

fn field_selection(nested: &ParseNestedMeta<'_>) -> Result<Option<Member>> {
    if !nested.input.peek(syn::token::Paren) {
        return Ok(None);
    }
    let mut selection = None;
    nested.parse_nested_meta(|field| {
        if !field.path.is_ident("field") || selection.is_some() {
            return Err(field.error("expected one field = name or field = index selection"));
        }
        selection = Some(field.value()?.parse()?);
        Ok(())
    })?;
    selection
        .map(Some)
        .ok_or_else(|| nested.error("expected one field = name or field = index selection"))
}

pub(super) fn facade(attribute: &Attribute, duplicate: bool) -> Result<Path> {
    if duplicate {
        return Err(Error::new_spanned(
            attribute,
            "each item takes at most one #[meta(crate = path)] attribute",
        ));
    }
    let mut path = None;
    attribute.parse_nested_meta(|nested| {
        if !nested.path.is_ident("crate") || path.is_some() {
            return Err(nested.error("facade metadata only supports #[meta(crate = path)]"));
        }
        path = Some(nested.value()?.parse()?);
        Ok(())
    })?;
    path.ok_or_else(|| Error::new_spanned(attribute, "expected #[meta(crate = path)]"))
}
