//! `GET /api/v1/album/{hash}/media` — paginated media of one album.

use axum::Json;
use axum::extract::{Path, Query, State};
use sqlx::{QueryBuilder, Sqlite};

use crate::controllers::album::resolve_album;
use crate::controllers::pagination::Pagination;
use crate::error::AppError;
use crate::models::Media;
use crate::state::AppState;

use super::secure::link_expiry;
use super::types::{
    MEDIA_SELECT, MediaItem, MediaListQuery, MediaPage, parse_media_sort, parse_media_types,
    push_media_order, push_media_type_filter,
};

/// `GET /api/v1/album/{hash}/media`.
pub async fn list_media(
    State(state): State<AppState>,
    Path(album_hash): Path<String>,
    Query(query): Query<MediaListQuery>,
) -> Result<Json<MediaPage>, AppError> {
    let album = resolve_album(&state, &album_hash).await?;
    let pagination = Pagination::new(query.page, query.limit)?;
    let sorts = parse_media_sort(query.sort.as_deref())?;
    let types = parse_media_types(query.media_type.as_deref())?;

    let mut counter = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM media WHERE album_id = ");
    counter.push_bind(album.id);
    push_media_type_filter(&mut counter, &types);
    let total: i64 = counter.build_query_scalar().fetch_one(&state.db).await?;

    let mut builder = QueryBuilder::<Sqlite>::new(MEDIA_SELECT);
    builder.push(" WHERE album_id = ").push_bind(album.id);
    push_media_type_filter(&mut builder, &types);
    push_media_order(&mut builder, &sorts);
    builder
        .push(" LIMIT ")
        .push_bind(i64::from(pagination.limit));
    builder.push(" OFFSET ").push_bind(pagination.offset);

    let rows: Vec<Media> = builder.build_query_as().fetch_all(&state.db).await?;
    let exp = link_expiry();
    let pictures = rows
        .iter()
        .map(|media| MediaItem::new(&state, &album, media, exp))
        .collect();

    Ok(Json(MediaPage {
        page: pagination.page,
        per_page: pagination.limit,
        total,
        pages: pagination.pages(total),
        pictures,
    }))
}
