//! `GET /api/v1/album/{hash}/media` — paginated media of one album — and
//! `GET /api/v1/album/{hash}/media/{media}` for a single item.

use axum::Json;
use axum::extract::{Path, Query, State};

use crate::error::AppError;
use crate::helpers::MAX_LIMIT;
use crate::helpers::flags::flag;
use crate::helpers::pagination::DEFAULT_LIMIT;
use crate::resolvers::{resolve_album, resolve_media};
use crate::state::AppState;

use super::secure::link_expiry;
use super::types::{
    Limit, MediaItem, MediaListQuery, MediaSelection, PaginateRes, count_media, load_media_items,
    parse_limit, parse_media_sort, parse_media_types,
};

/// `GET /api/v1/album/{hash}/media`.
pub async fn list_media(
    State(state): State<AppState>,
    Path(album_hash): Path<String>,
    Query(query): Query<MediaListQuery>,
) -> Result<Json<PaginateRes<MediaItem>>, AppError> {
    let album = resolve_album(&state, &album_hash).await?;
    let limit = parse_limit(query.limit.as_deref(), Limit::Fixed(DEFAULT_LIMIT))?;
    let page = query.page.unwrap_or(1);
    if page == 0 {
        return Err(AppError::BadRequest("page must be >= 1".into()));
    }

    let nested = flag(query.nest.as_deref());
    let sorts = parse_media_sort(query.sort.as_deref())?;
    let types = parse_media_types(query.media_type.as_deref())?;

    let total = count_media(&state.db, album.id, nested, &types).await?;

    let (limit, offset) = match limit {
        Limit::All => (None, 0),
        Limit::Fixed(0) => {
            return Err(AppError::BadRequest("limit must be >= 1".into()));
        }
        Limit::Fixed(limit) if limit > MAX_LIMIT => {
            return Err(AppError::BadRequest(format!(
                "limit must be between 1 and {MAX_LIMIT}"
            )));
        }
        Limit::Fixed(limit) => (Some(limit), i64::from(page - 1) * i64::from(limit)),
    };

    let items = load_media_items(
        &state,
        &MediaSelection {
            album_id: album.id,
            nested,
            types: &types,
            sorts: &sorts,
            limit,
            offset,
            exp: link_expiry(),
        },
    )
    .await?;

    Ok(Json(PaginateRes { total, items }))
}

/// `GET /api/v1/album/{hash}/media/{media}`.
pub async fn get_media(
    State(state): State<AppState>,
    Path((album_hash, media_hash)): Path<(String, String)>,
) -> Result<Json<MediaItem>, AppError> {
    let album = resolve_album(&state, &album_hash).await?;
    let media = resolve_media(&state, &album, &media_hash).await?;

    Ok(Json(MediaItem::new(&state, &album, &media, link_expiry())))
}
