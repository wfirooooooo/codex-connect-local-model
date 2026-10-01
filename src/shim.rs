use std::convert::Infallible;
use std::io::Write;
use std::path::PathBuf;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::HttpClient;

type ProxyBody = BoxBody<Bytes, hyper::Error>;

pub async fn serve(listener: TcpListener, upstream: Uri, client: HttpClient, log: Option<PathBuf>) {
    if let Some(dir) = log.as_ref().and_then(|path| path.parent()) {
        let _ = std::fs::create_dir_all(dir);
    }
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let client = client.clone();
        let upstream = upstream.clone();
        let log = log.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let client = client.clone();
                let upstream = upstream.clone();
                let log = log.clone();
                async move { proxy(request, client, upstream, log).await }
            });
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                eprintln!("connection error: {err}");
            }
        });
    }
}

async fn proxy(
    request: Request<Incoming>,
    client: HttpClient,
    upstream: Uri,
    log: Option<PathBuf>,
) -> Result<Response<ProxyBody>, Infallible> {
    let (parts, body) = request.into_parts();
    let body = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => return Ok(error(StatusCode::BAD_REQUEST, &format!("cannot read request body: {err}"))),
    };

    let (body, dropped) = if parts.method == Method::POST && parts.uri.path() == "/v1/responses" {
        drop_unsupported_tools(body)
    } else {
        (body, Vec::new())
    };
    if !dropped.is_empty()
        && let Some(path) = log
        && let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(file, "dropped tools: {}", dropped.join(", "));
    }

    let uri = match join(&upstream, &parts.uri) {
        Ok(uri) => uri,
        Err(err) => return Ok(error(StatusCode::INTERNAL_SERVER_ERROR, &err)),
    };

    let mut builder = Request::builder().method(parts.method).uri(uri);
    for (name, value) in parts.headers.iter() {
        match name.as_str() {
            "host" | "content-length" | "connection" | "transfer-encoding" | "accept-encoding" => {}
            _ => {
                builder = builder.header(name, value);
            }
        }
    }

    let request = match builder.body(Full::new(body)) {
        Ok(request) => request,
        Err(err) => return Ok(error(StatusCode::BAD_REQUEST, &format!("cannot build request: {err}"))),
    };

    match client.request(request).await {
        Ok(response) => {
            let (parts, body) = response.into_parts();
            Ok(Response::from_parts(parts, body.boxed()))
        }
        Err(err) => Ok(error(StatusCode::BAD_GATEWAY, &format!("upstream error: {err}"))),
    }
}

fn drop_unsupported_tools(raw: Bytes) -> (Bytes, Vec<String>) {
    let Ok(mut document) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return (raw, Vec::new());
    };
    let Some(tools) = document.get_mut("tools").and_then(|value| value.as_array_mut()) else {
        return (raw, Vec::new());
    };

    let mut dropped = Vec::new();
    tools.retain(|tool| {
        if tool.get("type").and_then(|value| value.as_str()) == Some("function") {
            return true;
        }
        let name = tool
            .get("name")
            .or_else(|| tool.get("type"))
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        dropped.push(name.to_owned());
        false
    });

    if dropped.is_empty() {
        return (raw, dropped);
    }
    let bytes = serde_json::to_vec(&document).map(Bytes::from).unwrap_or(raw);
    (bytes, dropped)
}

fn join(base: &Uri, incoming: &Uri) -> Result<Uri, String> {
    let scheme = base.scheme_str().unwrap_or("http");
    let authority = base.authority().ok_or("upstream URL is missing a host")?;
    let path = format!("{}{}", base.path().trim_end_matches('/'), incoming.path());
    let path_and_query = match incoming.query() {
        Some(query) => format!("{path}?{query}"),
        None => path,
    };
    Uri::builder()
        .scheme(scheme)
        .authority(authority.clone())
        .path_and_query(path_and_query)
        .build()
        .map_err(|err| format!("invalid upstream URL: {err}"))
}

fn error(status: StatusCode, message: &str) -> Response<ProxyBody> {
    let body = serde_json::json!({ "error": { "message": message, "type": "invalid_request_error" } });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full(Bytes::from(body.to_string())))
        .expect("static response is valid")
}

fn full(bytes: Bytes) -> ProxyBody {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}
