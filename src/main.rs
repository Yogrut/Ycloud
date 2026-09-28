#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !args.is_empty() {
        anyhow::ensure!(
            args.len() == 2 && args[0] == "doctor" && args[1] == "traffic",
            "用法：ycloud doctor traffic（CONFIG_PATH 指向现有配置文件）"
        );
        let config_path = std::env::var_os("CONFIG_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "./config.json".into());
        println!("{}", ycloud::traffic::diagnose_ledger(&config_path)?);
        return Ok(());
    }
    ycloud::run().await
}
