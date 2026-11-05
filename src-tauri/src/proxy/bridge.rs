use anyhow::{Result, anyhow};
use http::StatusCode;
use http::uri::Authority;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, Uri};
use hyper_util::rt::TokioIo;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use crate::stats::ProxyStats;

pub struct ProxyBridge {
    pub task: tokio::task::JoinHandle<()>,
}

/// A dropped JoinHandle only detaches its task. If a connect is cancelled
/// while the bridge is being built, that would leave the HTTP listener holding
/// the port forever — so the task must be aborted whenever the bridge dies.
impl Drop for ProxyBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ProxyBridge {
    pub async fn start(socks_port: u16, http_port: u16, stats: Arc<ProxyStats>) -> Result<Self> {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", http_port))
            .await
            .map_err(|e| anyhow!("Failed to bind HTTP proxy on port {}: {}", http_port, e))?;

        let task = tokio::spawn(async move {
            tracing::info!(
                "Rust SOCKS5→HTTP proxy bridge listening on :{} (SOCKS5 :{})",
                http_port,
                socks_port
            );
            loop {
                let (stream, addr) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("Failed to accept connection: {}", e);
                        continue;
                    }
                };

                tracing::debug!("HTTP proxy accepted connection from {}", addr);
                let socks_port = socks_port;
                let stats = Arc::clone(&stats);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, socks_port, stats).await {
                        tracing::debug!("Connection handler finished: {}", e);
                    }
                });
            }
        });

        // Own the bridge *before* awaiting: if this future is dropped by a
        // cancelled connect, Drop aborts the spawned listener task.
        let bridge = ProxyBridge { task };

        // Wait for port to be ready
        crate::network::port::wait_for_port(http_port, std::time::Duration::from_secs(5)).await;

        Ok(bridge)
    }

    pub async fn abort(&mut self) {
        // Replace the JoinHandle with a completed dummy to take ownership
        let task = std::mem::replace(&mut self.task, tokio::spawn(async {}));
        task.abort();
        let _ = task.await;  // Wait for task to finish, ensuring TcpListener is dropped
    }
}

// ── Connection handler ───────────────────────────────────────

async fn handle_connection(incoming: TcpStream, socks_port: u16, stats: Arc<ProxyStats>) -> Result<()> {
    let hyper_service = service_fn(move |req: Request<Incoming>| {
        let stats = Arc::clone(&stats);
        async move {
            handle_proxy_request(req, socks_port, stats).await
        }
    });

    // Builder::serve_connection returns Connection, then .with_upgrades() enables CONNECT tunneling
    hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(incoming), hyper_service)
        .with_upgrades()
        .await
        .map_err(|e| anyhow!("HTTP/1.1 connection error: {}", e))?;

    Ok(())
}

// ── Request dispatcher ───────────────────────────────────────

async fn handle_proxy_request(
    req: Request<Incoming>,
    socks_port: u16,
    stats: Arc<ProxyStats>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let result = if req.method() == hyper::Method::CONNECT {
        handle_connect(req, socks_port, Arc::clone(&stats)).await
    } else {
        handle_http_request(req, socks_port, Arc::clone(&stats)).await
    };

    Ok(result.unwrap_or_else(|e| {
        tracing::warn!("Proxy request failed: {}", e);
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Full::new(Bytes::from(format!("Proxy error: {}", e))))
            .unwrap()
    }))
}

// ── SOCKS5 client protocol ───────────────────────────────────

/// Establish a SOCKS5 tunnel to the target address through our local SOCKS proxy.
/// Returns a TcpStream that is transparently proxied through the SSH tunnel.
async fn socks5_connect(
    socks_port: u16,
    host: &str,
    port: u16,
) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", socks_port))
        .await
        .map_err(|e| anyhow!("Cannot connect to SOCKS5 proxy on port {}: {}", socks_port, e))?;

    // ── Greeting: version 5, one auth method (no auth) ──
    stream
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .map_err(|e| anyhow!("SOCKS5 greeting write failed: {}", e))?;

    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await.map_err(|e| anyhow!("SOCKS5 greeting read failed: {}", e))?;

    if buf[0] != 0x05 {
        return Err(anyhow!("SOCKS5 invalid greeting: version={}", buf[0]));
    }
    if buf[1] == 0xFF {
        return Err(anyhow!("SOCKS5 no acceptable auth method"));
    }

    // ── CONNECT request ──
    let mut req = vec![0x05, 0x01, 0x00]; // ver, CONNECT, rsv

    // Encode target address
    if let Ok(ipv4) = host.parse::<Ipv4Addr>() {
        req.push(0x01); // ATYP: IPv4
        req.extend_from_slice(&ipv4.octets());
    } else if let Ok(ipv6) = host.parse::<Ipv6Addr>() {
        req.push(0x04); // ATYP: IPv6
        req.extend_from_slice(&ipv6.octets());
    } else {
        let domain = host.as_bytes();
        if domain.len() > 255 {
            return Err(anyhow!("SOCKS5 domain too long: {} bytes", domain.len()));
        }
        req.push(0x03); // ATYP: domain
        req.push(domain.len() as u8);
        req.extend_from_slice(domain);
    }

    req.extend_from_slice(&port.to_be_bytes());
    stream
        .write_all(&req)
        .await
        .map_err(|e| anyhow!("SOCKS5 CONNECT write failed: {}", e))?;

    // ── Read response ──
    let mut resp = [0u8; 4];
    stream.read_exact(&mut resp).await.map_err(|e| anyhow!("SOCKS5 response read failed: {}", e))?;

    if resp[0] != 0x05 {
        return Err(anyhow!("SOCKS5 invalid response version: {}", resp[0]));
    }
    if resp[1] != 0x00 {
        let msg = match resp[1] {
            1 => "general SOCKS server failure",
            2 => "connection not allowed by ruleset",
            3 => "network unreachable",
            4 => "host unreachable",
            5 => "connection refused",
            6 => "TTL expired",
            7 => "command not supported",
            8 => "address type not supported",
            _ => "unknown error",
        };
        return Err(anyhow!("SOCKS5 CONNECT failed: {} (code={})", msg, resp[1]));
    }

    // Skip the BND.ADDR field (variable length)
    let skip = match resp[3] {
        0x01 => 4,  // IPv4
        0x04 => 16, // IPv6
        0x03 => {
            let mut len_buf = [0u8; 1];
            stream.read_exact(&mut len_buf).await.map_err(|e| anyhow!("SOCKS5 domain len: {}", e))?;
            len_buf[0] as usize
        }
        _ => return Err(anyhow!("SOCKS5 unknown BND.ADDR type: {}", resp[3])),
    };
    let mut addr_buf = vec![0u8; skip + 2]; // addr + port
    stream.read_exact(&mut addr_buf).await.map_err(|e| anyhow!("SOCKS5 BND.ADDR read: {}", e))?;

    tracing::debug!("SOCKS5 tunnel established to {}:{}", host, port);
    Ok(stream)
}

// ── CONNECT handler (HTTPS tunneling) ────────────────────────

async fn handle_connect(
    req: Request<Incoming>,
    socks_port: u16,
    stats: Arc<ProxyStats>,
) -> Result<Response<Full<Bytes>>> {
    let uri = req.uri().clone();
    let (host, port) = parse_authority(uri.authority().ok_or_else(|| anyhow!("CONNECT: missing authority"))?)?;

    tracing::info!("CONNECT tunnel -> {}:{}", host, port);

    // 1. Extract OnUpgrade from the REQUEST extensions (hyper puts it there for CONNECT requests)
    let on_upgrade = req
        .extensions()
        .get::<hyper::upgrade::OnUpgrade>()
        .ok_or_else(|| anyhow!("No upgrade available in request extensions — hyper upgrade mechanism not wired"))?
        .clone();

    // 2. Establish SOCKS5 tunnel to the target
    let outbound = socks5_connect(socks_port, &host, port).await?;

    // 3. Send 200 Connection Established to the client
    let resp = Response::builder()
        .status(StatusCode::OK)
        .body(Full::new(Bytes::new()))
        .map_err(|e| anyhow!("CONNECT response build failed: {}", e))?;

    // 4. Spawn task to pipe bytes after upgrade completes
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded_io) => {
                let mut client_io = TokioIo::new(upgraded_io);
                let mut outbound_io = outbound;
                match tokio::io::copy_bidirectional(&mut client_io, &mut outbound_io).await {
                    Ok((from_client, from_server)) => {
                        stats.bytes_up.fetch_add(from_client, std::sync::atomic::Ordering::Relaxed);
                        stats.bytes_down.fetch_add(from_server, std::sync::atomic::Ordering::Relaxed);
                        tracing::info!(
                            "CONNECT tunnel <-> {}:{} closed (client↔{}B, server↔{}B)",
                            host, port,
                            from_client,
                            from_server
                        );
                    }
                    Err(e) => {
                        tracing::debug!("CONNECT tunnel I/O error: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("CONNECT upgrade failed: {}", e);
            }
        }
    });

    Ok(resp)
}

// ── HTTP request handler (plain HTTP via SOCKS5) ─────────────

async fn handle_http_request(
    req: Request<Incoming>,
    socks_port: u16,
    stats: Arc<ProxyStats>,
) -> Result<Response<Full<Bytes>>> {
    let uri = req.uri().clone();
    let method = req.method().clone();

    let host = uri
        .host()
        .ok_or_else(|| anyhow!("HTTP request missing host in URI"))?
        .to_string();
    let port = uri.port_u16().unwrap_or(80);
    let path = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();

    tracing::debug!("HTTP {} {}:{}{}", method, host, port, path);

    // 1. Connect to SOCKS5 → target
    let socks_stream = socks5_connect(socks_port, &host, port).await?;

    // 2. Build an HTTP/1.1 connection over the SOCKS5 stream
    let io = TokioIo::new(socks_stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| anyhow!("HTTP/1.1 handshake over SOCKS5 failed: {}", e))?;

    // Drive the connection in background
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!("HTTP client connection ended: {}", e);
        }
    });

    // 3. Convert absolute-form URI to origin-form (servers expect /path, not http://host/path)
    let origin_uri: Uri = format!("{}{}{}", path, if uri.query().is_some() { "?" } else { "" }, uri.query().unwrap_or(""))
        .parse()
        .unwrap_or_else(|_| {
            path.parse().unwrap_or_else(|_| Uri::from_static("/"))
        });

    // 4. Build and send the request
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(origin_uri);

    // Copy headers, but fix Host header
    for (key, value) in req.headers().iter() {
        if key == http::header::HOST {
            builder = builder.header(http::header::HOST, format!("{}:{}", host, port));
        } else {
            builder = builder.header(key, value);
        }
    }

    let body = req.collect().await.map_err(|e| anyhow!("request body collect: {}", e))?.to_bytes();
    stats.bytes_up.fetch_add(body.len() as u64, std::sync::atomic::Ordering::Relaxed);
    let outbound_req = builder
        .body(Full::new(body))
        .map_err(|e| anyhow!("request build: {}", e))?;

    let resp = sender
        .send_request(outbound_req)
        .await
        .map_err(|e| anyhow!("SOCKS5 upstream request: {}", e))?;

    // 5. Convert response to our response type
    let status = resp.status();
    let mut builder = Response::builder().status(status);
    for (key, value) in resp.headers().iter() {
        builder = builder.header(key, value);
    }

    let resp_body = resp.into_body().collect().await.map_err(|e| anyhow!("response body: {}", e))?.to_bytes();
    stats.bytes_down.fetch_add(resp_body.len() as u64, std::sync::atomic::Ordering::Relaxed);
    builder
        .body(Full::new(resp_body))
        .map_err(|e| anyhow!("response build: {}", e))
}

// ── Helpers ──────────────────────────────────────────────────

/// Parse "host:port" from a URI authority.
fn parse_authority(auth: &Authority) -> Result<(String, u16)> {
    let s = auth.as_str();

    // Handle IPv6: [::1]:8080
    if s.starts_with('[') {
        if let Some(bracket_end) = s.find("]:") {
            let host = s[1..bracket_end].to_string();
            let port: u16 = s[bracket_end + 2..].parse().map_err(|e| anyhow!("port parse: {}", e))?;
            return Ok((host, port));
        }
        // [::1] without port
        let host = s[1..s.len() - 1].to_string();
        return Ok((host, 80));
    }

    // host:port or just host
    match s.rsplit_once(':') {
        Some((host, port_str)) => {
            let port: u16 = port_str.parse().map_err(|e| anyhow!("port parse: {}", e))?;
            Ok((host.to_string(), port))
        }
        None => Ok((s.to_string(), 80)),
    }
}
