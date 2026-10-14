use async_trait::async_trait;
use serde_json::Value;

use crate::v2::domain::errors::DomainResult;
use crate::v2::ports::schema::{SchemaValidator, SchemaViolation};

#[derive(Debug, Clone, Default)]
pub struct BasicSchemaValidator;

#[async_trait]
impl SchemaValidator for BasicSchemaValidator {
    async fn validate(
        &self,
        schema: &Value,
        instance: &Value,
    ) -> DomainResult<Vec<SchemaViolation>> {
        let mut violations = Vec::new();
        validate_node(schema, instance, "$", &mut violations);
        Ok(violations)
    }
}

fn validate_node(
    schema: &Value,
    instance: &Value,
    path: &str,
    violations: &mut Vec<SchemaViolation>,
) {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        if !matches_type(expected, instance) {
            violations.push(SchemaViolation {
                path: path.to_string(),
                reason: format!("期望类型 {expected}"),
            });
            return;
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(instance) {
            violations.push(SchemaViolation {
                path: path.to_string(),
                reason: "值不在允许的 enum 中".to_string(),
            });
        }
    }
    match instance {
        Value::Number(number) => {
            if let (Some(min), Some(value)) = (
                schema.get("minimum").and_then(Value::as_f64),
                number.as_f64(),
            ) {
                if value < min {
                    violations.push(SchemaViolation {
                        path: path.to_string(),
                        reason: format!("数值小于最小值 {min}"),
                    });
                }
            }
            if let (Some(max), Some(value)) = (
                schema.get("maximum").and_then(Value::as_f64),
                number.as_f64(),
            ) {
                if value > max {
                    violations.push(SchemaViolation {
                        path: path.to_string(),
                        reason: format!("数值大于最大值 {max}"),
                    });
                }
            }
        }
        Value::String(text) => {
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
                if (text.chars().count() as u64) < min {
                    violations.push(SchemaViolation {
                        path: path.to_string(),
                        reason: format!("字符串长度小于 {min}"),
                    });
                }
            }
            if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
                if (text.chars().count() as u64) > max {
                    violations.push(SchemaViolation {
                        path: path.to_string(),
                        reason: format!("字符串长度大于 {max}"),
                    });
                }
            }
        }
        Value::Object(object) => {
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for name in required {
                    if let Some(name) = name.as_str() {
                        if !object.contains_key(name) {
                            violations.push(SchemaViolation {
                                path: format!("{path}.{name}"),
                                reason: "缺少必填字段".to_string(),
                            });
                        }
                    }
                }
            }
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, property_schema) in properties {
                    if let Some(value) = object.get(name) {
                        validate_node(
                            property_schema,
                            value,
                            &format!("{path}.{name}"),
                            violations,
                        );
                    }
                }
            }
            if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
                let allowed: Vec<&String> = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|properties| properties.keys().collect())
                    .unwrap_or_default();
                for name in object.keys() {
                    if !allowed.contains(&name) {
                        violations.push(SchemaViolation {
                            path: format!("{path}.{name}"),
                            reason: "该字段未被 schema 允许".to_string(),
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

fn matches_type(expected: &str, instance: &Value) -> bool {
    match expected {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        // JSON Schema 中 number 包含整数（整数是 number 的子集）。
        "number" => instance.is_number(),
        "integer" => instance.is_i64() || instance.is_u64(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn rejects_missing_required_field() {
        let schema = json!({
            "type": "object",
            "required": ["prompt"],
            "properties": { "prompt": { "type": "string" } }
        });
        let validator = BasicSchemaValidator;
        let violations = validator.validate(&schema, &json!({})).await.unwrap();
        assert!(violations.iter().any(|v| v.path == "$.prompt"));
    }

    #[tokio::test]
    async fn accepts_integer_for_number_field() {
        let schema = json!({
            "type": "object",
            "properties": { "n": { "type": "number" } }
        });
        let validator = BasicSchemaValidator;
        let violations = validator
            .validate(&schema, &json!({ "n": 0 }))
            .await
            .unwrap();
        assert!(violations.is_empty());
    }

    #[tokio::test]
    async fn rejects_wrong_type() {
        let schema = json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } }
        });
        let validator = BasicSchemaValidator;
        let violations = validator
            .validate(&schema, &json!({ "n": "two" }))
            .await
            .unwrap();
        assert!(violations.iter().any(|v| v.path == "$.n"));
    }

    #[tokio::test]
    async fn accepts_valid_instance() {
        let schema = json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": { "type": "string", "minLength": 1 },
                "n": { "type": "integer", "minimum": 1, "maximum": 8 }
            }
        });
        let validator = BasicSchemaValidator;
        let violations = validator
            .validate(&schema, &json!({ "prompt": "a cat", "n": 2 }))
            .await
            .unwrap();
        assert!(violations.is_empty());
    }

    #[tokio::test]
    async fn rejects_unknown_field_when_disallowed() {
        let schema = json!({
            "type": "object",
            "properties": { "prompt": { "type": "string" } },
            "additionalProperties": false
        });
        let validator = BasicSchemaValidator;
        let violations = validator
            .validate(&schema, &json!({ "prompt": "hi", "hack": 1 }))
            .await
            .unwrap();
        assert!(violations.iter().any(|v| v.path == "$.hack"));
    }
}
