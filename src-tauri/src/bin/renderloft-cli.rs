use renderloft::v2::clients::cli::{execute, parse_command, print_output};

#[tokio::main]
async fn main() {
    // 子命令树由动词表渲染（见 `v2::clients::verbs`），这里只负责跑。
    let matches = parse_command();
    let output = execute(matches).await;
    // 退出码是给调度器看的契约：失败必须非零（见 `print_output` 的注释）。
    std::process::exit(print_output(output));
}
