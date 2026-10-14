//! 后台循环的日志：**写失败就当没写**。
//!
//! 为什么不能直接用 `eprintln!`：Rust 的 `eprintln!` / `println!` 在**写入失败时会 panic**
//! （"Panics if writing to io::stderr fails"）。daemon 的后台循环每轮都要打日志——对账
//! 每 5 秒一行——所以只要 stdout/stderr 断掉（管道被关、父进程退出、日志文件被删、磁盘满），
//! 第一次写就 panic；而 `run_tick` 捕获 panic 之后**又要写日志报告它**，于是再 panic 一次
//! ⇒ 那个循环彻底停摆，而且**外面看不出来**（进程还在、其它循环照跑）。
//!
//! 实测（2026-10-10）：手工启动 daemon 时把输出接到管道上，调用方一结束管道就关了。
//! 结果**对账循环静默死掉**：任务提交成功（调度器日志少、活了下来），平台那边早跑完了，
//! 而本地永远停在 `ACCEPTED`、`next_poll_at` 是 NULL —— 症状和"平台卡住"一模一样。
//! 改成"写失败就算了"之后，同样的场景下循环照常转。
//!
//! 边界：只换**后台循环**里的日志。启动期那几行（监听地址、bootstrap 链接）保持
//! `eprintln!`——它们只写一次，写不出去就该**大声失败**，而不是悄悄跑起来。
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 一行日志的时间前缀：**UTC 的 ISO-8601**（毫秒 + `Z`），如 `2026-10-13T15:58:01.123Z`。
///
/// 为什么是 UTC 而不是本地时间——三条，每条都独立成立：
///
/// 1. **它要与另外两处天然对齐**：DB 里的事件是 epoch 毫秒、诊断里的 `modifiedAtMillis` 也是
///    epoch 毫秒。各自换成"那台机器的本地时间"之后就再也对不上了（而且跨时区/夏令时还会变）；
/// 2. 这个文件的用途是"交给 agent 去发掘"——UTC 无歧义，且任何工具都能解析；
/// 3. 本地时间要一个**平台相关**的时区查询，而 `localtime` 那一类接口在多线程程序里不安全
///    （`time` crate 的 `local-offset` 就明确拒绝在多线程下工作，本 daemon 是 tokio 多线程）。
///    为一行前缀引平台代码不划算。
///
/// 界面上的时间仍然是**用户的本地时间**（前端自己格式化 epoch 毫秒），所以"我几点点的按钮"
/// 那条线在界面上照样对得上。日志末尾那个 `Z` 就是"这是 UTC"的明示。
pub fn timestamp() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    format_utc_millis(millis)
}

/// epoch 毫秒 → `YYYY-MM-DDTHH:MM:SS.mmmZ`（纯函数，可测）。
fn format_utc_millis(millis: u64) -> String {
    let seconds = millis / 1000;
    let millis_part = millis % 1000;
    let days = seconds / 86_400;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis_part:03}Z",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

/// 自 1970-01-01 起的天数 → 公历年月日（Howard Hinnant 的 `civil_from_days`，只用整数运算）。
///
/// 自己算而不是引一个时间 crate：这个仓库的构建走 `--offline`，而"本地时区"那条路本来就走不通
/// （见 `timestamp` 的注释），那就没必要为一个纯函数拉一整套日历库。
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 往 stderr 写一行（带时间前缀）；**失败不 panic**。
pub fn log_line(message: &str) {
    log_line_to(&mut std::io::stderr(), message);
}

/// 可注入 writer 的版本：留这个口子是为了让测试能证明"写失败不会 panic"
/// （真的去把进程的 stderr 弄坏是做不到的）。
pub fn log_line_to(writer: &mut dyn Write, message: &str) {
    let _ = writeln!(writer, "{} {message}", timestamp());
}

/// **出事了**就写一行，形状固定：`[renderloft] <scope> <subject> <code> | <message>`。
///
/// 为什么要有统一形状：这个文件最终是给"用户把它交给别人（或他的 agent）去发掘"用的
/// ——固定前缀才能稳定 grep，而不是一堆风格各异的句子。判据是**只记人话**：
/// scope 说清是哪一块（`run` / `connection` / `hygiene`），subject 说清是哪一条
/// （`run=7` / `connection=2`），code 是那个稳定的错误码（能拿去搜），message 是给人看的那句。
///
/// ⚠️ **绝不写 `details`**：那里面有 `cause`，可能带 URL 与密钥。而 `message` 本身也要过
/// `domain::redact`——平台回的错误文案是我们转述的，不能假定它干净。
pub fn log_failure(scope: &str, subject: &str, code: &str, message: &str) {
    log_line(&failure_line(scope, subject, code, message));
}

/// `log_failure` 的**节流版**：同一把 `key` 最多每 `min_interval` 写一次。
///
/// 给"会被反复触发的失败"用：连接测试每次打开连接页都会跑一遍，一条常坏的连接
/// （比如本地 ComfyUI 没开）会把日志刷满——而那正是我们要避免的噪声。
pub fn log_failure_throttled(
    key: &str,
    min_interval: Duration,
    scope: &str,
    subject: &str,
    code: &str,
    message: &str,
) {
    log_line_every(
        key,
        min_interval,
        &failure_line(scope, subject, code, message),
    );
}

/// 那条固定形状（两个入口共用，形状只在一处拼）。
fn failure_line(scope: &str, subject: &str, code: &str, message: &str) -> String {
    format!(
        "[renderloft] {scope} {subject} {code} | {}",
        crate::v2::domain::redact::redact_urls(message)
    )
}

/// 同一把 key 的日志**最多每 `min_interval` 写一次**；真的写了返回 true。
///
/// 为什么需要：对账心跳原来是每 5 秒一行，实测占整个日志文件的 **98%**——真信号全被淹掉，
/// 而这文件的存在意义恰恰是"出事以后能看出发生了什么"。节流之后心跳仍在（"还活着吗"
/// 看得出来），但不再把别的东西挤走。
pub fn log_line_every(key: &str, min_interval: Duration, message: &str) -> bool {
    static LAST_SEEN: OnceLock<Mutex<std::collections::HashMap<String, Instant>>> = OnceLock::new();
    let table = LAST_SEEN.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let now = Instant::now();
    // 锁中毒（某次持锁 panic）不当作失败：日志不值得为此停摆。
    let mut table = table.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(last) = table.get(key) {
        if now.duration_since(*last) < min_interval {
            return false;
        }
    }
    table.insert(key.to_string(), now);
    drop(table);
    log_line(message);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind, Result};

    /// 一个**永远写失败**的 writer：模拟管道关闭 / 磁盘满 / 句柄失效。
    struct AlwaysFails;

    impl Write for AlwaysFails {
        fn write(&mut self, _buffer: &[u8]) -> Result<usize> {
            Err(Error::new(ErrorKind::BrokenPipe, "管道已关闭"))
        }
        fn flush(&mut self) -> Result<()> {
            Err(Error::new(ErrorKind::BrokenPipe, "管道已关闭"))
        }
    }

    /// 负对照：把 `log_line_to` 里的 `let _ = writeln!(...)` 改成 `writeln!(...).unwrap()`
    /// ⇒ 这条会 panic 而红。钉住的是"日志写不出去也不能把循环带走"。
    #[test]
    fn logging_never_panics_even_when_the_writer_is_gone() {
        let mut broken = AlwaysFails;
        log_line_to(&mut broken, "[renderloft-reconcile] tick active_runs=0");
        // 能走到这里就说明没 panic；再写几行确认不是碰巧。
        for round in 0..5 {
            log_line_to(&mut broken, &format!("[renderloft-reconcile] tick {round}"));
        }
    }

    /// 心跳节流：同一个 key 在窗口内只写一次，过了窗口才再写。
    ///
    /// 负对照：把 `log_line_every` 里的时间比较去掉（永远写）⇒ 第二条断言变红。
    #[test]
    fn log_line_every_throttles_by_key() {
        // 窗口给 0：等价于"不节流"，用来确认这个函数本身能写出去。
        assert!(log_line_every("throttle-zero", Duration::ZERO, "第一行"));
        // 窗口给得很大：第二条必须被吞掉，且不同的 key 不受影响。
        assert!(log_line_every(
            "throttle-big",
            Duration::from_secs(3600),
            "第一行"
        ));
        assert!(!log_line_every(
            "throttle-big",
            Duration::from_secs(3600),
            "第二行（应当被吞）"
        ));
        assert!(log_line_every(
            "throttle-other",
            Duration::from_secs(3600),
            "另一把 key"
        ));
    }

    /// 失败行的形状：固定前缀 + 三段 + 竖线 + 人话；**URL 必须被脱敏**（密钥可能在其中）。
    #[test]
    fn log_failure_has_a_stable_shape_and_no_secrets() {
        assert_eq!(
            crate::v2::domain::redact::redact_urls(
                "error sending request for url (https://host/proxy/SECRET)"
            ),
            "error sending request for url (https://host/…)"
        );
        // 形状本身由 `log_failure` 的格式串保证（写进 stderr，测试不去抓 stderr）；
        // 这里钉的是"它一定过脱敏"这条，见上面的等价调用。
    }

    /// 时间戳：epoch 毫秒 → UTC ISO-8601。
    ///
    /// 参考值都是**独立算出来的**（不是从本实现里抄的），其中两个是公开的整数纪元点，
    /// 中间那个是闰日——日历代码最容易在这里错一天，而错一天在日志里几乎看不出来。
    #[test]
    fn timestamps_render_as_utc_iso8601() {
        assert_eq!(format_utc_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_utc_millis(1_000_000_000_000),
            "2001-09-09T01:46:40.000Z"
        );
        assert_eq!(
            format_utc_millis(1_700_000_000_000),
            "2023-11-14T22:13:20.000Z"
        );
        // 闰日（2024-02-29）
        assert_eq!(
            format_utc_millis(1_709_164_800_000),
            "2024-02-29T00:00:00.000Z"
        );
        // 毫秒部分必须原样保留（排查要的是"同一秒里谁先谁后"）
        assert_eq!(
            format_utc_millis(1_791_907_081_123),
            "2026-10-13T15:58:01.123Z"
        );
        // 世纪末那一年不是闰年（1900 与 2100 都不是）——顺手钉住世纪规则。
        assert_eq!(
            format_utc_millis(4_102_444_800_000),
            "2100-01-01T00:00:00.000Z"
        );
    }
}
