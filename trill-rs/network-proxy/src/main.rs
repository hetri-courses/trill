use anyhow::Result;
use clap::Parser;
use trill_network_proxy::Args;
use trill_network_proxy::NetworkProxy;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let _ = args;
    let proxy = NetworkProxy::builder().build().await?;
    proxy.run().await?.wait().await
}
