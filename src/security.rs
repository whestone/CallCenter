//! Querschnittliche Schutzmaßnahmen.

use axum::{
    extract::Request,
    http::{header, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// CSRF-Schutz: Schreibende Anfragen, die laut Origin/Referer von einer fremden
/// Seite stammen, werden abgewiesen (OWASP "Verifying Origin With Standard Headers").
/// Zusammen mit SameSite=Lax-Cookies verhindert das, dass fremde Seiten im Namen
/// eines angemeldeten Agenten Formulare absenden.
pub async fn csrf_guard(req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let headers = req.headers();
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let source = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|v| v.to_str().ok());
    if let (Some(host), Some(source)) = (host, source) {
        if !same_host(source, host) {
            tracing::warn!(source, host, "CSRF-Schutz: fremde Anfrage blockiert");
            return (StatusCode::FORBIDDEN, "Anfrage von fremder Seite blockiert (CSRF-Schutz)").into_response();
        }
    }
    next.run(req).await
}

fn same_host(source: &str, host: &str) -> bool {
    source
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or("") == host)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::same_host;

    #[test]
    fn origin_checks() {
        assert!(same_host("http://127.0.0.1:3000", "127.0.0.1:3000"));
        assert!(same_host("http://127.0.0.1:3000/tickets/1?x=1", "127.0.0.1:3000"));
        assert!(!same_host("https://evil.example", "127.0.0.1:3000"));
        assert!(!same_host("http://127.0.0.1:3000.evil.example/", "127.0.0.1:3000"));
        assert!(!same_host("null", "127.0.0.1:3000"));
    }
}
