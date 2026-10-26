use std::sync::{Arc, Mutex};

use anyhow::Result;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub mod gdm;
pub mod identity;
pub mod models;
pub mod playback;
mod player;
pub mod pms;
pub mod server;

use gdm::GDM_PORT;
use identity::{HTTP_PORT, Player};

struct Options {
    name: String,
    product: String,
}

fn hostname() -> String {
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim().to_string();
        if !h.is_empty() {
            return h;
        }
    }
    if let Ok(out) = std::process::Command::new("hostname").output() {
        if out.status.success() {
            let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
    }
    "plexplayerd".to_string()
}

fn print_usage() {
    println!(
        "plexplayerd — a LAN-discoverable Plex player\n\n\
         USAGE:\n    plexplayerd [OPTIONS]\n\n\
         OPTIONS:\n    --name <NAME>         Player name (default: hostname)\n    \
         --product <PRODUCT>     Product field (default: hostname)\n    -h, --help  \
         Print this help"
    );
}

fn parse_args() -> Options {
    let args: Vec<String> = std::env::args().collect();
    let default = hostname();
    let mut name = default.clone();
    let mut product = default.clone();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "--name" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --name requires a value");
                    std::process::exit(2);
                }
                name = args[i].clone();
            }
            "--product" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --product requires a value");
                    std::process::exit(2);
                }
                product = args[i].clone();
            }
            other => {
                eprintln!("error: unknown argument '{}'", other);
                print_usage();
                std::process::exit(2);
            }
        }
        i += 1;
    }
    Options { name, product }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let opts = parse_args();
    let player = Player::new(opts.name, opts.product);
    info!(
        "Plex player '{}' (device {}) — GDM on UDP {GDM_PORT}, /resources on {HTTP_PORT}",
        player.name, player.device_id
    );

    let playback = Arc::new(Mutex::new(playback::Playback::new()));
    let gdm = tokio::spawn(gdm::gdm_responder(player.gdm_reply()));
    server::resources_server(player, playback);
    tokio::signal::ctrl_c().await?;
    info!("ctrl+c, shutting down");
    gdm.abort();
    Ok(())
}
