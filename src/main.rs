mod config;
mod logging;
mod network;

use anyhow::{Context, Result};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Version, header},
};
use config::{Config, normalize_host};
use futures_util::StreamExt;
use logging::Access;
use reqwest::{Client, Url};
use std::{
    collections::HashSet,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct App {
    config: Config,
    client: Client,
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    let nominated: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .map(|s| s.trim().to_owned())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "proxy-connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

fn cross_origin_headers(headers: &HeaderMap) -> HeaderMap {
    // Credentials for Emby must never be sent to a redirected storage/CDN origin.
    let mut safe = HeaderMap::new();
    for name in [
        "accept",
        "accept-language",
        "accept-encoding",
        "user-agent",
        "range",
        "if-range",
        "if-none-match",
        "if-modified-since",
        "if-match",
        "if-unmodified-since",
    ] {
        for value in headers.get_all(name) {
            safe.append(name, value.clone());
        }
    }
    safe
}

fn target_url(origin: &str, uri: &axum::http::Uri) -> Result<Url> {
    anyhow::ensure!(
        uri.path().starts_with('/') && uri.authority().is_none(),
        "request target must use origin form"
    );
    // Concatenation keeps a client path beginning // from replacing the origin.
    Ok(Url::parse(&format!(
        "{}{}",
        origin.trim_end_matches('/'),
        uri.path_and_query().map_or("/", |v| v.as_str())
    ))?)
}

fn connection_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| v.trim().eq_ignore_ascii_case("upgrade"))
}

fn failure(status: StatusCode, access: &mut Access) -> Response<Body> {
    access.status = status.as_u16();
    access.complete = true;
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-emby-proxy-request-id", &access.id)
        .body(Body::from(
            status.canonical_reason().unwrap_or("Proxy error"),
        ))
        .unwrap()
}

async fn proxy(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request<Body>,
) -> Response<Body> {
    let path = request.uri().path().to_ascii_lowercase();
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let mut access = Access {
        id: uuid::Uuid::new_v4().to_string(),
        method: request.method().to_string(),
        route: if websocket {
            "websocket"
        } else if path.contains("/videos/")
            || path.contains("/stream/")
            || path.ends_with("/download")
        {
            "playback"
        } else {
            "api"
        },
        started: Instant::now(),
        status: 502,
        redirects: 0,
        bytes: 0,
        complete: false,
    };
    if request.method() == Method::CONNECT || request.uri().scheme().is_some() {
        return failure(StatusCode::BAD_REQUEST, &mut access);
    }
    if websocket && (request.method() != Method::GET || !connection_upgrade(request.headers())) {
        return failure(StatusCode::BAD_REQUEST, &mut access);
    }
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(normalize_host);
    let origin = host
        .as_ref()
        .and_then(|h| app.config.routes.get(h))
        .unwrap_or(&app.config.default_upstream);
    let mut url = match target_url(origin, request.uri()) {
        Ok(url) => url,
        Err(_) => return failure(StatusCode::BAD_REQUEST, &mut access),
    };
    let initial_origin = url.origin();
    let upgrade = if websocket {
        Some(hyper::upgrade::on(&mut request))
    } else {
        None
    };
    let (parts, body) = request.into_parts();
    let mut method = parts.method;
    let mut headers = parts.headers;
    strip_hop_headers(&mut headers);
    for name in [
        "host",
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
        "cf-connecting-ip",
        "true-client-ip",
        "content-length",
    ] {
        headers.remove(name);
    }
    headers.insert(
        "x-forwarded-for",
        HeaderValue::from_str(&peer.ip().to_string()).unwrap(),
    );
    headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
    if let Some(host) = host.as_ref().and_then(|h| HeaderValue::from_str(h).ok()) {
        headers.insert("x-forwarded-host", host);
    }
    let mut data = match tokio::time::timeout(
        Duration::from_secs(app.config.read_timeout_seconds),
        to_bytes(body, app.config.max_request_body_bytes),
    )
    .await
    {
        Ok(Ok(data)) => data,
        Ok(Err(_)) => return failure(StatusCode::PAYLOAD_TOO_LARGE, &mut access),
        Err(_) => return failure(StatusCode::REQUEST_TIMEOUT, &mut access),
    };
    let mut visited = HashSet::new();
    loop {
        if !network::allowed_url(&url, app.config.allow_private_upstreams) {
            return failure(StatusCode::BAD_GATEWAY, &mut access);
        }
        if !visited.insert(url.as_str().to_owned()) {
            return failure(StatusCode::BAD_GATEWAY, &mut access);
        }
        let mut outbound = app
            .client
            .request(method.clone(), url.clone())
            .headers(headers.clone())
            .body(data.clone());
        if websocket {
            outbound = outbound
                .version(Version::HTTP_11)
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket");
        }
        let upstream = match outbound.send().await {
            Ok(response) => response,
            Err(error) => {
                // reqwest's display text contains signed URLs; never log it.
                tracing::warn!(request_id=%access.id, timeout=error.is_timeout(), connect=error.is_connect(), "upstream request failed");
                let status = if error.is_timeout() {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                };
                return failure(status, &mut access);
            }
        };
        let status = upstream.status();
        if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            if websocket || access.redirects >= usize::from(app.config.max_redirects) {
                return failure(StatusCode::BAD_GATEWAY, &mut access);
            }
            let next = upstream
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| url.join(v).ok());
            let Some(mut next) = next else {
                return failure(StatusCode::BAD_GATEWAY, &mut access);
            };
            if !matches!(next.scheme(), "http" | "https")
                || !next.username().is_empty()
                || next.password().is_some()
            {
                return failure(StatusCode::BAD_GATEWAY, &mut access);
            }
            next.set_fragment(None);
            if (status == StatusCode::SEE_OTHER && method != Method::HEAD)
                || (matches!(status.as_u16(), 301 | 302) && method == Method::POST)
            {
                method = Method::GET;
                data = Bytes::new();
                headers.remove(header::CONTENT_TYPE);
                headers.remove(header::CONTENT_ENCODING);
            }
            if next.origin() != url.origin() {
                if !data.is_empty() {
                    return failure(StatusCode::BAD_GATEWAY, &mut access);
                }
                headers = cross_origin_headers(&headers);
            }
            access.redirects += 1;
            tracing::info!(request_id=%access.id, hop=access.redirects, status=status.as_u16(), upstream_host=next.host_str().unwrap_or(""), "following upstream redirect");
            url = next;
            continue;
        }
        access.status = status.as_u16();
        let mut response_headers = upstream.headers().clone();
        strip_hop_headers(&mut response_headers);
        response_headers.remove(header::LOCATION);
        response_headers.remove("content-location");
        if url.origin() != initial_origin {
            response_headers.remove(header::SET_COOKIE);
        }
        response_headers.insert(
            "x-emby-proxy-request-id",
            HeaderValue::from_str(&access.id).unwrap(),
        );
        if status == StatusCode::SWITCHING_PROTOCOLS {
            if !connection_upgrade(upstream.headers())
                || !upstream
                    .headers()
                    .get(header::UPGRADE)
                    .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
            {
                return failure(StatusCode::BAD_GATEWAY, &mut access);
            }
            let Some(incoming) = upgrade else {
                return failure(StatusCode::BAD_GATEWAY, &mut access);
            };
            response_headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
            response_headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
            tokio::spawn(async move {
                let mut access = access;
                if let (Ok(incoming), Ok(mut outgoing)) = tokio::join!(incoming, upstream.upgrade())
                {
                    let mut incoming = hyper_util::rt::TokioIo::new(incoming);
                    if let Ok((received, sent)) =
                        tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await
                    {
                        access.bytes = received + sent;
                        access.complete = true;
                    }
                }
            });
            let mut response = Response::new(Body::empty());
            *response.status_mut() = status;
            *response.headers_mut() = response_headers;
            return response;
        }
        if method == Method::HEAD
            || status == StatusCode::NO_CONTENT
            || status == StatusCode::NOT_MODIFIED
        {
            access.complete = true;
            let mut response = Response::new(Body::empty());
            *response.status_mut() = status;
            *response.headers_mut() = response_headers;
            return response;
        }
        let expected_length = upstream.content_length();
        let stream = async_stream::stream! {
            let mut access = access;
            let mut stream = upstream.bytes_stream();
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(chunk) => {
                        access.bytes += chunk.len() as u64;
                        access.complete = expected_length == Some(access.bytes);
                        yield Ok::<_, std::io::Error>(chunk);
                    }
                    Err(_) => { yield Err(std::io::Error::other("upstream stream interrupted")); return; }
                }
            }
            access.complete = true;
        };
        let mut response = Response::new(Body::from_stream(stream));
        *response.status_mut() = status;
        *response.headers_mut() = response_headers;
        return response;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut check = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => path = Some(args.next().context("--config requires a file")?.into()),
            "--check-config" => check = true,
            "--version" => {
                println!("emby-proxy {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => anyhow::bail!("usage: emby-proxy [--config FILE] [--check-config] [--version]"),
        }
    }
    let config = Config::load(path.as_deref())?;
    if check {
        println!("configuration valid");
        return Ok(());
    }
    let _logs = logging::init(&config.log_directory, config.log_retention_days)?;
    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(config.connect_timeout_seconds))
        .read_timeout(Duration::from_secs(config.read_timeout_seconds))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(32)
        .tcp_keepalive(Duration::from_secs(60));
    if !config.allow_private_upstreams {
        builder = builder.dns_resolver(Arc::new(network::PublicResolver));
    }
    let client = builder.build()?;
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(listen=%listener.local_addr()?, version=env!("CARGO_PKG_VERSION"), "proxy started");
    let app = Router::new()
        .fallback(proxy)
        .with_state(Arc::new(App { config, client }));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
        tracing::info!("proxy shutting down");
    })
    .await?;
    Ok(())
}
