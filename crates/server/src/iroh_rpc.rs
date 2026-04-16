use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::{collections::BTreeSet, fmt::Write as _};

use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, TransportAddr,
    endpoint::{Connection, RecvStream, SendStream, presets},
};
use protocol::{BackendRequest, BackendResponse, IROH_ALPN, StreamDescriptor, TrackId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

use crate::error::{Error, Result};
use crate::server::MusicServer;

const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct IrohConfig {
    pub secret: Option<String>,
    pub relay: Option<String>,
    pub peers: BTreeSet<EndpointId>,
}

#[derive(Debug)]
pub struct ServerHandle {
    pub endpoint: Endpoint,
    pub task: JoinHandle<()>,
}

#[derive(Debug, Clone)]
pub struct RemoteClient {
    endpoint: Endpoint,
    addr: EndpointAddr,
    conn: Arc<Mutex<Option<Connection>>>,
    last_path: Arc<Mutex<Option<String>>>,
}

impl RemoteClient {
    pub async fn connect(endpoint_id: EndpointId, relay: Option<RelayUrl>) -> Result<Self> {
        eprintln!(
            "[server-rpc-client] local endpoint startup remote_endpoint={} relay={}",
            endpoint_id,
            relay
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "<none>".to_string())
        );
        let addr = match relay {
            Some(relay_url) => EndpointAddr::new(endpoint_id).with_relay_url(relay_url),
            None => EndpointAddr::new(endpoint_id),
        };
        Self::connect_addr(addr).await
    }

    pub async fn connect_addr(addr: EndpointAddr) -> Result<Self> {
        Self::connect_addr_with_config(addr, IrohConfig::default()).await
    }

    pub async fn connect_addr_with_config(addr: EndpointAddr, config: IrohConfig) -> Result<Self> {
        eprintln!(
            "[server-rpc-client] local endpoint startup remote_addr={:?}",
            addr
        );
        let endpoint = endpoint_builder(&config).bind().await?;
        eprintln!(
            "[server-rpc-client] local endpoint ready local_endpoint={}",
            endpoint.id()
        );
        let client = Self {
            endpoint,
            addr,
            conn: Arc::new(Mutex::new(None)),
            last_path: Arc::new(Mutex::new(None)),
        };
        let _ = client.connection().await?;
        Ok(client)
    }

    pub async fn request(&self, request: BackendRequest) -> Result<BackendResponse> {
        eprintln!(
            "[server-rpc-client] request start kind={}",
            request_name(&request)
        );
        let conn = self.connection().await?;
        match self
            .request_on_connection_with_timeout(&conn, &request)
            .await
        {
            Ok(BackendResponse::Error { message }) => Err(Error::InvalidRequest(message)),
            Ok(response) => {
                eprintln!(
                    "[server-rpc-client] request ok kind={}",
                    request_name(&request)
                );
                self.log_connection_path_if_changed(&conn, "request").await;
                Ok(response)
            }
            Err(error) => {
                eprintln!(
                    "[server-rpc-client] request failed kind={} error={error}; reconnecting once",
                    request_name(&request)
                );
                self.clear_connection().await;
                let conn = self.connection().await?;
                let response = match self
                    .request_on_connection_with_timeout(&conn, &request)
                    .await?
                {
                    BackendResponse::Error { message } => {
                        return Err(Error::InvalidRequest(message));
                    }
                    response => response,
                };
                eprintln!(
                    "[server-rpc-client] request ok kind={}",
                    request_name(&request)
                );
                self.log_connection_path_if_changed(&conn, "request").await;
                Ok(response)
            }
        }
    }

    async fn request_on_connection(
        &self,
        conn: &Connection,
        request: &BackendRequest,
    ) -> Result<BackendResponse> {
        let (mut send, mut recv) = conn.open_bi().await?;
        write_json(&mut send, &request).await?;
        send.finish()?;
        read_json(&mut recv).await
    }

    async fn request_on_connection_with_timeout(
        &self,
        conn: &Connection,
        request: &BackendRequest,
    ) -> Result<BackendResponse> {
        timeout(RPC_TIMEOUT, self.request_on_connection(conn, request))
            .await
            .map_err(|_| Error::Timeout(format!("rpc {}", request_name(request))))?
    }

    pub async fn stream(&self, track_id: TrackId) -> Result<(StreamDescriptor, Vec<u8>)> {
        eprintln!("[server-rpc-client] stream start track_id={}", track_id.0);
        let conn = self.connection().await?;
        match self.stream_on_connection(&conn, track_id.clone()).await {
            Ok(stream) => {
                self.log_connection_path_if_changed(&conn, "stream").await;
                Ok(stream)
            }
            Err(error) => {
                eprintln!(
                    "[server-rpc-client] stream failed track_id={} error={error}; reconnecting once",
                    track_id.0
                );
                self.clear_connection().await;
                let conn = self.connection().await?;
                self.stream_on_connection(&conn, track_id).await
            }
        }
    }

    async fn stream_on_connection(
        &self,
        conn: &Connection,
        track_id: TrackId,
    ) -> Result<(StreamDescriptor, Vec<u8>)> {
        let (descriptor, mut recv) = timeout(
            STREAM_OPEN_TIMEOUT,
            self.stream_open_on_connection(conn, track_id.clone()),
        )
        .await
        .map_err(|_| Error::Timeout(format!("stream open {}", track_id.0)))??;
        eprintln!(
            "[server-rpc-client] stream descriptor track_id={} file_size={} content_type={} transfer_timeout=disabled",
            descriptor.track_id.0, descriptor.file_size, descriptor.content_type
        );
        let bytes = recv.read_to_end(usize::MAX).await?;
        eprintln!("[server-rpc-client] stream ok bytes={}", bytes.len());
        Ok((descriptor, bytes))
    }

    async fn stream_open_on_connection(
        &self,
        conn: &Connection,
        track_id: TrackId,
    ) -> Result<(StreamDescriptor, RecvStream)> {
        let (mut send, mut recv) = conn.open_bi().await?;
        write_json(&mut send, &BackendRequest::OpenStream { track_id }).await?;
        send.finish()?;
        let descriptor = match read_response(&mut recv).await? {
            BackendResponse::Stream(stream) => stream,
            _ => {
                return Err(Error::InvalidRequest(
                    "backend returned unexpected response for stream".to_string(),
                ));
            }
        };
        Ok((descriptor, recv))
    }

    async fn connection(&self) -> Result<Connection> {
        let mut guard = self.conn.lock().await;
        if let Some(conn) = guard.as_ref() {
            return Ok(conn.clone());
        }

        eprintln!("[server-rpc-client] connecting backend transport");
        let conn = timeout(
            CONNECT_TIMEOUT,
            self.endpoint.connect(self.addr.clone(), IROH_ALPN),
        )
        .await
        .map_err(|_| Error::Timeout("connect backend transport".to_string()))??;
        eprintln!(
            "[server-rpc-client] backend transport connected remote_endpoint={}",
            conn.remote_id()
        );
        log_connection_path("server-rpc-client", "connected", &conn);
        *self.last_path.lock().await = Some(connection_path_label(&conn));
        *guard = Some(conn.clone());
        Ok(conn)
    }

    async fn clear_connection(&self) {
        let mut guard = self.conn.lock().await;
        if let Some(conn) = guard.take() {
            conn.close(1u32.into(), b"reconnect");
        }
        *self.last_path.lock().await = None;
    }

    async fn log_connection_path_if_changed(&self, conn: &Connection, event: &str) {
        let label = connection_path_label(conn);
        let mut guard = self.last_path.lock().await;
        if guard.as_deref() != Some(label.as_str()) {
            eprintln!("[server-rpc-client] path changed event={event} {label}");
            *guard = Some(label);
        }
    }
}

pub async fn spawn_iroh_server(server: MusicServer, config: &IrohConfig) -> Result<ServerHandle> {
    let endpoint = endpoint_builder(config)
        .alpns(vec![IROH_ALPN.to_vec()])
        .bind()
        .await?;
    eprintln!(
        "[server-rpc] listening endpoint={} relay={} peers={}",
        endpoint.id(),
        config.relay.as_deref().unwrap_or("<none>"),
        peer_policy_label(&config.peers)
    );
    let server = Arc::new(server);
    let allowed_peers = Arc::new(config.peers.clone());
    let endpoint_for_task = endpoint.clone();
    let task = tokio::spawn(async move {
        loop {
            let Some(incoming) = endpoint_for_task.accept().await else {
                break;
            };
            let server = Arc::clone(&server);
            let allowed_peers = Arc::clone(&allowed_peers);
            tokio::spawn(async move {
                eprintln!(
                    "[server-rpc] incoming connection remote_addr={:?}",
                    incoming.remote_addr()
                );
                match incoming.accept() {
                    Ok(accepting) => match accepting.await {
                        Ok(conn) => {
                            let remote_id = conn.remote_id();
                            if !allowed_peers.is_empty() && !allowed_peers.contains(&remote_id) {
                                eprintln!(
                                    "[server-rpc] rejected connection remote_endpoint={} allowed_peers={}",
                                    remote_id,
                                    peer_policy_label(&allowed_peers)
                                );
                                conn.close(1u32.into(), b"peer not allowed");
                                return;
                            }
                            eprintln!(
                                "[server-rpc] accepted connection remote_endpoint={}",
                                remote_id
                            );
                            log_connection_path("server-rpc", "accepted", &conn);
                            loop {
                                match conn.accept_bi().await {
                                    Ok((send, recv)) => {
                                        let server = Arc::clone(&server);
                                        tokio::spawn(async move {
                                            if let Err(error) =
                                                handle_rpc_stream(server, send, recv).await
                                            {
                                                eprintln!("iroh rpc stream failed: {error}");
                                            }
                                        });
                                    }
                                    Err(error) => {
                                        eprintln!(
                                            "[server-rpc] connection closed remote_endpoint={} error={error}",
                                            remote_id
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("[server-rpc] accepting connection failed: {error}")
                        }
                    },
                    Err(error) => eprintln!("[server-rpc] incoming.accept failed: {error}"),
                }
            });
        }
    });
    Ok(ServerHandle { endpoint, task })
}

async fn handle_rpc_stream(
    server: Arc<MusicServer>,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    let request: BackendRequest = read_json(&mut recv).await?;
    eprintln!("[server-rpc] request kind={}", request_name(&request));
    match request {
        BackendRequest::OpenStream { track_id } => {
            let response = match server.handle(BackendRequest::OpenStream { track_id }) {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("[server-rpc] stream request failed: {error}");
                    write_json(
                        &mut send,
                        &BackendResponse::Error {
                            message: error.to_string(),
                        },
                    )
                    .await?;
                    send.finish()?;
                    return Ok(());
                }
            };
            let BackendResponse::Stream(stream) = response else {
                eprintln!("[server-rpc] stream request returned non-stream response");
                write_json(
                    &mut send,
                    &BackendResponse::Error {
                        message: "unexpected stream response".to_string(),
                    },
                )
                .await?;
                send.finish()?;
                return Ok(());
            };
            write_json(&mut send, &BackendResponse::Stream(stream.clone())).await?;
            let full_path = PathBuf::from(&stream.path);
            let bytes = tokio::fs::read(&full_path).await?;
            eprintln!(
                "[server-rpc] stream sending path={} bytes={}",
                full_path.display(),
                bytes.len()
            );
            send.write_all(&bytes).await?;
            send.finish()?;
        }
        other => {
            let response = match server.handle(other) {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("[server-rpc] request failed: {error}");
                    BackendResponse::Error {
                        message: error.to_string(),
                    }
                }
            };
            write_json(&mut send, &response).await?;
            send.finish()?;
        }
    }
    Ok(())
}

fn log_connection_path(prefix: &str, event: &str, conn: &Connection) {
    eprintln!(
        "[{prefix}] path event={event} {}",
        connection_path_label(conn)
    );
}

fn connection_path_label(conn: &Connection) -> String {
    let Some(path) = conn.to_info().selected_path() else {
        return "type=unknown selected_path=<none>".to_string();
    };
    let kind = if path.is_relay() {
        "relay"
    } else if path.is_ip() {
        "direct"
    } else {
        "custom"
    };
    let rtt = path
        .rtt()
        .map(|rtt| format!(" rtt_ms={}", rtt.as_millis()))
        .unwrap_or_default();
    format!(
        "type={} selected_path={:?} remote_addr={}{}",
        kind,
        path.id(),
        transport_addr_label(path.remote_addr()),
        rtt
    )
}

fn transport_addr_label(addr: &TransportAddr) -> String {
    match addr {
        TransportAddr::Relay(relay) => format!("relay:{relay}"),
        TransportAddr::Ip(addr) => format!("ip:{addr}"),
        TransportAddr::Custom(addr) => format!("custom:{addr}"),
        other => format!("unknown:{other:?}"),
    }
}

fn peer_policy_label(peers: &BTreeSet<EndpointId>) -> String {
    if peers.is_empty() {
        return "open".to_string();
    }

    let mut label = String::from("allow:");
    for (index, peer) in peers.iter().enumerate() {
        if index > 0 {
            label.push(',');
        }
        let _ = write!(label, "{peer}");
    }
    label
}

fn endpoint_builder(config: &IrohConfig) -> iroh::endpoint::Builder {
    let mut builder = Endpoint::builder(presets::N0);
    if let Some(secret) = &config.secret {
        let secret = SecretKey::from_str(secret)
            .map_err(|error| Error::InvalidRequest(format!("invalid --secret: {error}")))
            .expect("validated secret");
        builder = builder.secret_key(secret);
    }
    if let Some(relay) = &config.relay {
        let relay: RelayUrl = relay
            .parse()
            .map_err(|error| Error::InvalidRequest(format!("invalid --relay: {error}")))
            .expect("validated relay");
        builder = builder.relay_mode(RelayMode::custom([relay]));
    }
    builder
}

async fn write_json<T: serde::Serialize>(
    send: &mut iroh::endpoint::SendStream,
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| Error::InvalidRequest("message too large".to_string()))?;
    send.write_u32(len).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn read_json<T: serde::de::DeserializeOwned>(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<T> {
    let len = recv.read_u32().await?;
    let mut bytes = vec![0_u8; len as usize];
    recv.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn read_response(recv: &mut iroh::endpoint::RecvStream) -> Result<BackendResponse> {
    match read_json(recv).await? {
        BackendResponse::Error { message } => Err(Error::InvalidRequest(message)),
        response => Ok(response),
    }
}

fn request_name(request: &BackendRequest) -> &'static str {
    match request {
        BackendRequest::GetLibrarySummary => "GetLibrarySummary",
        BackendRequest::ListArtists => "ListArtists",
        BackendRequest::GetStarred => "GetStarred",
        BackendRequest::SetStarred { .. } => "SetStarred",
        BackendRequest::GetArtist { .. } => "GetArtist",
        BackendRequest::GetAlbum { .. } => "GetAlbum",
        BackendRequest::GetAlbumTracks { .. } => "GetAlbumTracks",
        BackendRequest::GetTrack { .. } => "GetTrack",
        BackendRequest::GetCoverArt { .. } => "GetCoverArt",
        BackendRequest::ResolveId { .. } => "ResolveId",
        BackendRequest::Search { .. } => "Search",
        BackendRequest::OpenStream { .. } => "OpenStream",
    }
}
