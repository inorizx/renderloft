//! 连接的**可选运行参数**：只有用户环境才知道的那几个量。
//!
//! 判据是"这个值是不是**用户环境**决定的"，而不是"我们懒得做成配置"：
//!
//! * **请求超时**：一条走代理的远端 ComfyUI、一次要传 200 MB 素材的提交——多慢算正常，
//!   只有用户知道。写死 120 秒时，慢的那条连接**永远失败**，而用户没有任何办法。
//! * **跟踪上限**：远端任务多久没进终态才算"我们不跟了"。写死 1 小时时，跑 90 分钟的视频
//!   工作流会被**本地判失败**（`TRACKING_EXHAUSTED`，见 `application/reconcile.rs`），
//!   而远端其实还在跑——这是真会咬人的那种。
//!
//! 两条规矩：
//!
//! 1. **默认值 = 今天的行为**（不填与现在完全一样），所以这对现有用户零影响；
//! 2. **夹紧**：填 0 或 100 年都不能让系统失去意义。超时 10 秒~1 小时、跟踪上限
//!    1 分钟~24 小时，越界夹到边界（UI 的提示里写明这个范围，所以不是"静默改数"）。
//!    非法值（非数字、负数、0）当**没填**处理，回落默认值。
//!
//! 放在 domain 而不是 application：**infrastructure 的 driver 也要读它**（超时在 driver 里
//! 用），而 driver 不得 import application（`scripts/check-layers.mjs` 会拦）。
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// 平台请求超时。默认与今天一致（各 driver 里的 `from_secs(120)`）。
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;
pub const MIN_REQUEST_TIMEOUT_SECS: u64 = 10;
pub const MAX_REQUEST_TIMEOUT_SECS: u64 = 3600;

/// 对账死线。默认与今天一致（`TRACKING_DEADLINE_MILLIS` = 1 小时）。
pub const DEFAULT_TRACKING_DEADLINE_SECS: u64 = 3600;
pub const MIN_TRACKING_DEADLINE_SECS: u64 = 60;
pub const MAX_TRACKING_DEADLINE_SECS: u64 = 24 * 3600;

/// 默认死线的毫秒形态。
///
/// 这个数字原来在**四处**各写了一遍（`scheduler.rs` / `reconcile.rs` / `sqlite/exec.rs` /
/// `infrastructure/fakes.rs`，其中 reconcile 那处自己写着"与 scheduler 同值；此处独立声明"）。
/// 现在只有这一处：`Run::apply_tracking` 与两处恢复路径都引它。
pub const DEFAULT_TRACKING_DEADLINE_MILLIS: u64 = DEFAULT_TRACKING_DEADLINE_SECS * 1000;

/// 连接 `config` 里的键名。**与 UI 上的字段一一对应**（连接抽屉的高级项），改一处要改两处。
pub const REQUEST_TIMEOUT_KEY: &str = "requestTimeoutSeconds";
pub const TRACKING_DEADLINE_KEY: &str = "trackingDeadlineSeconds";

/// 读一个"秒"配置：非法/0/负数 ⇒ `None`（= 没填），合法则夹进 `[min, max]`。
///
/// 字符串也认（`"300"`）：表单历史上会把数字传成字符串，判据按**值**不按 JSON 类型。
fn read_secs(config: &BTreeMap<String, Value>, key: &str, min: u64, max: u64) -> Option<u64> {
    let value = match config.get(key)? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }?;
    if value == 0 {
        return None;
    }
    Some(value.clamp(min, max))
}

/// 平台请求超时。
///
/// `site_default_secs` 是**这个调用点**的默认值：各端点的预算本来就不同（提交/上传可以长，
/// 探活故意短——"探活卡两分钟比探活失败更糟"）。所以这里只提供"用户填了就用用户的"，
/// 不把各处的默认统一掉。
pub fn request_timeout(config: &BTreeMap<String, Value>, site_default_secs: u64) -> Duration {
    let secs = read_secs(
        config,
        REQUEST_TIMEOUT_KEY,
        MIN_REQUEST_TIMEOUT_SECS,
        MAX_REQUEST_TIMEOUT_SECS,
    )
    .unwrap_or(site_default_secs);
    Duration::from_secs(secs)
}

/// 用户**显式配了**的对账死线（毫秒）；没配是 `None`。
///
/// 单独留一个"配了没有"的问法，是给"只在配了的时候多写一次"的调用方用的
/// （`application/scheduler.rs` 提交成功后按它写 run 的死线）：拿"生效值"去和默认值比大小
/// 来判断"配没配"是间接的，而且用户真填了 3600 时那个判断会失效。
pub fn configured_tracking_deadline_millis(config: &BTreeMap<String, Value>) -> Option<u64> {
    read_secs(
        config,
        TRACKING_DEADLINE_KEY,
        MIN_TRACKING_DEADLINE_SECS,
        MAX_TRACKING_DEADLINE_SECS,
    )
    .map(|secs| secs * 1000)
}

/// 对账死线（毫秒）。没配就是默认 1 小时。
pub fn tracking_deadline_millis(config: &BTreeMap<String, Value>) -> u64 {
    configured_tracking_deadline_millis(config).unwrap_or(DEFAULT_TRACKING_DEADLINE_SECS * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn unset_falls_back_to_the_call_site_default() {
        let empty = config(&[]);
        // 同一个连接、两个不同预算的调用点：各拿各的默认（探活故意短）。
        assert_eq!(request_timeout(&empty, 120), Duration::from_secs(120));
        assert_eq!(request_timeout(&empty, 5), Duration::from_secs(5));
        assert_eq!(
            tracking_deadline_millis(&empty),
            DEFAULT_TRACKING_DEADLINE_SECS * 1000
        );
    }

    #[test]
    fn configured_value_wins_over_the_call_site_default() {
        let configured = config(&[(REQUEST_TIMEOUT_KEY, json!(600))]);
        assert_eq!(request_timeout(&configured, 120), Duration::from_secs(600));
        assert_eq!(request_timeout(&configured, 5), Duration::from_secs(600));

        let deadline = config(&[(TRACKING_DEADLINE_KEY, json!(7200))]);
        assert_eq!(tracking_deadline_millis(&deadline), 7_200_000);
    }

    /// 表单来的值历史上是字符串（见 `Connections.vue` 的 `form.*`），按值认、不按类型认。
    #[test]
    fn string_numbers_are_accepted() {
        let configured = config(&[(REQUEST_TIMEOUT_KEY, json!("300"))]);
        assert_eq!(request_timeout(&configured, 120), Duration::from_secs(300));
    }

    /// 非法值当"没填"：非数字、负数、0 都不该把系统变成"立刻超时"或"永不超时"。
    #[test]
    fn junk_and_zero_fall_back_to_the_default() {
        for value in [json!("abc"), json!(-5), json!(0), json!(true), json!(null)] {
            let junk = config(&[(REQUEST_TIMEOUT_KEY, value.clone())]);
            assert_eq!(
                request_timeout(&junk, 120),
                Duration::from_secs(120),
                "{value} 应回落默认"
            );
            let junk = config(&[(TRACKING_DEADLINE_KEY, value.clone())]);
            assert_eq!(
                tracking_deadline_millis(&junk),
                DEFAULT_TRACKING_DEADLINE_SECS * 1000,
                "{value} 应回落默认"
            );
        }
    }

    /// 越界夹到边界：用户填 1 秒（每张图都超时）或 100 年（永远不结束）都不该生效。
    #[test]
    fn out_of_range_values_are_clamped() {
        let too_small = config(&[(REQUEST_TIMEOUT_KEY, json!(1))]);
        assert_eq!(
            request_timeout(&too_small, 120),
            Duration::from_secs(MIN_REQUEST_TIMEOUT_SECS)
        );
        let too_large = config(&[(REQUEST_TIMEOUT_KEY, json!(999_999))]);
        assert_eq!(
            request_timeout(&too_large, 120),
            Duration::from_secs(MAX_REQUEST_TIMEOUT_SECS)
        );

        let too_small = config(&[(TRACKING_DEADLINE_KEY, json!(5))]);
        assert_eq!(
            tracking_deadline_millis(&too_small),
            MIN_TRACKING_DEADLINE_SECS * 1000
        );
        let too_large = config(&[(TRACKING_DEADLINE_KEY, json!(999_999_999))]);
        assert_eq!(
            tracking_deadline_millis(&too_large),
            MAX_TRACKING_DEADLINE_SECS * 1000
        );
    }
}
