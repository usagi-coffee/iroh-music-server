use std::env;
use std::process::ExitCode;
use std::str::FromStr;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use iroh::{EndpointAddr, EndpointId, RelayUrl};
use iroh_tickets::endpoint::EndpointTicket;
use serde::Deserialize;
use subsonic::{
    Backend, RemoteBackend, RequestContext, SubsonicConfig, SubsonicResponse, handle_request,
};

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

#[derive(Clone)]
struct AppState {
    config: SubsonicConfig,
    backend: RemoteBackend,
}

#[derive(Debug, Deserialize)]
struct RawQuery {
    u: Option<String>,
    p: Option<String>,
    t: Option<String>,
    s: Option<String>,
    f: Option<String>,
    id: Option<String>,
    query: Option<String>,
}

async fn run() -> server::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return Ok(());
    }

    let config = parse_config(args.into_iter())?;
    let relay = parse_relay(config.relay.as_deref())?;
    let backend_addr = backend_addr_from_config(&config, relay.clone())?;
    eprintln!(
        "[subsonic] startup bind={} backend={} relay={}",
        config.bind,
        backend_addr.id,
        relays_for_log(&backend_addr)
    );
    eprintln!("[subsonic] connecting backend");
    let backend = RemoteBackend::connect_addr(backend_addr).await?;
    eprintln!("[subsonic] backend transport connected, probing summary");
    let summary = backend.summary().await?;
    eprintln!("[subsonic] backend summary probe ok: {summary:?}");
    let bind = config.bind.clone();
    let state = AppState { config, backend };
    let app = Router::new()
        .route("/rest/{*rest}", get(rest_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("subsonic facade listening on http://{bind}");
    axum::serve(listener, app)
        .await
        .map_err(server::Error::from)?;

    Ok(())
}

async fn rest_handler(
    State(state): State<AppState>,
    Path(rest): Path<String>,
    Query(query): Query<RawQuery>,
) -> Response {
    let normalized = normalize_rest_path(&rest);
    let format = match query.f.as_deref() {
        Some("json") => ResponseFormat::Json,
        _ => ResponseFormat::Xml,
    };
    let auth_mode = if query.t.is_some() && query.s.is_some() {
        "token"
    } else if query.p.is_some() {
        "password"
    } else {
        "none"
    };
    let request = RequestContext {
        path: normalized,
        query: [
            query.u.map(|v| ("u".to_string(), v)),
            query.p.map(|v| ("p".to_string(), v)),
            query.t.map(|v| ("t".to_string(), v)),
            query.s.map(|v| ("s".to_string(), v)),
            query.f.map(|v| ("f".to_string(), v)),
            query.id.map(|v| ("id".to_string(), v)),
            query.query.map(|v| ("query".to_string(), v)),
        ]
        .into_iter()
        .flatten()
        .collect(),
    };
    eprintln!(
        "[subsonic] request path={} format={} auth={} id={:?} query={:?}",
        request.path,
        format.as_str(),
        auth_mode,
        request
            .query
            .iter()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.as_str()),
        request
            .query
            .iter()
            .find(|(k, _)| k == "query")
            .map(|(_, v)| v.as_str())
    );

    match handle_request(&state.config, &state.backend, request).await {
        Ok(response) => {
            eprintln!("[subsonic] request ok path={}", rest);
            into_http_response(response)
        }
        Err(error) => {
            eprintln!("[subsonic] request failed path={} error={error}", rest);
            into_http_response(error_response(format, &format!("backend error: {error}")))
        }
    }
}

fn normalize_rest_path(rest: &str) -> String {
    let rest = rest.strip_suffix(".view").unwrap_or(rest);
    format!("/rest/{rest}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseFormat {
    Xml,
    Json,
}

impl ResponseFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Xml => "xml",
            Self::Json => "json",
        }
    }
}

fn error_response(format: ResponseFormat, message: &str) -> SubsonicResponse {
    match format {
        ResponseFormat::Xml => SubsonicResponse::Xml(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response status=\"failed\" version=\"1.16.1\"><error code=\"0\" message=\"{}\" /></subsonic-response>",
            xml_escape(message)
        )),
        ResponseFormat::Json => SubsonicResponse::Json(
            serde_json::json!({
                "subsonic-response": {
                    "status": "failed",
                    "version": "1.16.1",
                    "error": { "code": 0, "message": message }
                }
            })
            .to_string(),
        ),
    }
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn into_http_response(response: SubsonicResponse) -> Response {
    match response {
        SubsonicResponse::Xml(body) => (
            [(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/xml"),
            )],
            body,
        )
            .into_response(),
        SubsonicResponse::Json(body) => (
            [(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            body,
        )
            .into_response(),
        SubsonicResponse::Binary {
            content_type,
            bytes,
        } => {
            let mut response = bytes.into_response();
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_str(&content_type)
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );
            response
        }
    }
}

fn parse_config(args: impl Iterator<Item = String>) -> server::Result<SubsonicConfig> {
    let mut config = SubsonicConfig::default();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => config.bind = args.next().ok_or_else(missing_value)?,
            "--endpoint" => config.endpoint = args.next().ok_or_else(missing_value)?,
            "--ticket" => config.ticket = Some(args.next().ok_or_else(missing_value)?),
            "--relay" => config.relay = Some(args.next().ok_or_else(missing_value)?),
            "--username" => config.username = args.next().ok_or_else(missing_value)?,
            "--password" => config.password = args.next().ok_or_else(missing_value)?,
            other => {
                return Err(server::Error::InvalidRequest(format!(
                    "unknown argument: {other}"
                )));
            }
        }
    }
    if config.endpoint.trim().is_empty()
        && config
            .ticket
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return Err(server::Error::InvalidRequest(
            "expected --endpoint or --ticket".to_string(),
        ));
    }
    Ok(config)
}

fn parse_relay(relay: Option<&str>) -> server::Result<Option<RelayUrl>> {
    relay
        .map(|relay| {
            RelayUrl::from_str(relay)
                .map_err(|error| server::Error::InvalidRequest(format!("invalid --relay: {error}")))
        })
        .transpose()
}

fn backend_addr_from_config(
    config: &SubsonicConfig,
    relay: Option<RelayUrl>,
) -> server::Result<EndpointAddr> {
    if let Some(ticket) = config
        .ticket
        .as_deref()
        .filter(|ticket| !ticket.trim().is_empty())
    {
        let ticket = EndpointTicket::from_str(ticket)
            .map_err(|error| server::Error::InvalidRequest(format!("invalid --ticket: {error}")))?;
        let mut addr: EndpointAddr = ticket.into();
        if let Some(relay) = relay {
            addr = addr.with_relay_url(relay);
        }
        return Ok(addr);
    }

    let endpoint = EndpointId::from_str(&config.endpoint)
        .map_err(|error| server::Error::InvalidRequest(format!("invalid --endpoint: {error}")))?;
    let addr = match relay {
        Some(relay) => EndpointAddr::new(endpoint).with_relay_url(relay),
        None => EndpointAddr::new(endpoint),
    };
    Ok(addr)
}

fn relays_for_log(addr: &EndpointAddr) -> String {
    let relays = addr
        .addrs
        .iter()
        .filter_map(|addr| match addr {
            iroh::TransportAddr::Relay(relay) => Some(relay.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if relays.is_empty() {
        "<none>".to_string()
    } else {
        relays.join(",")
    }
}

fn missing_value() -> server::Error {
    server::Error::InvalidRequest("missing value for flag".to_string())
}

fn print_usage() {
    println!("usage:");
    println!(
        "  subsonic --bind 127.0.0.1:4040 (--ticket <endpoint-ticket> | --endpoint <server-endpoint-id> [--relay <relay-url>])"
    );
}
