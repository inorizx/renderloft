//! 凭据引用：一条连接可以**借用**另一条（RunningHub）连接的密钥，而不是复制一份。
//!
//! 要解决的问题：ComfyUI 跑在 RunningHub 云端（`baseUrl` 带 `/proxy`）时，用的**就是**
//! RunningHub 那把 Key。此前只能让用户回控制台再复制一遍 ⇒ 同一份密钥在凭据库里存了两份：
//! 轮换要改两处、删一条另一条悄悄失效。
//!
//! 做法（关键一条：**引用的是"绑定"，不是"密钥"**）：连接的 `config.sourceConnectionId`
//! 指向一条 RunningHub 连接；**执行前**把被引用连接的 `secret_bindings` 拿过来用。
//! 于是：
//!   - driver 一行都不用改（它照旧解析自己 revision 上的绑定）；
//!   - 密钥本体不出凭据库（借过来的是槽位名，值仍由 driver 向凭据库要）；
//!   - 轮换自动生效（槽位没变，值变了）；
//!   - 被引用的连接**删掉/停用会立刻报错**，而不是悄悄坏。
//!
//! ⚠️ 为什么不把绑定**复制**进这条连接的 revision：那就是"存了两份"的老问题换了个位置——
//! 引用方会在被引用方轮换后继续用旧槽位，而槽位一旦被删就永久指向空气。
//!
//! ⚠️ 为什么只在**执行边界**解析，而不是塞进 `ConnectionRepository::find_revision`：
//! 那条路也被**写路径**用着（`UpdateConnection` 会拿当前 revision 的 `secret_bindings`
//! 去组装新 revision）——在那里解析会把借来的槽位**烤进**用户自己的配置里，
//! 于是"删掉引用"之后它还在用别人的槽位。执行边界解析即使漏了一处，后果也只是
//! "拿不到密钥 ⇒ 明确报错"，而不是静默写坏配置。
//!
//! 解析点（改这里时一起看）：
//!   - `application/scheduler.rs`（认领后提交）
//!   - `application/reconcile.rs`（对账）
//!   - `application/cancel_operation.rs`（远端取消）
//!   - `transports/http/handlers.rs`（`check_connection` 测试按钮、`get_connection` 的密钥指纹）
//!
//! 为什么只有"非 RunningHub 连接"能引用：引用是为了让 ComfyUI 复用 RH 的密钥；而
//! RunningHub 自己的连接**就是**密钥的宿主，让它再引用别人没有产品路径，却会造出
//! 一批"读连接但不解析引用"的旁路（导入工作流、账号状态），那些路会静默用错密钥。
//! 所以写入口直接拒绝（`CONNECTION_SOURCE_NOT_ALLOWED`），而不是让它在半路上失灵。

use serde_json::Value;
use std::collections::BTreeMap;

use crate::v2::domain::errors::{DomainResult, SafeError};
use crate::v2::domain::ids::ConnectionId;
use crate::v2::domain::model::{ConnectionRevision, ConnectionStatus};
use crate::v2::ports::repositories::ConnectionRepository;

/// 连接 config 里的引用键。**引用的是连接 id**（不是密钥、不是槽位名）。
pub const SOURCE_CONNECTION_ID: &str = "sourceConnectionId";

/// 密钥宿主（被引用方）必须是这个 vendor。
const SOURCE_VENDOR: &str = "runninghub";

/// 从 config 里读引用目标。
///
/// 类型不对时**显式报错**，不静默当成"没有引用"：`"3"`（字符串）这种手写配置如果被忽略，
/// 用户看到的是运行期「缺少 API Key」，而真正的原因是他把数字写成了字符串。
pub fn source_connection_id(
    config: &BTreeMap<String, Value>,
) -> DomainResult<Option<ConnectionId>> {
    match config.get(SOURCE_CONNECTION_ID) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|id| Some(ConnectionId(id)))
            .ok_or_else(|| {
                SafeError::validation(
                    "CONNECTION_CONFIG_INVALID",
                    format!("config.{SOURCE_CONNECTION_ID} 必须是连接 id（数字）"),
                )
            }),
    }
}

/// 把 revision 解析成**执行用的有效 revision**。
///
/// 没有引用时**原样返回**（零额外查询，老连接一行代码路径都不变）。
/// 有引用时：校验目标（存在、是 RunningHub、未停用、自身不再引用），
/// 然后把它的 `secret_bindings` 装到返回的 revision 上。
pub async fn resolve(
    connections: &dyn ConnectionRepository,
    mut revision: ConnectionRevision,
) -> DomainResult<ConnectionRevision> {
    let Some(source_id) = source_connection_id(&revision.config)? else {
        return Ok(revision);
    };
    let source = connections
        .find_connection(source_id)
        .await?
        .ok_or_else(|| {
            SafeError::validation(
                "CONNECTION_SOURCE_NOT_FOUND",
                format!(
                    "引用的连接 #{}({}) 不存在（可能已被删除），请重新选择或改为自己填 API Key",
                    source_id.0, SOURCE_CONNECTION_ID
                ),
            )
        })?;
    if source.vendor != SOURCE_VENDOR {
        return Err(SafeError::validation(
            "CONNECTION_SOURCE_INVALID",
            format!(
                "引用的连接 #{} 是 {} 类型，只有 RunningHub 连接可以作为密钥来源",
                source_id.0, source.vendor
            ),
        ));
    }
    if source.status != ConnectionStatus::Active {
        return Err(SafeError::validation(
            "CONNECTION_SOURCE_INACTIVE",
            format!(
                "引用的连接 #{}「{}」已停用，请启用它或改为自己填 API Key",
                source_id.0, source.display_name
            ),
        ));
    }
    let source_revision = connections
        .find_revision(source.current_revision_id)
        .await?
        .ok_or_else(|| {
            SafeError::not_found(
                "CONNECTION_REVISION_NOT_FOUND",
                format!("引用的连接 #{} 没有可用的 revision", source_id.0),
            )
        })?;
    // 只跟一层。被引用方自己也带引用 ⇒ 拒绝而不是"再跟一层"：链式引用会让
    // "到底用了谁的密钥"变成一道需要展开的题，而它本来只要一句话能答。
    if source_connection_id(&source_revision.config)?.is_some() {
        return Err(SafeError::validation(
            "CONNECTION_SOURCE_CHAINED",
            format!(
                "引用的连接 #{} 自己也在引用别的连接，不支持链式引用",
                source_id.0
            ),
        ));
    }
    revision.secret_bindings = source_revision.secret_bindings;
    Ok(revision)
}

/// 写入口的校验：在**用户还能当场改**的时刻拦住明显错的引用。
///
/// 与 `resolve` 的分工：这里管"你现在就写错了"（指向自己、指向不存在的连接、
/// 指向非 RunningHub 的连接、RunningHub 连接不该引用别人）；`resolve` 管
/// "写的时候对、后来变了"（被引用方被删/被停用）——两处都要，因为只有后者能覆盖时间维度。
pub async fn validate_reference(
    connections: &dyn ConnectionRepository,
    vendor: &str,
    own_id: Option<ConnectionId>,
    config: &BTreeMap<String, Value>,
) -> DomainResult<()> {
    let Some(source_id) = source_connection_id(config)? else {
        return Ok(());
    };
    if vendor == SOURCE_VENDOR {
        return Err(SafeError::validation(
            "CONNECTION_SOURCE_NOT_ALLOWED",
            "RunningHub 连接本身就是密钥来源，不能再引用别的连接",
        ));
    }
    if own_id == Some(source_id) {
        return Err(SafeError::validation(
            "CONNECTION_SOURCE_SELF",
            "不能引用自己",
        ));
    }
    let source = connections
        .find_connection(source_id)
        .await?
        .ok_or_else(|| {
            SafeError::validation(
                "CONNECTION_SOURCE_NOT_FOUND",
                format!("引用的连接 #{} 不存在", source_id.0),
            )
        })?;
    if source.vendor != SOURCE_VENDOR {
        return Err(SafeError::validation(
            "CONNECTION_SOURCE_INVALID",
            format!(
                "引用的连接 #{} 是 {} 类型，只有 RunningHub 连接可以作为密钥来源",
                source_id.0, source.vendor
            ),
        ));
    }
    Ok(())
}

/// 这个错误是不是"凭据引用坏了"（**不可自愈**）。
///
/// 用途只有一个：对账循环要据此把 run **当场判死**，而不是把它当成一次网络抖动
/// （对账错误默认是"下一轮再试"）——引用目标被删/停用/改类型都不会自己好起来，
/// 等下去只会让用户在"跑了好久"之后才看到失败。
///
/// 只看 `resolve` 可能返回的那几个码。存储类错误（读库失败）刻意**不算**在内：
/// 那种是暂时的，该留着重试。
pub fn is_reference_error(error: &SafeError) -> bool {
    matches!(
        error.code.as_str(),
        "CONNECTION_SOURCE_NOT_FOUND"
            | "CONNECTION_SOURCE_INVALID"
            | "CONNECTION_SOURCE_INACTIVE"
            | "CONNECTION_SOURCE_CHAINED"
            | "CONNECTION_CONFIG_INVALID"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::domain::ids::ConnectionRevisionId;
    use crate::v2::domain::model::{
        Connection, ConnectionStatus, LifecycleFeatures, SecretBinding, SecretResolution, Timestamp,
    };
    use crate::v2::infrastructure::fakes::InMemoryRepositories;
    use serde_json::json;
    use std::sync::Arc;

    fn connection(id: u64, vendor: &str, revision_id: u64, status: ConnectionStatus) -> Connection {
        Connection {
            id: ConnectionId(id),
            vendor: vendor.to_string(),
            protocol: vendor.to_string(),
            display_name: format!("conn-{id}"),
            current_revision_id: ConnectionRevisionId(revision_id),
            status,
        }
    }

    fn revision(
        id: u64,
        connection_id: u64,
        config: Vec<(&str, Value)>,
        bindings: Vec<&str>,
    ) -> ConnectionRevision {
        ConnectionRevision {
            id: ConnectionRevisionId(id),
            connection_id: ConnectionId(connection_id),
            config: config
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            secret_bindings: bindings
                .into_iter()
                .map(|slot| SecretBinding {
                    slot: slot.to_string(),
                    resolution: SecretResolution::Current,
                })
                .collect(),
            effective_capabilities: vec!["workflow-execution".to_string()],
            lifecycle_features: LifecycleFeatures::default(),
            created_at: Timestamp("0".to_string()),
        }
    }

    /// 一条 RunningHub 连接（#1）+ 一条引用它的 ComfyUI 连接（#2）。
    fn environment(source_status: ConnectionStatus) -> Arc<InMemoryRepositories> {
        let repositories = Arc::new(InMemoryRepositories::default());
        repositories.seed_connection(
            connection(1, "runninghub", 11, source_status),
            vec![revision(
                11,
                1,
                vec![("baseUrl", json!("https://www.runninghub.cn"))],
                vec!["connection-1-api_key"],
            )],
        );
        repositories.seed_connection(
            connection(2, "comfyui", 22, ConnectionStatus::Active),
            vec![revision(
                22,
                2,
                vec![("baseUrl", json!("https://www.runninghub.cn/proxy"))],
                vec![],
            )],
        );
        repositories
    }

    fn referencing() -> ConnectionRevision {
        revision(
            22,
            2,
            vec![
                ("baseUrl", json!("https://www.runninghub.cn/proxy")),
                (SOURCE_CONNECTION_ID, json!(1)),
            ],
            vec![],
        )
    }

    #[tokio::test]
    async fn a_referencing_connection_borrows_the_source_bindings() {
        let repositories = environment(ConnectionStatus::Active);
        let resolved = resolve(repositories.as_ref(), referencing())
            .await
            .expect("引用应当解析成功");
        assert_eq!(
            resolved
                .secret_bindings
                .iter()
                .map(|binding| binding.slot.as_str())
                .collect::<Vec<_>>(),
            vec!["connection-1-api_key"]
        );
        // 配置本身不动：借来的是绑定，不是把 config 也换掉。
        assert_eq!(resolved.config.get(SOURCE_CONNECTION_ID), Some(&json!(1)));
    }

    #[tokio::test]
    async fn a_connection_without_a_reference_is_returned_untouched() {
        let repositories = environment(ConnectionStatus::Active);
        let own = revision(
            22,
            2,
            vec![("baseUrl", json!("http://127.0.0.1:8188"))],
            vec!["connection-2-api_key"],
        );
        let resolved = resolve(repositories.as_ref(), own.clone())
            .await
            .expect("没有引用时应当原样返回");
        assert_eq!(resolved.secret_bindings.len(), 1);
        assert_eq!(resolved.secret_bindings[0].slot, "connection-2-api_key");
        assert_eq!(resolved.id, own.id);
    }

    #[tokio::test]
    async fn a_deleted_source_is_a_clear_error_not_a_silent_fallback() {
        let repositories = environment(ConnectionStatus::Active);
        let broken = revision(
            22,
            2,
            vec![(SOURCE_CONNECTION_ID, json!(99))],
            vec!["connection-2-api_key"],
        );
        let error = resolve(repositories.as_ref(), broken)
            .await
            .expect_err("指向不存在的连接必须报错");
        assert_eq!(error.code, "CONNECTION_SOURCE_NOT_FOUND");
        // ⚠️ 关键：**不能**退回自己的绑定。回退会让"删了引用方"看起来没事，
        // 实际用的是另一把（可能已经作废的）Key——错得悄无声息。
        assert!(error.message.contains("#99"));
    }

    #[tokio::test]
    async fn a_non_runninghub_source_is_rejected() {
        let repositories = Arc::new(InMemoryRepositories::default());
        repositories.seed_connection(
            connection(3, "comfyui", 33, ConnectionStatus::Active),
            vec![revision(33, 3, vec![], vec!["connection-3-api_key"])],
        );
        // 指向 #3（存在，但是 ComfyUI）——不是"不存在"，所以报的必须是类型错。
        let wrong_type = revision(
            22,
            2,
            vec![(SOURCE_CONNECTION_ID, json!(3))],
            vec!["connection-2-api_key"],
        );
        let error = resolve(repositories.as_ref(), wrong_type)
            .await
            .expect_err("只有 RunningHub 连接可以当密钥来源");
        assert_eq!(error.code, "CONNECTION_SOURCE_INVALID");
    }

    #[tokio::test]
    async fn a_disabled_source_is_rejected() {
        let repositories = environment(ConnectionStatus::Disabled);
        let error = resolve(repositories.as_ref(), referencing())
            .await
            .expect_err("引用的连接停用后必须报错");
        assert_eq!(error.code, "CONNECTION_SOURCE_INACTIVE");
    }

    #[tokio::test]
    async fn a_chained_reference_is_rejected_instead_of_followed() {
        let repositories = Arc::new(InMemoryRepositories::default());
        repositories.seed_connection(
            connection(1, "runninghub", 11, ConnectionStatus::Active),
            vec![revision(
                11,
                1,
                vec![(SOURCE_CONNECTION_ID, json!(4))],
                vec!["connection-1-api_key"],
            )],
        );
        let error = resolve(repositories.as_ref(), referencing())
            .await
            .expect_err("被引用方自己也带引用时必须拒绝");
        assert_eq!(error.code, "CONNECTION_SOURCE_CHAINED");
    }

    #[tokio::test]
    async fn a_non_numeric_reference_is_a_config_error() {
        let repositories = environment(ConnectionStatus::Active);
        let broken = revision(22, 2, vec![(SOURCE_CONNECTION_ID, json!("1"))], vec![]);
        let error = resolve(repositories.as_ref(), broken)
            .await
            .expect_err("字符串 id 必须显式报错");
        assert_eq!(error.code, "CONNECTION_CONFIG_INVALID");
    }

    #[tokio::test]
    async fn writing_a_reference_is_validated_at_the_write_path() {
        let repositories = environment(ConnectionStatus::Active);
        let config = |value: Value| BTreeMap::from([(SOURCE_CONNECTION_ID.to_string(), value)]);

        // 指向自己
        let error = validate_reference(
            repositories.as_ref(),
            "comfyui",
            Some(ConnectionId(2)),
            &config(json!(2)),
        )
        .await
        .expect_err("自引用必须被拒");
        assert_eq!(error.code, "CONNECTION_SOURCE_SELF");

        // 指向不存在的连接
        let error = validate_reference(repositories.as_ref(), "comfyui", None, &config(json!(99)))
            .await
            .expect_err("指向不存在的连接必须被拒");
        assert_eq!(error.code, "CONNECTION_SOURCE_NOT_FOUND");

        // RunningHub 连接不该引用别人
        let error = validate_reference(
            repositories.as_ref(),
            "runninghub",
            Some(ConnectionId(1)),
            &config(json!(1)),
        )
        .await
        .expect_err("RunningHub 连接不能引用");
        assert_eq!(error.code, "CONNECTION_SOURCE_NOT_ALLOWED");

        // 正常情况放行
        validate_reference(
            repositories.as_ref(),
            "comfyui",
            Some(ConnectionId(2)),
            &config(json!(1)),
        )
        .await
        .expect("指向一条 RunningHub 连接应当放行");
    }
}
