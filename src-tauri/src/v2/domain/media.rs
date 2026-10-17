use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use super::errors::{DomainResult, SafeError};
use super::ids::{ArtifactId, AssetId, RunId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    #[default]
    Asset,
    Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaObject {
    pub id: MediaObjectId,
    pub kind: MediaKind,
    pub filename: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub checksum: String,
    pub storage_key: Option<String>,
    pub provider_refs: Vec<ProviderObjectRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MediaObjectId {
    Asset(AssetId),
    Artifact(ArtifactId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderObjectRef {
    pub provider: String,
    pub remote_id: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    pub asset_id: AssetId,
    pub checksum: String,
    /// 这个资产被绑定到哪个**媒体字段**（字段键 `<节点>::<字段>`）。
    ///
    /// ⚠️ **第一性原理**：资产的真实身份是「(recipe revision, 字段键)」——不是数组下标。
    /// 上游节点声明的顺序、schema 的键序、调用方的传参顺序，谁都不该是语义；位置数组
    /// 只是旧的传输编码（为什么错：见 `MediaMapping`）。
    ///
    /// `None` = "按位置来的"（旧 HTTP 数组、旧 assets_json 记录）。它**只能**在 revision
    /// 仍是旧 `asset-N` 映射时被解释成字段键，判定集中在 `MediaMapping::bind`——不许在
    /// 别处补一个默认值，那正是错位再次发生的入口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_key: Option<String>,
}

impl AssetRef {
    pub fn new(asset_id: AssetId, checksum: impl Into<String>) -> Self {
        Self {
            asset_id,
            checksum: checksum.into(),
            field_key: None,
        }
    }

    /// 绑上字段键（`<节点>::<字段>`）。
    pub fn with_field_key(mut self, field_key: impl Into<String>) -> Self {
        self.field_key = Some(field_key.into());
        self
    }
}

// ---------- 媒体映射：资产按字段键定位 ----------

/// 字段键的分隔符：`<节点>::<字段>`（导入时就是这么造的，界面上显示的也是它）。
pub const FIELD_KEY_SEPARATOR: &str = "::";

/// 由节点与字段拼出字段键。
pub fn field_key(node: &str, field: &str) -> String {
    format!("{node}{FIELD_KEY_SEPARATOR}{field}")
}

/// 一个媒体槽位：资产最终要写进**哪个节点、哪个字段**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSlot {
    pub node: String,
    pub field: String,
}

/// 归一化后的 `provider_config.mediaMapping`。
///
/// ⚠️ 为什么必须有这一层（这条修复的根因）：旧实现里资产的语义由**数组下标**隐式决定
/// （`asset-{index}` 当作槽位名），而上游节点顺序、schema 键序、调用方顺序各自可独立变化
/// ——三者谁都不是资产的身份。实测：一条 4 槽 RunningHub 换装配方，上游节点顺序是
/// `475(图1)、519(图4)、520(图2)、521(图3)`，导入按上游顺序生成 `asset-0..asset-3`，
/// 而 schema 的 properties 按字典序展示恰好又是 `475<519<520<521` ⇒ 界面第二个格子
/// 其实是 519。调用方只填「图1+图2」时按位置落进了 519。
///
/// 现在的规则：**身份 = 字段键**。旧形状（`asset-N` → `{node, field}`）只作为兼容编码
/// 存在——读的时候按 N 的数字序解释，再用值里的 node/field 还原成字段键；新写入的一律
/// 是字段键。对 keyed revision 收到位置数组时**明确拒绝**（`POSITIONAL_ASSETS_UNSUPPORTED`），
/// 不许猜。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaMapping {
    /// 字段键 → 槽位（两种形状统一后的规范形式）。
    slots: BTreeMap<String, MediaSlot>,
    /// 旧形状的**位置序**：`asset-0, asset-1, …` 按数字升序展开出的字段键。
    /// 只对旧 revision 是 `Some`；新形状是 `None`（于是位置数组被明确拒绝，而不是猜）。
    positional: Option<Vec<String>>,
}

/// 一个已绑定的资产：字段键 + 槽位，来自 `MediaMapping::bind`。
#[derive(Debug, Clone)]
pub struct BoundAsset<'a> {
    pub asset: &'a AssetRef,
    pub field_key: String,
    pub slot: MediaSlot,
}

impl MediaMapping {
    pub fn from_provider_config(config: &BTreeMap<String, Value>) -> DomainResult<Self> {
        Self::from_value(config.get("mediaMapping"))
    }

    /// 解析映射表。**不猜**：认不出的形状一律报 `MEDIA_MAPPING_INVALID`，
    /// 而不是"跳过这一条"——静默跳过等于把资产悄悄丢掉（那正是旧实现最坏的失效方式）。
    pub fn from_value(value: Option<&Value>) -> DomainResult<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let Some(object) = value.as_object() else {
            return Err(SafeError::validation(
                "MEDIA_MAPPING_INVALID",
                "mediaMapping 必须是一个对象：键 = 字段键（\"<节点>::<字段>\"）或旧槽位名（\"asset-N\"），\
                 值 = { \"node\": \"<节点>\", \"field\": \"<字段>\" }",
            ));
        };
        let mut legacy: BTreeMap<usize, (String, MediaSlot)> = BTreeMap::new();
        let mut keyed: BTreeMap<String, MediaSlot> = BTreeMap::new();
        for (key, entry) in object {
            let slot = self::slot_from_entry(key, entry)?;
            match legacy_index(key) {
                Some(index) => {
                    let derived = field_key(&slot.node, &slot.field);
                    legacy.insert(index, (derived, slot));
                }
                None => {
                    keyed.insert(key.trim().to_string(), slot);
                }
            }
        }
        // 形状混用（一部分 asset-N、一部分字段键）：分不清哪些该按位置解释 ⇒ 拒绝，不猜。
        if !legacy.is_empty() && !keyed.is_empty() {
            return Err(SafeError::validation(
                "MEDIA_MAPPING_INVALID",
                "mediaMapping 混用了旧槽位名（asset-N）与新字段键。两种形状不许混：\
                 要么整张表都是 asset-N（旧 revision），要么整张表都是字段键（新 revision）。",
            ));
        }
        let mut slots: BTreeMap<String, MediaSlot> = BTreeMap::new();
        let mut positional: Option<Vec<String>> = None;
        if !legacy.is_empty() {
            let mut order = Vec::with_capacity(legacy.len());
            for (_, (key, slot)) in legacy {
                if slots.insert(key.clone(), slot).is_some() {
                    return Err(SafeError::validation(
                        "MEDIA_MAPPING_INVALID",
                        format!("mediaMapping 里有两个旧槽位指向同一个字段 `{key}`；一个字段只能有一个槽位。"),
                    ));
                }
                order.push(key);
            }
            positional = Some(order);
        }
        for (key, slot) in keyed {
            if key.is_empty() {
                return Err(SafeError::validation(
                    "MEDIA_MAPPING_INVALID",
                    "mediaMapping 里有空字符串字段键；键必须是 \"<节点>::<字段>\"。",
                ));
            }
            slots.insert(key, slot);
        }
        Ok(Self { slots, positional })
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn slot(&self, field_key: &str) -> Option<&MediaSlot> {
        self.slots.get(field_key)
    }

    /// 这个 revision 接受的媒体字段键（**错误信息里要原样列出来**：调用方据此改）。
    pub fn field_keys(&self) -> impl Iterator<Item = &String> {
        self.slots.keys()
    }

    /// 旧形状的位置序（`asset-N` 数字序展开出的字段键）；新形状是 `None`。
    pub fn positional(&self) -> Option<&[String]> {
        self.positional.as_deref()
    }

    fn describe_keys(&self) -> String {
        if self.slots.is_empty() {
            return "（它没有任何媒体字段）".to_string();
        }
        self.slots
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("、")
    }

    /// 把一组资产引用绑到槽位上——**两种形状的统一入口**：
    /// * 全部带字段键（新形状）：逐个校验键属于本 revision，重复键报错；
    /// * 全部不带字段键（旧位置数组）：仅当本 revision 仍是旧 `asset-N` 映射时按下标解释，
    ///   否则报 `POSITIONAL_ASSETS_UNSUPPORTED`；
    /// * 混着来：报错（不许猜哪一半按位置算）。
    ///
    /// 返回顺序与输入一致（调用方可以按位取回自己的 `bytes`）。
    pub fn bind<'a>(&self, assets: &'a [AssetRef]) -> DomainResult<Vec<BoundAsset<'a>>> {
        if assets.is_empty() {
            return Ok(Vec::new());
        }
        let keyed_count = assets
            .iter()
            .filter(|asset| asset.field_key.is_some())
            .count();
        if keyed_count != 0 && keyed_count != assets.len() {
            return Err(SafeError::validation(
                "MEDIA_MAPPING_INVALID",
                "同一组 assets 里混了两种形状：一部分带字段键、一部分没有。要么整组都用字段键\
                 （对象形状 {\"<字段键>\": {assetId, checksum}}），要么整组都是旧位置数组。",
            ));
        }
        if keyed_count == assets.len() {
            let mut seen = BTreeSet::new();
            let mut bound = Vec::with_capacity(assets.len());
            for asset in assets {
                let key = asset.field_key.clone().unwrap_or_default();
                if !seen.insert(key.clone()) {
                    return Err(SafeError::validation(
                        "DUPLICATE_ASSET_FIELD",
                        format!("同一个字段键 `{key}` 在一个 run 里给了多个资产；一个字段只能绑一个资产。"),
                    ));
                }
                let Some(slot) = self.slots.get(&key) else {
                    return Err(SafeError::validation(
                        "ASSET_FIELD_UNKNOWN",
                        format!(
                            "这个 revision 没有媒体字段 `{key}`。它接受的字段键：{}。\
                             字段键来自 recipe.describe 的 inputSchema（format=\"file\" 的那些键）。",
                            self.describe_keys()
                        ),
                    )
                    .with_details(BTreeMap::from([(
                        "allowedFields".to_string(),
                        serde_json::json!(self
                            .slots
                            .keys()
                            .map(String::as_str)
                            .collect::<Vec<_>>()),
                    )])));
                };
                bound.push(BoundAsset {
                    asset,
                    field_key: key,
                    slot: slot.clone(),
                });
            }
            return Ok(bound);
        }
        // 旧位置形状：只在旧映射上可解释。
        let Some(order) = &self.positional else {
            return Err(SafeError::validation(
                "POSITIONAL_ASSETS_UNSUPPORTED",
                format!(
                    "这个 revision 的资产按**字段键**定位，不接受位置数组（assets 给了 {} 个元素）。\
                     请改成对象形状：{{ \"<字段键>\": {{ \"assetId\": …, \"checksum\": … }} }}——\
                     它接受的字段键：{}。",
                    assets.len(),
                    self.describe_keys()
                ),
            ));
        };
        if assets.len() > order.len() {
            return Err(SafeError::validation(
                "ASSET_COUNT_EXCEEDED",
                format!(
                    "请求带了 {} 个资产，但这个 revision 只有 {} 个媒体槽位（{}）。\
                     去掉多余的资产，或者改用对象形状只传有值的槽位。",
                    assets.len(),
                    order.len(),
                    order.join("、")
                ),
            ));
        }
        let mut bound = Vec::with_capacity(assets.len());
        for (index, asset) in assets.iter().enumerate() {
            let key = order[index].clone();
            // 位置序与槽位表由同一次解析生成，理论上必然命中；这里仍然显式报错而不是
            // 补一个默认值——"缺槽位"绝不允许被静默兜底成写进一个不存在的节点。
            let slot = self.slots.get(&key).cloned().ok_or_else(|| {
                SafeError::validation(
                    "MEDIA_MAPPING_INVALID",
                    format!("mediaMapping 的位置序引用了不存在的字段 `{key}`"),
                )
            })?;
            bound.push(BoundAsset {
                asset,
                field_key: key,
                slot,
            });
        }
        Ok(bound)
    }
}

/// `asset-N` → `Some(N)`；其它键一律 `None`（是字段键）。
fn legacy_index(key: &str) -> Option<usize> {
    key.strip_prefix("asset-")?.parse::<usize>().ok()
}

/// 从一条映射项里读出槽位。值里有 node/field 就用它；键是 `<节点>::<字段>` 形状时两处
/// **必须一致**（不一致 = 一个资产两个去处，是数据错误，不许猜）。
fn slot_from_entry(key: &str, entry: &Value) -> DomainResult<MediaSlot> {
    let from_key = key
        .split_once(FIELD_KEY_SEPARATOR)
        .filter(|(node, field)| !node.trim().is_empty() && !field.trim().is_empty())
        .map(|(node, field)| MediaSlot {
            node: node.to_string(),
            field: field.to_string(),
        });
    let Some(entry) = entry.as_object() else {
        return Err(SafeError::validation(
            "MEDIA_MAPPING_INVALID",
            format!("mediaMapping 的 `{key}` 必须是一个对象 {{ \"node\": …, \"field\": … }}。"),
        ));
    };
    let node = entry.get("node").and_then(Value::as_str);
    let field = entry.get("field").and_then(Value::as_str);
    match (node, field, from_key) {
        (Some(node), Some(field), derived)
            if !node.trim().is_empty() && !field.trim().is_empty() =>
        {
            let slot = MediaSlot {
                node: node.to_string(),
                field: field.to_string(),
            };
            if let Some(derived) = derived {
                if derived != slot {
                    return Err(SafeError::validation(
                        "MEDIA_MAPPING_INVALID",
                        format!(
                            "mediaMapping 的 `{key}` 自相矛盾：键说的是 `{derived_node}::{derived_field}`，\
                             值里的 node/field 却是 `{node}::{field}`。一个资产只能有一个去处，修好它再提交。",
                            derived_node = derived.node,
                            derived_field = derived.field,
                        ),
                    ));
                }
            }
            Ok(slot)
        }
        // 值里没写 node/field：键本身是 `<节点>::<字段>` 就由键推出来（不额外猜）。
        (_, _, Some(derived)) => Ok(derived),
        _ => Err(SafeError::validation(
            "MEDIA_MAPPING_INVALID",
            format!(
                "mediaMapping 的 `{key}` 读不出槽位：值必须是 {{ \"node\": \"<节点>\", \"field\": \"<字段>\" }}，\
                 或者键本身是 \"<节点>::<字段>\" 形状。"
            ),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub run_id: RunId,
    pub filename: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub checksum: String,
    pub storage_key: String,
    pub provider_refs: Vec<ProviderObjectRef>,
}

impl Artifact {
    pub fn validate(&self) -> Result<(), SafeError> {
        if self.filename.trim().is_empty() {
            return Err(SafeError::validation(
                "ARTIFACT_FILENAME_REQUIRED",
                "Artifact 必须包含文件名",
            ));
        }
        if self.checksum.trim().is_empty() {
            return Err(SafeError::validation(
                "ARTIFACT_CHECKSUM_REQUIRED",
                "Artifact 必须包含校验和",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_validation_requires_filename_and_checksum() {
        let artifact = Artifact {
            id: ArtifactId(1),
            run_id: RunId(1),
            filename: String::new(),
            mime_type: "image/png".to_string(),
            byte_size: 0,
            checksum: String::new(),
            storage_key: "key".to_string(),
            provider_refs: vec![],
        };
        assert!(artifact.validate().is_err());
    }

    #[test]
    fn artifact_validation_accepts_valid_artifact() {
        let artifact = Artifact {
            id: ArtifactId(1),
            run_id: RunId(1),
            filename: "out.png".to_string(),
            mime_type: "image/png".to_string(),
            byte_size: 10,
            checksum: "abc".to_string(),
            storage_key: "key".to_string(),
            provider_refs: vec![],
        };
        assert!(artifact.validate().is_ok());
    }

    // ---------- 媒体映射：资产按字段键定位 ----------

    use serde_json::json;

    fn provider_config(mapping: Value) -> BTreeMap<String, Value> {
        BTreeMap::from([("mediaMapping".to_string(), mapping)])
    }

    /// 用户报的那条 4 槽配方：上游节点顺序 475(图1)、519(图4)、520(图2)、521(图3)。
    fn qwen_mapping() -> Value {
        json!({
            "asset-0": { "node": "475", "field": "image" },
            "asset-1": { "node": "519", "field": "image" },
            "asset-2": { "node": "520", "field": "image" },
            "asset-3": { "node": "521", "field": "image" }
        })
    }

    #[test]
    fn legacy_mapping_derives_field_keys_and_keeps_positional_order() {
        let mapping =
            MediaMapping::from_provider_config(&provider_config(qwen_mapping())).expect("旧映射应可读");
        let keys: Vec<&str> = mapping.field_keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["475::image", "519::image", "520::image", "521::image"]);
        assert_eq!(
            mapping.positional().expect("旧映射有位置序"),
            ["475::image", "519::image", "520::image", "521::image"]
        );
        assert_eq!(
            mapping.slot("519::image"),
            Some(&MediaSlot {
                node: "519".to_string(),
                field: "image".to_string()
            })
        );
    }

    /// 回归（用户场景）：旧位置数组的第一个元素必须落在**上游声明的第一个槽位**（475），
    /// 而不是 schema 字典序里的第二个。
    #[test]
    fn legacy_positional_array_maps_by_asset_slot_order() {
        let mapping =
            MediaMapping::from_provider_config(&provider_config(qwen_mapping())).expect("旧映射应可读");
        let assets = vec![AssetRef::new(AssetId(7), "sum-a")];
        let bound = mapping.bind(&assets).expect("旧数组应可解释");
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].field_key, "475::image");
        assert_eq!(bound[0].slot.node, "475");
    }

    /// keyed 形状：只传 475 + 520 时恰好绑上这两个槽位——**顺序与位置都不决定语义**。
    #[test]
    fn keyed_assets_bind_by_field_key_regardless_of_order() {
        let mapping = MediaMapping::from_value(Some(&json!({
            "475::image": { "node": "475", "field": "image" },
            "519::image": { "node": "519", "field": "image" },
            "520::image": { "node": "520", "field": "image" },
            "521::image": { "node": "521", "field": "image" }
        })))
        .expect("keyed 映射应可读");
        assert_eq!(mapping.positional(), None, "keyed 映射没有位置序");

        let assets = vec![
            AssetRef::new(AssetId(2), "sum-cloth").with_field_key("520::image"),
            AssetRef::new(AssetId(1), "sum-person").with_field_key("475::image"),
        ];
        let bound = mapping.bind(&assets).expect("两个已知键都应绑上");
        assert_eq!(bound.len(), 2);
        assert_eq!(bound[0].field_key, "520::image");
        assert_eq!(bound[0].slot.node, "520");
        assert_eq!(bound[1].field_key, "475::image");
        assert_eq!(bound[1].slot.node, "475");
    }

    #[test]
    fn keyed_revision_rejects_positional_array_with_a_stable_code() {
        let mapping = MediaMapping::from_value(Some(&json!({
            "475::image": { "node": "475", "field": "image" }
        })))
        .expect("应可读");
        let error = mapping
            .bind(&[AssetRef::new(AssetId(1), "sum")])
            .expect_err("keyed revision 收到位置数组必须被拒");
        assert_eq!(error.code, "POSITIONAL_ASSETS_UNSUPPORTED");
        assert!(
            error.message.contains("475::image"),
            "错误信息要告诉调用方接受的字段键：{}",
            error.message
        );
    }

    #[test]
    fn unknown_field_key_is_named_with_the_allowed_set() {
        let mapping = MediaMapping::from_value(Some(&json!({
            "475::image": { "node": "475", "field": "image" }
        })))
        .expect("应可读");
        let error = mapping
            .bind(&[AssetRef::new(AssetId(1), "sum").with_field_key("520::image")])
            .expect_err("未知键必须报错");
        assert_eq!(error.code, "ASSET_FIELD_UNKNOWN");
        assert!(error.message.contains("520::image"), "{}", error.message);
        assert!(error.message.contains("475::image"), "{}", error.message);
        assert_eq!(
            error.details.get("allowedFields"),
            Some(&json!(["475::image"])),
            "机器可读的允许集合也要给"
        );
    }

    #[test]
    fn duplicate_field_key_is_rejected() {
        let mapping = MediaMapping::from_value(Some(&json!({
            "475::image": { "node": "475", "field": "image" }
        })))
        .expect("应可读");
        let error = mapping
            .bind(&[
                AssetRef::new(AssetId(1), "a").with_field_key("475::image"),
                AssetRef::new(AssetId(2), "b").with_field_key("475::image"),
            ])
            .expect_err("同一字段两个资产必须被拒");
        assert_eq!(error.code, "DUPLICATE_ASSET_FIELD");
    }

    #[test]
    fn too_many_positional_assets_is_rejected_instead_of_silently_truncated() {
        let mapping =
            MediaMapping::from_provider_config(&provider_config(qwen_mapping())).expect("旧映射应可读");
        let assets = (0..5)
            .map(|index| AssetRef::new(AssetId(index), "sum"))
            .collect::<Vec<_>>();
        let error = mapping.bind(&assets).expect_err("超量必须报错");
        assert_eq!(error.code, "ASSET_COUNT_EXCEEDED");
    }

    #[test]
    fn mixed_shapes_are_rejected() {
        let mapping = MediaMapping::from_value(Some(&json!({
            "475::image": { "node": "475", "field": "image" }
        })))
        .expect("应可读");
        let error = mapping
            .bind(&[
                AssetRef::new(AssetId(1), "a").with_field_key("475::image"),
                AssetRef::new(AssetId(2), "b"),
            ])
            .expect_err("混形状必须被拒");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
    }

    #[test]
    fn malformed_mappings_are_rejected_not_skipped() {
        // 条目不是对象
        let error = MediaMapping::from_value(Some(&json!({ "475::image": "image" })))
            .expect_err("非对象条目必须被拒");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
        // 旧条目缺 node/field
        let error = MediaMapping::from_value(Some(&json!({ "asset-0": { "field": "image" } })))
            .expect_err("旧条目缺 node 必须被拒");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
        // 顶层的形状不是对象（例如误传数组）
        let error =
            MediaMapping::from_value(Some(&json!([{ "node": "1" }]))).expect_err("顶层必须是对象");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
        // 旧槽位与新字段键混用
        let error = MediaMapping::from_value(Some(&json!({
            "asset-0": { "node": "475", "field": "image" },
            "520::image": { "node": "520", "field": "image" }
        })))
        .expect_err("形状混用必须被拒");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
        // 键与值各说各的节点（一个资产两个去处）
        let error = MediaMapping::from_value(Some(&json!({
            "475::image": { "node": "520", "field": "image" }
        })))
        .expect_err("键值矛盾必须被拒");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
        // 两个旧槽位指向同一字段
        let error = MediaMapping::from_value(Some(&json!({
            "asset-0": { "node": "475", "field": "image" },
            "asset-1": { "node": "475", "field": "image" }
        })))
        .expect_err("重复槽位必须被拒");
        assert_eq!(error.code, "MEDIA_MAPPING_INVALID");
    }

    #[test]
    fn keyed_entry_without_node_field_can_be_derived_from_the_key() {
        let mapping = MediaMapping::from_value(Some(&json!({ "17::image": {} })))
            .expect("键形状够用时不许硬要值里再写一遍");
        assert_eq!(
            mapping.slot("17::image"),
            Some(&MediaSlot {
                node: "17".to_string(),
                field: "image".to_string()
            })
        );
    }

    #[test]
    fn missing_mapping_is_an_empty_mapping_not_an_error() {
        let mapping = MediaMapping::from_provider_config(&BTreeMap::new()).expect("缺省不是错误");
        assert!(mapping.is_empty());
        // 没有媒体字段时收到任何资产都是明确错误，不是静默忽略。
        let error = mapping
            .bind(&[AssetRef::new(AssetId(1), "a").with_field_key("475::image")])
            .expect_err("没有媒体字段时资产必须被拒");
        assert_eq!(error.code, "ASSET_FIELD_UNKNOWN");
    }

    #[test]
    fn asset_ref_roundtrips_with_and_without_field_key() {
        // 旧记录（没有 field_key）必须还能反序列化。
        let legacy: AssetRef = serde_json::from_value(json!({ "asset_id": 3, "checksum": "abc" }))
            .expect("旧 assets_json 必须可读");
        assert_eq!(legacy.field_key, None);
        // 新记录带字段键。
        let keyed: AssetRef =
            serde_json::from_value(json!({ "asset_id": 3, "checksum": "abc", "field_key": "475::image" }))
                .expect("新记录应可读");
        assert_eq!(keyed.field_key.as_deref(), Some("475::image"));
        // None 不序列化出去（旧读者拿到的是旧形状）。
        let text = serde_json::to_string(&legacy).expect("应可序列化");
        assert!(!text.contains("field_key"), "{text}");
    }
}
