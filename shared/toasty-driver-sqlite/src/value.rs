use rusqlite::{
    Row,
    types::{ToSql, ToSqlOutput, ValueRef},
};
use toasty_core::stmt;

#[derive(Debug)]
pub(crate) struct Value(stmt::Value);

impl From<stmt::Value> for Value {
    fn from(value: stmt::Value) -> Self {
        Self(value)
    }
}

impl Value {
    /// Converts this SQLite driver value into the core Toasty value.
    pub(crate) fn into_inner(self) -> stmt::Value {
        self.0
    }

    /// Converts a SQLite value within a row to a Toasty value.
    pub(crate) fn from_sql(row: &Row, index: usize, ty: &stmt::Type) -> Self {
        let value: Option<rusqlite::types::Value> = row.get(index).unwrap();

        let core_value = match value {
            Some(rusqlite::types::Value::Null) | None => stmt::Value::Null,
            Some(rusqlite::types::Value::Integer(value)) => match ty {
                stmt::Type::Bool => stmt::Value::Bool(value != 0),
                stmt::Type::I8 => stmt::Value::I8(value as i8),
                stmt::Type::I16 => stmt::Value::I16(value as i16),
                stmt::Type::I32 => stmt::Value::I32(value as i32),
                stmt::Type::I64 => stmt::Value::I64(value),
                stmt::Type::U8 => stmt::Value::U8(value as u8),
                stmt::Type::U16 => stmt::Value::U16(value as u16),
                stmt::Type::U32 => stmt::Value::U32(value as u32),
                stmt::Type::U64 => stmt::Value::U64(value as u64),
                _ => todo!("ty={ty:#?}"),
            },
            Some(rusqlite::types::Value::Real(value)) => match ty {
                stmt::Type::F32 => stmt::Value::F32(value as f32),
                stmt::Type::F64 => stmt::Value::F64(value),
                _ => todo!("ty={ty:#?}"),
            },
            Some(rusqlite::types::Value::Text(value)) => match ty {
                stmt::Type::Uuid => stmt::Value::Uuid(value.parse().expect("text is a valid uuid")),
                stmt::Type::List(elem) => json_text_to_value_list(&value, elem),
                // A bare `#[document]` column (`Type::Object`) decodes
                // shape-directed to the named `Value::Object` wire form; the
                // engine raises it to the embed's positional record.
                stmt::Type::Object => json_text_to_value(&value, ty),
                _ => stmt::Value::String(value),
            },
            Some(rusqlite::types::Value::Blob(value)) => match ty {
                stmt::Type::Bytes => stmt::Value::Bytes(value),
                stmt::Type::Uuid => {
                    stmt::Value::Uuid(uuid::Uuid::from_slice(&value).expect("blob is a valid uuid"))
                }
                _ => todo!("value={value:#?}"),
            },
        };

        Value(core_value)
    }

    /// Converts a SQLite value within a row using SQLite's runtime storage class.
    pub(crate) fn from_sql_infer(row: &Row, index: usize) -> Self {
        let value: Option<rusqlite::types::Value> = row.get(index).unwrap();

        let core_value = match value {
            Some(rusqlite::types::Value::Null) | None => stmt::Value::Null,
            Some(rusqlite::types::Value::Integer(value)) => stmt::Value::I64(value),
            Some(rusqlite::types::Value::Real(value)) => stmt::Value::F64(value),
            Some(rusqlite::types::Value::Text(value)) => stmt::Value::String(value),
            Some(rusqlite::types::Value::Blob(value)) => stmt::Value::Bytes(value),
        };

        Value(core_value)
    }
}

impl ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        use stmt::Value;

        match &self.0 {
            Value::Bool(true) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(1))),
            Value::Bool(false) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(0))),
            Value::I8(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(
                *v as i64,
            ))),
            Value::I16(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(
                *v as i64,
            ))),
            Value::I32(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(
                *v as i64,
            ))),
            Value::I64(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(*v))),
            Value::U8(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(
                *v as i64,
            ))),
            Value::U16(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(
                *v as i64,
            ))),
            Value::U32(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(
                *v as i64,
            ))),
            Value::U64(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Integer(
                *v as i64,
            ))),
            Value::F32(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Real(*v as f64))),
            Value::F64(v) => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Real(*v))),
            Value::String(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Text(v.as_bytes()))),
            Value::Bytes(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Blob(&v[..]))),
            Value::Uuid(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Blob(v.as_bytes()))),
            Value::Null => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Null)),
            // A `Vec<scalar>` / document collection (`List`) or a bare
            // `#[document]` embed (`Object`) is stored as JSON text.
            Value::List(_) | Value::Object(_) => Ok(ToSqlOutput::Owned(
                rusqlite::types::Value::Text(value_to_json_text(&self.0)),
            )),
            _ => todo!("value = {:#?}", self.0),
        }
    }
}

fn value_to_json_text(value: &stmt::Value) -> String {
    toasty_sql::json::to_string(value).expect("serialize document value to JSON")
}

fn json_text_to_value_list(text: &str, elem_ty: &stmt::Type) -> stmt::Value {
    toasty_sql::json::list_from_str(text, elem_ty)
        .expect("SQLite returned non-JSON for a collection column")
}

fn json_text_to_value(text: &str, ty: &stmt::Type) -> stmt::Value {
    toasty_sql::json::from_str(text, ty).expect("SQLite returned non-JSON for a document column")
}
