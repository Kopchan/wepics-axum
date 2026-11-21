//! HTML entry point: the SPA shell for people, server-side OG tags for bots.

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HOST, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;

use crate::error::AppError;
use crate::helpers::secure_link;
use crate::models::{Album, ROOT_ALBUM_HASH};
use crate::resolvers::resolve_album;
use crate::state::AppState;

const SITE_NAME: &str = "wepics";
const DEFAULT_DESCRIPTION: &str = "Фотографии и видео, собранные в альбомы.";

/// Directory with the static frontend (JS, CSS, favicon, fonts).
pub(crate) fn public_dir() -> PathBuf {
    std::env::var_os("PUBLIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("public"))
}

/// `GET /` — the SPA for people, a page with OG tags for bots.
pub async fn root_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !is_bot(&headers) {
        return serve_index().await;
    }

    let album = match resolve_album(&state, ROOT_ALBUM_HASH).await {
        Ok(album) => album,
        Err(AppError::NotFound) => return Ok(not_found_page()),
        Err(err) => return Err(err),
    };

    render_bot_page(&state, &album, &headers).await
}

/// Fallback for `/<albumHash>` and other SPA routes.
pub async fn spa(State(state): State<AppState>, request: Request) -> Result<Response, AppError> {
    let path = request.uri().path().to_owned();

    // API/media/static paths must stay 404s instead of turning into HTML.
    if path.starts_with("/api")
        || path.starts_with("/file")
        || path.starts_with("/thumb")
        || path.starts_with("/public")
    {
        return Err(AppError::NotFound);
    }
    if !is_bot(request.headers()) {
        return serve_index().await;
    }

    let hash = path.trim_matches('/');
    if hash.is_empty() || hash.contains('/') {
        return Ok(not_found_page());
    }

    let album = match resolve_album(&state, hash).await {
        Ok(album) => album,
        Err(AppError::NotFound) => return Ok(not_found_page()),
        Err(err) => return Err(err),
    };

    render_bot_page(&state, &album, request.headers()).await
}

async fn serve_index() -> Result<Response, AppError> {
    let html = tokio::fs::read_to_string(public_dir().join("index.html")).await?;
    Ok(html_response(html, StatusCode::OK))
}

async fn render_bot_page(
    state: &AppState,
    album: &Album,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let base = base_url(headers);
    let is_root = album.hash == ROOT_ALBUM_HASH;

    let image: Option<(String, i64, i64)> = sqlx::query_as(
        "SELECT hash, width, height FROM media \
         WHERE album_id = ?1 AND \"type\" IN ('image', 'vector', 'animated') \
         ORDER BY date IS NULL, date, id LIMIT 1",
    )
    .bind(album.id)
    .fetch_optional(&state.db)
    .await?;

    let media_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE album_id = ?1")
        .bind(album.id)
        .fetch_one(&state.db)
        .await?;

    let title = if is_root {
        SITE_NAME.to_string()
    } else {
        format!("{} — {SITE_NAME}", album.name)
    };
    let description = if is_root {
        DEFAULT_DESCRIPTION.to_string()
    } else {
        format!("Альбом «{}», {media_count} медиа.", album.name)
    };

    let canonical_path = if is_root {
        "/".to_string()
    } else {
        format!("/{}", album.hash)
    };
    let url = format!("{base}{canonical_path}");

    let image = image.map(|(hash, width, height)| {
        let exp = secure_link::expires_at(chrono::Utc::now().timestamp());
        let token = state.sign_media(&album.hash, &hash, exp);
        OgImage {
            url: format!(
                "{base}/file/v1/{}/{}?sign={token}&exp={exp}",
                encode_segment(&album.hash),
                encode_segment(&hash)
            ),
            width,
            height,
        }
    });

    let meta = social_meta(&title, &description, &url, image.as_ref(), &album.name);
    let index = tokio::fs::read_to_string(public_dir().join("index.html")).await?;

    Ok(html_response(
        inject_meta(&index, &title, &meta),
        StatusCode::OK,
    ))
}

struct OgImage {
    url: String,
    width: i64,
    height: i64,
}

fn social_meta(
    title: &str,
    description: &str,
    url: &str,
    image: Option<&OgImage>,
    image_alt: &str,
) -> String {
    let mut out = String::new();
    push_meta(&mut out, "property", "og:site_name", SITE_NAME);
    push_meta(&mut out, "property", "og:type", "website");
    push_meta(&mut out, "property", "og:title", title);
    push_meta(&mut out, "property", "og:description", description);
    push_meta(&mut out, "property", "og:url", url);
    push_meta(&mut out, "name", "twitter:card", "summary_large_image");
    push_meta(&mut out, "name", "twitter:title", title);
    push_meta(&mut out, "name", "twitter:description", description);

    if let Some(image) = image {
        push_meta(&mut out, "property", "og:image", &image.url);
        push_meta(&mut out, "property", "og:image:alt", image_alt);
        if image.width > 0 {
            push_meta(
                &mut out,
                "property",
                "og:image:width",
                &image.width.to_string(),
            );
        }
        if image.height > 0 {
            push_meta(
                &mut out,
                "property",
                "og:image:height",
                &image.height.to_string(),
            );
        }
        push_meta(&mut out, "name", "twitter:image", &image.url);
    }

    out.push_str(&format!(
        "<link rel=\"canonical\" href=\"{}\">\n",
        escape_html(url)
    ));
    out
}

fn push_meta(out: &mut String, attr: &str, key: &str, value: &str) {
    out.push_str("<meta ");
    out.push_str(attr);
    out.push_str("=\"");
    out.push_str(key);
    out.push_str("\" content=\"");
    out.push_str(&escape_html(value));
    out.push_str("\">\n");
}

fn inject_meta(index: &str, title: &str, meta: &str) -> String {
    let mut html = index.to_owned();

    if let (Some(start), Some(end)) = (html.find("<title>"), html.find("</title>"))
        && start + "<title>".len() <= end
    {
        html.replace_range(start + "<title>".len()..end, &escape_html(title));
    }

    match html.find("</head>") {
        Some(pos) => html.insert_str(pos, meta),
        None => html = format!("{meta}{html}"),
    }

    html
}

fn html_response(body: String, status: StatusCode) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn not_found_page() -> Response {
    html_response(
        "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\
         <title>404 — wepics</title></head><body><h1>404</h1>\
         <p>Альбом не найден.</p><p><a href=\"/\">На главную</a></p></body></html>"
            .to_string(),
        StatusCode::NOT_FOUND,
    )
}

fn is_bot(headers: &HeaderMap) -> bool {
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();

    [
        "bot",
        "crawler",
        "spider",
        "facebookexternalhit",
        "twitterbot",
        "telegrambot",
        "slackbot",
        "discordbot",
        "linkedinbot",
        "whatsapp",
        "pinterest",
        "embedly",
        "redditbot",
        "applebot",
    ]
    .iter()
    .any(|needle| user_agent.contains(needle))
}

fn base_url(headers: &HeaderMap) -> String {
    if let Ok(value) = std::env::var("PUBLIC_URL") {
        let value = value.trim_end_matches('/');
        if !value.is_empty() {
            return value.to_owned();
        }
    }

    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .filter(|value| *value == "http" || *value == "https")
        .unwrap_or("http");

    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(HOST))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("localhost");

    format!("{scheme}://{host}")
}

fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}
