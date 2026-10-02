mod attributes;

use syn::{
    Attribute, Data, DeriveInput, Error, Expr, Fields, Generics, Ident, Index, Member, Path,
    Result, Type, spanned::Spanned,
};

use crate::error_class::ErrorClass;

pub(super) struct ApplicationInput<'a> {
    pub(super) ident: &'a Ident,
    pub(super) generics: &'a Generics,
    pub(super) facade: Path,
    pub(super) declarations: Vec<Declaration<'a>>,
}

pub(super) struct Declaration<'a> {
    pub(super) variant: Option<&'a Ident>,
    pub(super) metadata: Metadata<'a>,
}

pub(super) enum Metadata<'a> {
    Fixed {
        class: ErrorClass,
        reason: Box<Expr>,
    },
    Transparent(DelegatedField<'a>),
}

pub(super) struct DelegatedField<'a> {
    pub(super) member: Member,
    pub(super) ty: &'a Type,
}

#[derive(Clone)]
enum RawMetadata {
    Fixed {
        class: Option<ErrorClass>,
        reason: Option<Box<Expr>>,
    },
    Transparent(Option<Member>),
}

impl<'a> ApplicationInput<'a> {
    pub(super) fn parse(input: &'a DeriveInput) -> Result<Self> {
        let mut errors = None;
        let mut facade_override = None;
        let mut item_metadata = Vec::new();
        for attribute in metadata_attributes(&input.attrs) {
            if crate::facade::is_override(attribute) {
                match attributes::facade(attribute, facade_override.is_some()) {
                    Ok(path) => facade_override = Some(path),
                    Err(error) => combine(&mut errors, error),
                }
            } else {
                item_metadata.push(attribute);
            }
        }
        let mut declarations = Vec::new();
        match &input.data {
            Data::Enum(data) => {
                let defaults = match metadata_attribute(&item_metadata)
                    .and_then(|attribute| attribute.map(RawMetadata::parse).transpose())
                {
                    Ok(defaults) => defaults,
                    Err(error) => {
                        combine(&mut errors, error);
                        None
                    }
                };
                for variant in &data.variants {
                    reject_field_metadata(&variant.fields, &mut errors);
                    match parse_metadata(
                        &variant.ident,
                        &variant.fields,
                        &metadata_attributes(&variant.attrs).collect::<Vec<_>>(),
                        defaults.as_ref(),
                    ) {
                        Ok(metadata) => declarations.push(Declaration {
                            variant: Some(&variant.ident),
                            metadata,
                        }),
                        Err(error) => combine(&mut errors, error),
                    }
                }
            }
            Data::Struct(data) => {
                reject_field_metadata(&data.fields, &mut errors);
                match parse_metadata(&input.ident, &data.fields, &item_metadata, None) {
                    Ok(metadata) => declarations.push(Declaration {
                        variant: None,
                        metadata,
                    }),
                    Err(error) => combine(&mut errors, error),
                }
            }
            Data::Union(_) => combine(
                &mut errors,
                Error::new(
                    input.ident.span(),
                    "ApplicationErrorMeta can only be derived for enums and structs",
                ),
            ),
        }
        if let Some(error) = errors {
            return Err(error);
        }
        Ok(Self {
            ident: &input.ident,
            generics: &input.generics,
            facade: crate::facade::resolve_for(
                facade_override,
                input.ident.span(),
                "tufo-application",
                "tufo_application",
            )?,
            declarations,
        })
    }
}

fn parse_metadata<'a>(
    ident: &Ident,
    fields: &'a Fields,
    attributes: &[&Attribute],
    defaults: Option<&RawMetadata>,
) -> Result<Metadata<'a>> {
    let attribute = metadata_attribute(attributes)?;
    let metadata = attribute
        .map(RawMetadata::parse)
        .transpose()?
        .or_else(|| defaults.cloned());
    let Some(metadata) = metadata else {
        return Err(Error::new(
            ident.span(),
            "missing metadata; expected #[meta(class = Class, reason = expression)] or #[meta(transparent)]",
        ));
    };
    match metadata {
        RawMetadata::Fixed { class, reason } => {
            let (class_default, reason_default) = match defaults {
                Some(RawMetadata::Fixed { class, reason }) => (class.clone(), reason.clone()),
                Some(RawMetadata::Transparent(_)) | None => (None, None),
            };
            match (class.or(class_default), reason.or(reason_default)) {
                (Some(class), Some(reason)) => Ok(Metadata::Fixed { class, reason }),
                _ => Err(attribute.map_or_else(
                    || {
                        Error::new(
                            ident.span(),
                            "fixed metadata requires both class and reason",
                        )
                    },
                    |attribute| {
                        Error::new_spanned(
                            attribute,
                            "fixed metadata requires both class and reason",
                        )
                    },
                )),
            }
        }
        RawMetadata::Transparent(selection) => Ok(Metadata::Transparent(DelegatedField::parse(
            ident, fields, selection,
        )?)),
    }
}

fn metadata_attribute<'a>(attributes: &[&'a Attribute]) -> Result<Option<&'a Attribute>> {
    let mut errors = None;
    for duplicate in attributes.iter().skip(1) {
        combine(
            &mut errors,
            Error::new_spanned(
                duplicate,
                "each enum, variant, or struct takes at most one metadata declaration",
            ),
        );
    }
    if let Some(error) = errors {
        return Err(error);
    }
    Ok(attributes.first().copied())
}

impl<'a> DelegatedField<'a> {
    fn parse(ident: &Ident, fields: &'a Fields, selection: Option<Member>) -> Result<Self> {
        let member = selection.map_or_else(|| only_field(ident, fields), Ok)?;
        let field = match (fields, &member) {
            (Fields::Named(fields), Member::Named(name)) => fields
                .named
                .iter()
                .find(|field| field.ident.as_ref() == Some(name)),
            (Fields::Unnamed(fields), Member::Unnamed(index)) => usize::try_from(index.index)
                .ok()
                .and_then(|index| fields.unnamed.iter().nth(index)),
            _ => None,
        };
        let Some(field) = field else {
            return Err(Error::new(
                member.span(),
                format!(
                    "`{ident}` has no selected field `{}`",
                    quote::quote!(#member)
                ),
            ));
        };
        Ok(Self {
            member,
            ty: &field.ty,
        })
    }
}

fn only_field(ident: &Ident, fields: &Fields) -> Result<Member> {
    if fields.len() != 1 {
        return Err(Error::new(
            ident.span(),
            "#[meta(transparent)] requires exactly one field; select a field with #[meta(transparent(field = name))] or field = index",
        ));
    }
    Ok(fields
        .iter()
        .next()
        .and_then(|field| field.ident.clone())
        .map_or_else(|| Member::Unnamed(Index::from(0)), Member::Named))
}

fn reject_field_metadata(fields: &Fields, errors: &mut Option<Error>) {
    for attribute in fields
        .iter()
        .flat_map(|field| metadata_attributes(&field.attrs))
    {
        combine(
            errors,
            Error::new_spanned(
                attribute,
                "metadata belongs on variants or structs, not fields",
            ),
        );
    }
}

fn metadata_attributes(attributes: &[Attribute]) -> impl Iterator<Item = &Attribute> {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("meta"))
}

fn combine(errors: &mut Option<Error>, error: Error) {
    match errors {
        Some(errors) => errors.combine(error),
        None => *errors = Some(error),
    }
}
