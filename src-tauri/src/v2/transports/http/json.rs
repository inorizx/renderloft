//! 与 `axum::Json` 同名的包装提取器。
//!
//! 动机：axum 在请求体反序列化失败时返回**纯文本**响应（HTTP 422，
//! 内容形如 `Failed to deserialize the JSON body into the target type: ...`），
//! 既不是统一信封，也跳过了错误脱敏。前端只能把它显示成"无法解析的响应"，
//! 真实的字段名/类型错误被完全埋掉。
//!
//! 这里保持与 `axum::Json` 完全一致的响应侧行为，只接管请求侧的 rejection：
//! 转成结构化 `SafeError` 信封，且不回显请求体中的值（避免密钥被写进错误消息）。

use axum::extract::{FromRequest, Request};
use axum::response::IntoResponse;
use serde::de::DeserializeOwned;

use crate::v2::domain::errors::{ErrorKind, SafeError};

use super::error;

pub struct Json<T>(pub T);

impl<T> Json<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> IntoResponse for Json<T>
where
    axum::Json<T>: IntoResponse,
{
    fn into_response(self) -> axum::response::Response {
        axum::Json(self.0).into_response()
    }
}

/// 把 serde 的错误摘要收敛成可安全回显的消息。
///
/// serde 的类型错误会把出错的值原样打印出来（`invalid type: string "xxx"`），
/// 在携带密钥的端点上等于把值回显到客户端，因此只提取**缺字段**这类不含值的信息，
/// 其余情况一律给概括描述。
fn safe_message(detail: &str) -> String {
    const MISSING: &str = "missing field `";
    if let Some(start) = detail.find(MISSING) {
        let rest = &detail[start + MISSING.len()..];
        if let Some(end) = rest.find('`') {
            return format!("请求体缺少必需字段：{}", &rest[..end]);
        }
    }
    if detail.contains("invalid type") || detail.contains("invalid length") {
        return "请求体存在字段类型不匹配".to_string();
    }
    if detail.contains("unknown field") {
        return "请求体包含未知字段".to_string();
    }
    if detail.contains("Failed to parse the request body as JSON")
        || detail.contains("expected value")
        || detail.contains("EOF while parsing")
    {
        return "请求体不是合法的 JSON".to_string();
    }
    if detail.contains("Content-Type") {
        return "请求头 Content-Type 必须为 application/json".to_string();
    }
    "请求体格式不正确".to_string()
}

impl<S, T> FromRequest<S> for Json<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = axum::response::Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Json(value)),
            Err(rejection) => Err(error::error_response(SafeError {
                code: "INVALID_REQUEST_BODY".to_string(),
                kind: ErrorKind::Validation,
                message: safe_message(&rejection.to_string()),
                retryable: false,
                details: Default::default(),
            })),
        }
    }
}
