use serde::Serialize;

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub kind: ErrorKind,
    pub message: String,
    pub detail: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    Connection,
    Query,
    Edit,
    Credential,
    Config,
    NotFound,
    Mongo,
}

impl AppError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
        }
    }
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// 一行可读文案:`message · detail`。
    pub fn display(&self) -> String {
        match &self.detail {
            Some(d) if !d.is_empty() => format!("{} · {d}", self.message),
            _ => self.message.clone(),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

impl std::error::Error for AppError {}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serializes_to_camel_case_json() {
        let e = AppError::new(ErrorKind::Query, "boom").with_detail("syntax error");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "query");
        assert_eq!(v["message"], "boom");
        assert_eq!(v["detail"], "syntax error");
    }
}
