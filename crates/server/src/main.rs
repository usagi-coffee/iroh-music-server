use std::env;
use std::process::ExitCode;
use std::str::FromStr;

use server::{BackendRequest, IrohConfig, MusicServer, ServerConfig, spawn_iroh_server};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> server::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return Ok(());
    }

    let (music_dir, lastfm_api_key, iroh) = parse_config(args.into_iter())?;
    if let Some(secret) = &iroh.secret {
        let _ = iroh::SecretKey::from_str(secret)
            .map_err(|error| server::Error::InvalidRequest(format!("invalid --secret: {error}")))?;
    }
    if let Some(relay) = &iroh.relay {
        let _ = iroh::RelayUrl::from_str(relay)
            .map_err(|error| server::Error::InvalidRequest(format!("invalid --relay: {error}")))?;
    }

    let config = ServerConfig::new(
        music_dir,
        lastfm_api_key.or_else(|| env::var("LASTFM_API_KEY").ok()),
    );
    let server = MusicServer::load(config)?;
    let summary = server.handle(BackendRequest::GetLibrarySummary)?;
    let handle = spawn_iroh_server(server, &iroh).await?;
    let endpoint = handle.endpoint.id();
    println!("server backend ready: {summary:?}");
    println!("endpoint={endpoint}");
    if let Some(relay) = iroh.relay.as_deref() {
        println!("relay={relay}");
    }
    tokio::signal::ctrl_c().await?;
    handle.endpoint.close().await;
    handle.task.abort();

    Ok(())
}

fn parse_config(
    args: impl Iterator<Item = String>,
) -> server::Result<(std::path::PathBuf, Option<String>, IrohConfig)> {
    let mut music_dir = None;
    let mut lastfm_api_key = None;
    let mut iroh = IrohConfig::default();
    let mut args = args.peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--music-dir" => music_dir = args.next().map(Into::into),
            "--lastfm-api-key" => lastfm_api_key = Some(args.next().ok_or_else(missing_value)?),
            "--secret" => iroh.secret = Some(args.next().ok_or_else(missing_value)?),
            "--relay" => iroh.relay = Some(args.next().ok_or_else(missing_value)?),
            other => {
                return Err(server::Error::InvalidRequest(format!(
                    "unknown argument: {other}"
                )));
            }
        }
    }

    let music_dir = music_dir.ok_or_else(|| {
        server::Error::InvalidRequest("expected --music-dir /path/to/music".to_string())
    })?;
    Ok((music_dir, lastfm_api_key, iroh))
}

fn missing_value() -> server::Error {
    server::Error::InvalidRequest("missing value for flag".to_string())
}

fn print_usage() {
    println!("usage:");
    println!(
        "  server --music-dir /path/to/music [--lastfm-api-key <api-key>] [--secret <secret-key>] [--relay <relay-url>]"
    );
}
