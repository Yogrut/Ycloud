#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ycloud::run().await
}
