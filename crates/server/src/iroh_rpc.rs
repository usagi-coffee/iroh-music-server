use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, endpoint::presets};
use protocol::{BackendRequest, BackendResponse, IROH_ALPN, StreamDescriptor, TrackId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

use crate::error::{Error, Result};
use crate::server::MusicServer;

#[derive(Debug, Clone, Default)]
pub struct IrohConfig {
    pub secret: Option<String>,
    pub relay: Option<String>,
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
        let endpoint = endpoint_builder(&IrohConfig::default()).bind().await?;
        let addr = match relay {
            Some(relay_url) => EndpointAddr::new(endpoint_id).with_relay_url(relay_url),
            None => EndpointAddr::new(endpoint_id),
        };
        eprintln!(
            "[server-rpc-client] local endpoint ready local_endpoint={}",
            endpoint.id()
        );
        Ok(Self { endpoint, addr })
    }

    pub async fn request(&self, request: BackendRequest) -> Result<BackendResponse> {
        eprintln!(
            "[server-rpc-client] request start kind={}",
            request_name(&request)
        );
        let conn = self.endpoint.connect(self.addr.clone(), IROH_ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        write_json(&mut send, &request).await?;
        send.finish()?;
        let response = read_response(&mut recv).await?;
        conn.close(0u32.into(), b"done");
        eprintln!(
            "[server-rpc-client] request ok kind={}",
            request_name(&request)
        );
        Ok(response)
    }

    pub async fn stream(&self, track_id: TrackId) -> Result<(StreamDescriptor, Vec<u8>)> {
        eprintln!("[server-rpc-client] stream start track_id={}", track_id.0);
        let conn = self.endpoint.connect(self.addr.clone(), IROH_ALPN).await?;
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
        let bytes = recv.read_to_end(usize::MAX).await?;
        conn.close(0u32.into(), b"done");
        eprintln!("[server-rpc-client] stream ok bytes={}", bytes.len());
        Ok((descriptor, bytes))
    }
}

pub async fn spawn_iroh_server(server: MusicServer, config: &IrohConfig) -> Result<ServerHandle> {
    let endpoint = endpoint_builder(config)
        .alpns(vec![IROH_ALPN.to_vec()])
        .bind()
        .await?;
    eprintln!(
        "[server-rpc] listening endpoint={} relay={}",
        endpoint.id(),
        config.relay.as_deref().unwrap_or("<none>")
    );
    let server = Arc::new(server);
    let endpoint_for_task = endpoint.clone();
    let task = tokio::spawn(async move {
        loop {
            let Some(incoming) = endpoint_for_task.accept().await else {
                break;
            };
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                eprintln!(
                    "[server-rpc] incoming connection remote_addr={:?}",
                    incoming.remote_addr()
                );
                match incoming.accept() {
                    Ok(accepting) => match accepting.await {
                        Ok(conn) => {
                            eprintln!(
                                "[server-rpc] accepted connection remote_endpoint={}",
                                conn.remote_id()
                            );
                            let result = async {
                            let (mut send, mut recv) = conn.accept_bi().await?;
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
                                    write_json(
                                        &mut send,
                                        &BackendResponse::Stream(stream.clone()),
                                    )
                                    .await?;
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
                            Ok::<(), Error>(())
                        }
                        .await;
                            if result.is_err() {
                                if let Err(error) = &result {
                                    eprintln!("iroh rpc connection failed: {error}");
                                }
                                conn.close(1u32.into(), b"error");
                            } else {
                                eprintln!(
                                    "[server-rpc] connection complete remote_endpoint={}",
                                    conn.remote_id()
                                );
                                conn.closed().await;
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
        BackendRequest::GetArtist { .. } => "GetArtist",
        BackendRequest::GetAlbum { .. } => "GetAlbum",
        BackendRequest::GetTrack { .. } => "GetTrack",
        BackendRequest::GetCoverArt { .. } => "GetCoverArt",
        BackendRequest::Search { .. } => "Search",
        BackendRequest::OpenStream { .. } => "OpenStream",
    }
}
