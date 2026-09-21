//! Local transport boundary. Local processes are trusted; arbitrary websites are not.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub struct LocalAccess {
    authorities: Vec<String>,
}

impl LocalAccess {
    pub fn new(address: SocketAddr) -> Self {
        let authorities = vec![address.to_string(), format!("localhost:{}", address.port())];
        Self { authorities }
    }
}

pub async fn guard(State(policy): State<Arc<LocalAccess>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let hosts: Vec<_> = headers.get_all(header::HOST).iter().collect();
    let authority = if hosts.len() == 1 {
        hosts[0].to_str().ok()
    } else if hosts.is_empty() {
        request.uri().authority().map(|a| a.as_str())
    } else {
        None
    };
    if !authority.is_some_and(|a| policy.authorities.iter().any(|allowed| allowed.eq_ignore_ascii_case(a))) {
        return (StatusCode::FORBIDDEN, "untrusted API host").into_response();
    }
    if headers.contains_key(header::ORIGIN) || headers.contains_key("sec-fetch-site") {
        return (StatusCode::FORBIDDEN, "browser clients are not supported").into_response();
    }
    let grpc_web = headers.get_all(header::CONTENT_TYPE).iter().any(|v| v.to_str().is_ok_and(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase().starts_with("application/grpc-web")));
    if grpc_web || headers.contains_key("x-grpc-web") {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "use native gRPC").into_response();
    }
    next.run(request).await
}
