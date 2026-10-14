//! 字段**控件**：用户对"这个字段该用什么控件、有哪些合法选项"的决定。
//!
//! 为什么这层属于"用户的决定"而不是我们的启发式：有一部分**只有用户知道** ——
//! 他那张图其实只有三种画幅、他常用的采样器就那五个（`aspect_ratio`、`sampler_name`
//! 在拿不到引擎定义时我们只能给文本框）。
//!
//! 分工（三条边界）：
//! * **我们**只给默认值（引擎声明 > 值的形状 > 关键字建议，见 `recipe_import`）；
//! * **用户**一改就是他的决定，并且记下来源（`x-widget-source`）；
//! * 以后我们改匹配逻辑时，**不许覆盖 `source = user` 的那些**。
//!
//! ⚠️ 一个字段换成"图片或文件"会改变**执行语义**（值要走资产上传那条路、按
//! `mediaMapping` 回填图谱），所以它不是纯展示的改动 —— 改了要**追加一份 revision**，
//! 而不是当备注那样读时合并（那是 `apply_field_labels` 干的事）。

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// 用户能选的控件种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Widget {
    Text,
    Textarea,
    Number,
    Switch,
    Dropdown,
    File,
}

impl Widget {
    /// 认不出来就当"没指定"——**不许因为一个拼错的字符串就改变字段的行为**。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" => Some(Self::Text),
            "textarea" | "multiline" => Some(Self::Textarea),
            "number" => Some(Self::Number),
            "switch" | "boolean" => Some(Self::Switch),
            "dropdown" | "select" | "enum" => Some(Self::Dropdown),
            "file" | "image" => Some(Self::File),
            _ => None,
        }
    }

    /// 稳定的线格式（存进 `x-widget-source` 的兄弟键 `x-widget` 里，界面据此回填下拉）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Textarea => "textarea",
            Self::Number => "number",
            Self::Switch => "switch",
            Self::Dropdown => "dropdown",
            Self::File => "file",
        }
    }
}

/// 用户在编辑里对某条字段的控件决定。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldWidgetChange {
    pub key: String,
    pub widget: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// 把用户选的控件写进一份字段定义。
///
/// **只动"控件形状"这几个键**（`type`/`format`/`enum`），引擎给的范围（`minimum`/`maximum`/
/// `multipleOf`）留着——它们是**信息**（"这东西通常是 1–10000"），不是约束：
/// 组件不会拿它们拦人，但用户看得见，比一无所知好。
///
/// ⚠️ 选了"下拉"却没填选项 ⇒ 退回文本，**不硬造一个空下拉**（空下拉比文本框更糟：
/// 用户点开发现没得选）。这是"说了但做不到"的唯一情形，代码里明说，不假装做到了。
pub fn apply_widget(
    widget: Widget,
    options: &[String],
    schema: &mut serde_json::Map<String, Value>,
) {
    // 先清掉可能来自引擎的控件形状。
    schema.remove("enum");
    schema.remove("format");
    match widget {
        Widget::Text => {
            schema.insert("type".to_string(), json!("string"));
        }
        Widget::Textarea => {
            schema.insert("type".to_string(), json!("string"));
            schema.insert("format".to_string(), json!("textarea"));
        }
        Widget::Number => {
            schema.insert("type".to_string(), json!("number"));
        }
        Widget::Switch => {
            schema.insert("type".to_string(), json!("boolean"));
        }
        Widget::Dropdown => {
            schema.insert("type".to_string(), json!("string"));
            if !options.is_empty() {
                schema.insert("enum".to_string(), json!(options));
            }
        }
        Widget::File => {
            schema.insert("type".to_string(), json!("string"));
            schema.insert("format".to_string(), json!("file"));
        }
    }
    // 界面据此把下拉回填成用户选的那一项；`x-` 前缀是给消费者看的注解，JSON Schema 会忽略。
    schema.insert("x-widget".to_string(), json!(widget.as_str()));
}

/// 一次"改控件"的结果：新 schema + 每条被改字段的**有效值**（含"选了下拉但没选项"的降级）。
#[derive(Debug, Clone)]
pub struct AppliedWidgets {
    pub schema: Value,
    /// `key -> (实际生效的控件, 是否为媒体输入)`。
    pub effective: BTreeMap<String, (Widget, bool)>,
}

/// 把一批"改控件"的决定应用到 schema 上。
///
/// 返回**实际生效**的控件（而不是用户嘴上说的那个）：`file` 与否决定这条字段走不走媒体那条路，
/// 调用方要拿它去调整 `fieldMapping` / `mediaMapping`。
///
/// 指向 schema 里不存在的字段 ⇒ **跳过，不新增字段**（与 `apply_field_labels` 同一条规矩：
/// 用户的选择是描述，不该让 schema 长出应用根本没有的参数）。
pub fn apply_field_widgets(schema: &Value, changes: &[FieldWidgetChange]) -> AppliedWidgets {
    let mut schema = schema.clone();
    let mut effective = BTreeMap::new();
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return AppliedWidgets { schema, effective };
    };
    for change in changes {
        // 认不出的控件名 = 没指定 ⇒ 这条不动（不许有副作用）。
        let Some(widget) = Widget::parse(&change.widget) else {
            continue;
        };
        let Some(property) = properties
            .get_mut(&change.key)
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let options: Vec<String> = change
            .options
            .iter()
            .map(|option| option.trim().to_string())
            .filter(|option| !option.is_empty())
            .collect();
        apply_widget(widget, &options, property);
        property.insert("x-widget-source".to_string(), json!("user"));
        // 记下**实际生效**的控件：选了下拉却没填选项时它是文本（见 `apply_widget`）。
        let effective_widget = if widget == Widget::Dropdown && options.is_empty() {
            Widget::Text
        } else {
            widget
        };
        effective.insert(
            change.key.clone(),
            (effective_widget, effective_widget == Widget::File),
        );
    }
    AppliedWidgets { schema, effective }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_with(properties: Value) -> Value {
        json!({ "type": "object", "properties": properties })
    }

    /// 用户选了下拉 ⇒ 就是他填的那几个（引擎的枚举让位）。
    #[test]
    fn dropdown_uses_the_users_own_options() {
        let schema = schema_with(json!({
            "3::sampler_name": { "type": "string", "enum": ["euler", "dpmpp_2m"], "x-widget-source": "engine" }
        }));
        let applied = apply_field_widgets(
            &schema,
            &[FieldWidgetChange {
                key: "3::sampler_name".to_string(),
                widget: "dropdown".to_string(),
                options: vec!["euler".to_string(), "heun".to_string()],
            }],
        );
        let field = &applied.schema["properties"]["3::sampler_name"];
        assert_eq!(field["enum"], json!(["euler", "heun"]));
        assert_eq!(field["x-widget-source"], json!("user"));
        assert_eq!(field["x-widget"], json!("dropdown"));
        assert_eq!(
            applied.effective["3::sampler_name"],
            (Widget::Dropdown, false)
        );
    }

    /// 选了下拉却没填选项 ⇒ **退回文本**，而且 `effective` 如实说它是文本
    /// （调用方要拿它去决定映射，不能按用户嘴上说的算）。
    #[test]
    fn dropdown_without_options_degrades_to_text_and_says_so() {
        let schema = schema_with(json!({ "6::aspect_ratio": { "type": "string" } }));
        let applied = apply_field_widgets(
            &schema,
            &[FieldWidgetChange {
                key: "6::aspect_ratio".to_string(),
                widget: "dropdown".to_string(),
                options: vec!["  ".to_string()],
            }],
        );
        let field = &applied.schema["properties"]["6::aspect_ratio"];
        assert!(field.get("enum").is_none(), "不许造一个空下拉：{field}");
        assert_eq!(applied.effective["6::aspect_ratio"], (Widget::Text, false));
    }

    /// 认不出的控件名 = 没指定：字段**一个字都不动**（含来源标记）。
    #[test]
    fn unknown_widget_names_change_nothing() {
        let schema = schema_with(json!({
            "3::steps": { "type": "integer", "x-widget-source": "engine" }
        }));
        let applied = apply_field_widgets(
            &schema,
            &[FieldWidgetChange {
                key: "3::steps".to_string(),
                widget: "slidr".to_string(),
                options: vec![],
            }],
        );
        assert_eq!(
            applied.schema, schema,
            "拼错的控件名不许有副作用：{:?}",
            applied.schema
        );
        assert!(applied.effective.is_empty());
    }

    /// 指向不存在的字段 ⇒ 跳过、**不新增字段**（与 `apply_field_labels` 同一条规矩）。
    #[test]
    fn changes_never_grow_the_schema() {
        let schema = schema_with(json!({ "3::steps": { "type": "integer" } }));
        let applied = apply_field_widgets(
            &schema,
            &[FieldWidgetChange {
                key: "999::typo".to_string(),
                widget: "text".to_string(),
                options: vec![],
            }],
        );
        assert_eq!(
            applied.schema["properties"]
                .as_object()
                .map(|map| map.len()),
            Some(1)
        );
        assert!(applied.effective.is_empty());
    }

    /// 换成"图片或文件" ⇒ `format: file` + `effective` 标成媒体（执行语义变了，调用方要调整映射）。
    #[test]
    fn file_widget_is_reported_as_media() {
        let schema = schema_with(json!({ "17::image": { "type": "string" } }));
        let applied = apply_field_widgets(
            &schema,
            &[FieldWidgetChange {
                key: "17::image".to_string(),
                widget: "file".to_string(),
                options: vec![],
            }],
        );
        assert_eq!(
            applied.schema["properties"]["17::image"]["format"],
            json!("file")
        );
        assert_eq!(applied.effective["17::image"], (Widget::File, true));
    }

    /// 引擎给的范围留着：那是**信息**不是约束（组件不会拿它拦人，但用户看得见）。
    #[test]
    fn engine_ranges_survive_a_widget_change() {
        let schema = schema_with(json!({
            "3::steps": { "type": "integer", "minimum": 1.0, "maximum": 10000.0, "multipleOf": 1.0 }
        }));
        let applied = apply_field_widgets(
            &schema,
            &[FieldWidgetChange {
                key: "3::steps".to_string(),
                widget: "number".to_string(),
                options: vec![],
            }],
        );
        let field = &applied.schema["properties"]["3::steps"];
        assert_eq!(field["type"], json!("number"));
        assert_eq!(field["minimum"], json!(1.0));
        assert_eq!(field["maximum"], json!(10000.0));
    }
}
