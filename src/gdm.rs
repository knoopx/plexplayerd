use crate::identity::HTTP_PORT;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::io::{FromRawFd, IntoRawFd};

use anyhow::{Context, Result};
use socket2::{Domain, SockAddr, Socket, Type};
use tokio::net::UdpSocket;
use tracing::{info, warn};

pub const GDM_PORT: u16 = 32412;
pub const GDM_MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);

pub async fn gdm_responder(reply: String) -> Result<()> {
    let socket =
        Socket::new(Domain::IPV4, Type::DGRAM, None).context("creating the GDM UDP socket")?;
    socket
        .set_reuse_address(true)
        .context("setting SO_REUSEADDR")?;
    socket
        .set_nonblocking(true)
        .context("setting the socket non-blocking")?;
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), GDM_PORT);
    socket
        .bind(&SockAddr::from(bind))
        .with_context(|| format!("binding UDP {GDM_PORT} (Plex GDM)"))?;

    match socket.join_multicast_v4(&GDM_MULTICAST, &Ipv4Addr::UNSPECIFIED) {
        Ok(()) => info!("GDM: joined multicast group {GDM_MULTICAST} on port {GDM_PORT}"),
        Err(err) => warn!("GDM: failed to join multicast {GDM_MULTICAST}: {err}"),
    }
    let socket =
        UdpSocket::from_std(unsafe { std::net::UdpSocket::from_raw_fd(socket.into_raw_fd()) })
            .context("converting to tokio socket")?;
    info!("GDM: listening on UDP {GDM_PORT} (http port {HTTP_PORT})");

    let mut buf = [0u8; 1024];
    loop {
        let (len, peer) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(err) => {
                warn!("GDM: recv failed: {err}");
                continue;
            }
        };
        let preview = String::from_utf8_lossy(&buf[..len]);
        info!("GDM: probe from {peer} ({len} bytes): {preview:?}");
        info!("GDM: replying to {peer} with {} bytes", reply.len());
        if let Err(err) = socket.send_to(reply.as_bytes(), peer).await {
            warn!("GDM: reply to {peer} failed: {err}");
        }
    }
}
