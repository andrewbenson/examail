//! `mock-servers`: serves the mock Google APIs and Microsoft Graph.

use anyhow::Context;
use clap::Parser;

/// Mock Google APIs (Gmail, Calendar, People, OAuth) with a deterministic fixture mailbox, also
/// served as Microsoft Graph.
#[derive(Debug, Parser)]
struct Args {
    /// Port to listen on.
    #[arg(long, default_value_t = 8787)]
    port: u16,
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Fixture seed.
    #[arg(long, default_value_t = mock_servers::Config::default().seed)]
    seed: u64,
    /// The mock's "now", from which its clock runs: RFC 3339, optionally with a zone that
    /// also becomes the account's (`2026-11-17T09:00:00-05:00[America/New_York]`).
    #[arg(long, value_parser = parse_now)]
    now: Option<jiff::Zoned>,
    /// An RFC 822 message (`.eml`) to deliver to the inbox, at startup and on every reset.
    /// Repeatable.
    #[arg(long)]
    eml: Vec<std::path::PathBuf>,
    /// Milliseconds every API request waits before it's answered, like a real network.
    #[arg(long, default_value_t = 0)]
    latency: u64,
    /// Enforce a per-user Gmail quota of this many units a minute, as Gmail does (6000 is
    /// Gmail's). Without it, no quota is enforced.
    #[arg(long)]
    quota: Option<u32>,
    /// Give the account a profile photo of its own (People's `people/me`, userinfo's
    /// `picture`), served at `/__mock/photo/…`. Without it, it has Google's generated avatar.
    #[arg(long)]
    photo: bool,
    /// Give the saved contacts and the directory's colleagues photos of their own, served at
    /// `/__mock/photo/…`. Without it, they have Google's generated avatar.
    #[arg(long)]
    contact_photos: bool,
    /// Serve at most this many Microsoft Graph requests at once, as Outlook does (4 is its
    /// limit); one more gets a 429. Without it, there's no limit.
    #[arg(long)]
    graph_concurrency: Option<u32>,
}

/// Instants without a zone keep the default zone (America/New_York).
fn parse_now(text: &str) -> Result<jiff::Zoned, jiff::Error> {
    mock_servers::parse_time(text, mock_servers::Config::default().now.time_zone())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let mut config = mock_servers::Config {
        seed: args.seed,
        extra_messages: args
            .eml
            .iter()
            .map(|path| std::fs::read(path).with_context(|| format!("reading {}", path.display())))
            .collect::<anyhow::Result<_>>()?,
        latency: std::time::Duration::from_millis(args.latency),
        quota: args.quota,
        photo: args.photo,
        contact_photos: args.contact_photos,
        graph_concurrency: args.graph_concurrency,
        ..mock_servers::Config::default()
    };
    if let Some(now) = args.now {
        config.now = now;
    }
    let listener = tokio::net::TcpListener::bind((args.host.as_str(), args.port))
        .await
        .with_context(|| format!("binding {}:{}", args.host, args.port))?;
    log::info!(
        "mock-servers on http://{} (seed {}, now {}, latency {:?}, quota {:?}); dev token: {}",
        listener.local_addr()?,
        config.seed,
        config.now,
        config.latency,
        config.quota,
        mock_servers::DEV_ACCESS_TOKEN
    );
    mock_servers::serve(listener, &config).await?;
    Ok(())
}
