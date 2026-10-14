#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    if let Err(error) = renderloft::v2::daemon::run().await {
        eprintln!("RenderLoft daemon 启动失败：{error}");
        std::process::exit(1);
    }
}
