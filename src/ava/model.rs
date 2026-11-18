#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub operation: &'static str,
    pub field: String,
    pub kind: ProblemKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemKind {
    GraphqlError(String),
    Missing,
    WrongType,
    UnknownValue(String),
    UnparseableDate(String),
}

impl Problem {
    pub fn signature(&self) -> String {
        let tag = match &self.kind {
            ProblemKind::GraphqlError(_) => "graphql_error",
            ProblemKind::Missing => "missing",
            ProblemKind::WrongType => "wrong_type",
            ProblemKind::UnknownValue(_) => "unknown_value",
            ProblemKind::UnparseableDate(_) => "unparseable_date",
        };
        format!("{}:{}:{tag}", self.operation, self.field)
    }
}
