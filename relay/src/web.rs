//! The web client, embedded in the binary.
//!
//! Embedding rather than serving a directory keeps the image a single file,
//! removes a runtime path to get wrong, and leaves no filesystem to traverse.
//!
//! These routes sit outside the auth layer on purpose: the page has to load
//! before the user can enter a token, and it carries no secret of its own. The
//! API it talks to is still behind the layer.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE_CSS: &str = include_str!("../static/style.css");

/// Cache headers for the assets.
///
/// `no-cache` rather than a long max-age: the three files are a few kilobytes,
/// and a stale client after a relay upgrade would talk to an API it no longer
/// matches.
const CACHE: &str = "no-cache";

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static(CACHE)),
            // The page loads only its own two assets and talks only to its own
            // origin, so it can afford a policy this tight.
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'none'; script-src 'self'; style-src 'self'; \
                     connect-src 'self'; base-uri 'none'; form-action 'none'; \
                     frame-ancestors 'none'",
                ),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        body,
    )
        .into_response()
}

async fn index() -> Response {
    asset(INDEX_HTML, "text/html; charset=utf-8")
}

async fn app_js() -> Response {
    asset(APP_JS, "text/javascript; charset=utf-8")
}

async fn style_css() -> Response {
    asset(STYLE_CSS, "text/css; charset=utf-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn get_path(path: &str) -> (StatusCode, String, String) {
        let router: Router = routes();
        let response = router
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();

        (
            status,
            content_type,
            String::from_utf8_lossy(&body).into_owned(),
        )
    }

    #[tokio::test]
    async fn the_page_is_served_without_a_token() {
        let (status, content_type, body) = get_path("/").await;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/html"));
        assert!(body.contains("mikmik relay"));
    }

    #[tokio::test]
    async fn the_script_is_served_as_javascript() {
        let (status, content_type, _) = get_path("/app.js").await;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/javascript"));
    }

    #[tokio::test]
    async fn the_stylesheet_is_served_as_css() {
        let (status, content_type, _) = get_path("/style.css").await;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/css"));
    }

    /// The page must never reach for a remote script or an inline one; that is
    /// the whole reason the policy is this narrow.
    #[tokio::test]
    async fn the_page_carries_a_content_security_policy() {
        let router: Router = routes();
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        let policy = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();

        assert!(policy.contains("script-src 'self'"));
        assert!(policy.contains("frame-ancestors 'none'"));
    }
}
