use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendErrorCode {
    InvalidArgument,
    /// 生成预算耗尽：模型在 token 上限内没有产出结束符（本地 Gemma「停不下来」）。
    /// 与其它 InvalidArgument 分开，便于听写管线针对性回退原文。
    GenerationBudgetExceeded,
    /// 整段录音全为零样本或零长度：麦克风没有产生任何有效数据。
    NoAudio,
    InvalidState,
    Busy,
    Cancelled,
    PermissionDenied,
    Unsupported,
    Provider,
    Persistence,
    Platform,
    OutcomeUnknown,
    Internal,
}

#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq, Eq)]
#[error("{message}")]
pub struct BackendError {
    pub code: BackendErrorCode,
    pub message: String,
    pub retryable: bool,
    pub details: Option<serde_json::Value>,
}

impl BackendError {
    pub fn new(code: BackendErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: None,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable_and_errors_do_not_invent_secret_fields() {
        let value = serde_json::to_value(
            BackendError::new(BackendErrorCode::PermissionDenied, "permission required")
                .retryable(false),
        )
        .unwrap();
        assert_eq!(value["code"], "permission_denied");
        assert_eq!(value["retryable"], false);
        assert!(value.get("token").is_none());
        assert!(value.get("authorization").is_none());
        assert!(value.get("pin").is_none());
        assert_eq!(
            serde_json::to_value(BackendErrorCode::NoAudio).unwrap(),
            serde_json::json!("no_audio")
        );
        // 新增变体必须带稳定的 wire 值：前端/历史按字符串读，不能变成数字下标。
        assert_eq!(
            serde_json::to_value(BackendErrorCode::GenerationBudgetExceeded).unwrap(),
            serde_json::json!("generation_budget_exceeded")
        );
    }
}
