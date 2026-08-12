//! Parameterized SQLite query builder, port of
//! `packages/session-backends/sqlite-node/src/sqlite/sql.ts`.

/// A SQLite parameter value (JS `unknown` narrowed to what the backend
/// accepts: numbers, strings, booleans, null, and JSON values as text).
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
}

impl From<f64> for SqlValue {
    fn from(value: f64) -> Self {
        if value.fract() == 0.0 && value.abs() < 9.007199254740992e15 {
            SqlValue::Int(value as i64)
        } else {
            SqlValue::Float(value)
        }
    }
}

impl From<i64> for SqlValue {
    fn from(value: i64) -> Self {
        SqlValue::Int(value)
    }
}

impl From<&str> for SqlValue {
    fn from(value: &str) -> Self {
        SqlValue::Text(value.to_string())
    }
}

impl From<String> for SqlValue {
    fn from(value: String) -> Self {
        SqlValue::Text(value)
    }
}

impl From<bool> for SqlValue {
    fn from(value: bool) -> Self {
        SqlValue::Int(if value { 1 } else { 0 })
    }
}

impl From<Option<&str>> for SqlValue {
    fn from(value: Option<&str>) -> Self {
        match value {
            Some(value) => SqlValue::Text(value.to_string()),
            None => SqlValue::Null,
        }
    }
}

impl From<Option<String>> for SqlValue {
    fn from(value: Option<String>) -> Self {
        match value {
            Some(value) => SqlValue::Text(value),
            None => SqlValue::Null,
        }
    }
}

impl From<Option<f64>> for SqlValue {
    fn from(value: Option<f64>) -> Self {
        match value {
            Some(value) => value.into(),
            None => SqlValue::Null,
        }
    }
}

impl From<Option<i64>> for SqlValue {
    fn from(value: Option<i64>) -> Self {
        match value {
            Some(value) => SqlValue::Int(value),
            None => SqlValue::Null,
        }
    }
}

/// A parameterized SQLite query produced by the `sql!` macro.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlQuery {
    pub query_text: String,
    pub params: Vec<SqlValue>,
}

impl SqlQuery {
    pub fn new(query_text: String, params: Vec<SqlValue>) -> Self {
        Self { query_text, params }
    }

    pub fn has_params(&self) -> bool {
        !self.params.is_empty()
    }
}

/// Builds a parameterized query. Nested queries are inlined; other
/// interpolations become `?` parameters (JS `sql` template helper).
#[macro_export]
macro_rules! sql {
    ($($part:expr),* $(,)?) => {{
        let parts: Vec<$crate::sql::SqlPart> = vec![$($part.into()),*];
        $crate::sql::build_sql_query(&parts)
    }};
}

/// Runtime helper for the `sql!` macro: interleaves literal parts and
/// values, inlining SqlQuery values and parameterizing everything else.
pub fn build_sql_query(parts: &[SqlPart]) -> SqlQuery {
    let mut query_text = String::new();
    let mut params: Vec<SqlValue> = Vec::new();
    let mut first = true;
    for part in parts {
        match part {
            SqlPart::Literal(text) => {
                if !first {
                    // Literals are joined directly (JS template semantics).
                }
                query_text.push_str(text);
            }
            SqlPart::Value(value) => {
                query_text.push('?');
                params.push(value.clone());
            }
            SqlPart::Query(query) => {
                query_text.push_str(&query.query_text);
                params.extend(query.params.iter().cloned());
            }
        }
        first = false;
    }
    SqlQuery { query_text, params }
}

/// One segment of a `sql!` invocation.
#[derive(Clone, Debug)]
pub enum SqlPart {
    Literal(String),
    Value(SqlValue),
    Query(SqlQuery),
}

impl From<&str> for SqlPart {
    fn from(value: &str) -> Self {
        SqlPart::Literal(value.to_string())
    }
}

impl From<String> for SqlPart {
    fn from(value: String) -> Self {
        SqlPart::Literal(value)
    }
}

impl From<SqlQuery> for SqlPart {
    fn from(value: SqlQuery) -> Self {
        SqlPart::Query(value)
    }
}

impl From<SqlValue> for SqlPart {
    fn from(value: SqlValue) -> Self {
        SqlPart::Value(value)
    }
}

impl From<f64> for SqlPart {
    fn from(value: f64) -> Self {
        SqlPart::Value(value.into())
    }
}

impl From<i64> for SqlPart {
    fn from(value: i64) -> Self {
        SqlPart::Value(value.into())
    }
}

/// Joins trusted query fragments while preserving their parameter order
/// (JS `joinSqlFragments`).
pub fn join_sql_fragments(fragments: &[SqlQuery], separator: &str) -> SqlQuery {
    let mut query_text = String::new();
    let mut params: Vec<SqlValue> = Vec::new();
    for (index, fragment) in fragments.iter().enumerate() {
        if index > 0 {
            query_text.push_str(separator);
        }
        query_text.push_str(&fragment.query_text);
        params.extend(fragment.params.iter().cloned());
    }
    SqlQuery { query_text, params }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_parameterized_queries() {
        let id = "s1";
        let query = sql!("SELECT * FROM entries WHERE id = ", SqlPart::Value(id.into()), " AND seq > ", SqlPart::Value(5i64.into()));
        assert_eq!(query.query_text, "SELECT * FROM entries WHERE id = ? AND seq > ?");
        assert_eq!(query.params.len(), 2);
    }

    #[test]
    fn inlines_nested_queries() {
        let inner = SqlQuery::new("seq = ?".to_string(), vec![SqlValue::Int(3)]);
        let query = sql!("SELECT * FROM entries WHERE ", inner, " AND id = ", SqlPart::Value("s1".into()));
        assert_eq!(query.query_text, "SELECT * FROM entries WHERE seq = ? AND id = ?");
        assert_eq!(query.params.len(), 2);
        assert_eq!(query.params[0], SqlValue::Int(3));
    }

    #[test]
    fn joins_fragments() {
        let a = SqlQuery::new("a = ?".to_string(), vec![SqlValue::Int(1)]);
        let b = SqlQuery::new("b = ?".to_string(), vec![SqlValue::Text("x".to_string())]);
        let joined = join_sql_fragments(&[a, b], " AND ");
        assert_eq!(joined.query_text, "a = ? AND b = ?");
        assert_eq!(joined.params.len(), 2);
    }
}
