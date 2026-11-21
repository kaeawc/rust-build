use crate::tlsargs::ClientTls;
use tokio::io::AsyncBufReadExt as _;

#[derive(clap::Args)]
pub struct Args {
    /// `ws://` or `wss://` URL.
    url: String,
    /// Messages to send; with none, sends each line of stdin.
    messages: Vec<String>,
    #[command(flatten)]
    tls: ClientTls,
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    let mut socket = rb_client::ws::connect(&a.url, a.tls.options()?).await?;
    if a.messages.is_empty() {
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        while let Some(line) = lines.next_line().await? {
            let (reply, rtt) = rb_client::ws::round_trip(&mut socket, &line).await?;
            println!("{reply}\t({rtt:?})");
        }
    } else {
        for m in &a.messages {
            let (reply, rtt) = rb_client::ws::round_trip(&mut socket, m).await?;
            println!("{reply}\t({rtt:?})");
        }
    }
    let _ = socket.close(None).await;
    Ok(())
}
