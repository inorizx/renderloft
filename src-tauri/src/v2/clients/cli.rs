//! CLI 客户端 —— **从动词表渲染**（见 `verbs.rs`）。
//!
//! 这个文件里**不该再出现动词清单**：子命令树、参数、分发都由 `verbs.rs` 的表决定。
//! 加能力请改那张表。

use clap::ArgMatches;

use super::client::{cli_error, cli_success, ClientError, V2Client};
use super::verbs::{self, Args};

/// 解析命令行。`--help` / `--version` 由 clap 自己处理并退出。
pub fn parse_command() -> ArgMatches {
    verbs::cli_command().get_matches()
}

/// 执行一条命令，返回给调用方打印的 JSON 信封。
pub async fn execute(matches: ArgMatches) -> String {
    let Some((group, group_matches)) = matches.subcommand() else {
        return cli_error("client.init", &ClientError::network("缺少子命令"));
    };
    let Some((action, verb_matches)) = group_matches.subcommand() else {
        return cli_error(
            "client.init",
            &ClientError::network(format!("{group} 缺少子命令")),
        );
    };
    let Some(verb) = verbs::find_by_cli(group, action) else {
        // 走不到：子命令树就是从表渲染出来的。留一条是为了"渲染与查表用了不同的键"时立刻可见。
        return cli_error(
            "client.init",
            &ClientError::network(format!("动词表里没有 {group} {action}")),
        );
    };
    let args = match Args::from_cli(verb, verb_matches) {
        Ok(args) => args,
        Err(error) => return cli_error(verb.id, &error),
    };
    // daemon 不在就自己拉起来（**启动，但不拥有**，见 `daemon_boot`）。
    // `daemon.*` 例外：那一组是"关于 daemon 本身"的动词（它在不在跑 / 让它停），
    // 先把它启动起来再回答就自相矛盾了（见 `Verb::needs_daemon`）。
    let needs_daemon = verb.needs_daemon();
    let boot = if needs_daemon {
        super::daemon_boot::ensure_running()
    } else {
        Ok(super::daemon_boot::EnsureOutcome::AlreadyRunning)
    };
    let client = match V2Client::from_endpoint_file() {
        Ok(client) => client,
        // daemon.* 在"连端点文件都没有"时也要能回答（例如 status 回 running:false），
        // 所以给它一个不指向任何东西的 client，由动词自己判断。
        Err(_) if !needs_daemon => V2Client::offline(),
        Err(error) => {
            let message = match boot {
                Err(boot_error) => format!("{error}（自动启动 daemon 也失败了：{boot_error}）"),
                Ok(_) => error,
            };
            return cli_error("client.init", &ClientError::network(message));
        }
    };
    match verbs::execute(&client, verb, &args).await {
        Ok(data) => cli_success(verb.id, data),
        Err(error) => cli_error(verb.id, &super::daemon_boot::explain_failure(error, &boot)),
    }
}

/// 打印信封，并返回**进程退出码**（0 = 成功，1 = 失败）。
///
/// ⚠️ 为什么必须区分——这是给调度器看的契约。README 明确建议"用 Windows 任务计划调用它"
/// （例如每天 9 点出一张图）。实测（2026-10-09）：改之前无论成功失败都返回 0，
/// 于是调度器会把失败当成功，`if ($LASTEXITCODE -ne 0) { 报警 }` 这类判断**永远不触发**。
/// 这是典型的静默失效：任务没出图，而日志里一切正常。
///
/// 顺带把两个流分开：成功信封走 stdout（给 `jq` 之类的机器消费者），失败信封走 stderr
/// ——诊断信息不该混进 `> result.json` 里。
pub fn print_output(output: String) -> i32 {
    if is_error_envelope(&output) {
        eprintln!("{output}");
        1
    } else {
        println!("{output}");
        0
    }
}

/// 信封是不是失败的：看 `"ok": false`。
///
/// 判据落在**信封内容**上，而不是"由哪条分支产生的"——`execute` 把成功与失败都压成字符串，
/// 这里统一识别。解析不出来（理论上不会发生）时按成功处理：不制造假警报。
fn is_error_envelope(output: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| value.get("ok").and_then(serde_json::Value::as_bool))
        == Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 负对照：把 `print_output` 改回"无条件 `println!` + 恒返回 0" ⇒ 这条会红。
    #[test]
    fn a_failed_envelope_exits_nonzero_so_a_scheduler_notices() {
        assert_eq!(print_output(r#"{"ok":true,"data":[]}"#.to_string()), 0);
        assert_eq!(
            print_output(r#"{"ok":false,"error":{"code":"RECIPE_NOT_FOUND"}}"#.to_string()),
            1,
            "失败必须非零退出，否则任务计划会把失败当成功"
        );
        // 不是 JSON（理论上不会发生）按成功处理：不制造假警报。
        assert_eq!(print_output("not json".to_string()), 0);
    }
}
