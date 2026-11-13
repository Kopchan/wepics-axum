//! `GET /api/v1/albums/{hash}` — album info, children and media previews.

use axum::Json;
use axum::extract::{Path, Query, State};
use sqlx::{QueryBuilder, Sqlite};

use crate::error::AppError;
use crate::indexer::{self, SyncStart};
use crate::models::{Album, Media, MediaType};
use crate::resolvers::resolve_album;
use crate::state::AppState;

use super::secure::link_expiry;
use super::types::{
    AlbumItem, AlbumQuery, AlbumSort, MEDIA_SELECT, MediaItem, MediaSort, load_album_stats,
    parse_album_sort, parse_media_sort, parse_media_types, parse_preview_count, push_album_order,
    push_media_order, push_media_type_filter,
};

const ALBUM_COLUMNS: &str = "SELECT id, name, hash, parent_id, path, indexed_at, sort_name, \
     created_at, updated_at FROM albums";
const ALBUM_BY_ID: &str = "SELECT id, name, hash, parent_id, path, indexed_at, sort_name, \
     created_at, updated_at FROM albums WHERE id = ?1";

/// Laravel loads four previews per album unless `images` is explicitly `0`.
const DEFAULT_PREVIEWS: u32 = 4;
/// Laravel loads children by default; `simple` was the opt-out.
const DEFAULT_LAYERS: u8 = 1;

/// `GET /api/v1/albums/{hash}`.
pub async fn get_album(
    State(state): State<AppState>,
    Path(hash): Path<String>,
    Query(query): Query<AlbumQuery>,
) -> Result<Json<AlbumItem>, AppError> {
    let album = resolve_album(&state, &hash).await?;

    // Same behaviour as the existing SPA endpoint: a stale or never-indexed
    // album starts a background scan and the current (possibly empty) index is
    // returned immediately.
    let sync = match indexer::start_if_stale(&state, &album) {
        SyncStart::Fresh => None,
        SyncStart::Started => Some("started".to_string()),
        SyncStart::Running => Some("running".to_string()),
    };

    let media_sort = parse_media_sort(query.msort.as_deref())?;
    let album_sort = parse_album_sort(query.asort.as_deref())?;
    let media_types = parse_media_types(query.mtype.as_deref())?;
    let preview_limit = parse_preview_count(query.mlimit, DEFAULT_PREVIEWS)?;
    let nested_preview_limit = parse_preview_count(query.mlimitnest, DEFAULT_PREVIEWS)?;
    let layers = query.layers.unwrap_or(DEFAULT_LAYERS).min(2);
    let exp = link_expiry();

    let mut item = build_album(
        &state,
        &album,
        preview_limit,
        layers,
        nested_preview_limit,
        &media_sort,
        &album_sort,
        &media_types,
        exp,
    )
    .await?;

    item.sync = sync;
    item.ancestors = load_ancestors(&state, &album).await?;

    Ok(Json(item))
}

#[allow(clippy::too_many_arguments)]
async fn build_album(
    state: &AppState,
    album: &Album,
    preview_limit: u32,
    child_layers: u8,
    nested_preview_limit: u32,
    media_sort: &[MediaSort],
    album_sort: &[AlbumSort],
    media_types: &[MediaType],
    exp: i64,
) -> Result<AlbumItem, AppError> {
    let stats = load_album_stats(&state.db, album.id).await?;
    let mut item = AlbumItem::new(album, stats);

    if preview_limit > 0 {
        item.images =
            Some(load_previews(state, album, preview_limit, media_sort, media_types, exp).await?);
    }

    if child_layers > 0 {
        let children = load_children(state, album, album_sort).await?;
        let mut items = Vec::with_capacity(children.len());
        for child in &children {
            items.push(
                Box::pin(build_album(
                    state,
                    child,
                    nested_preview_limit,
                    child_layers - 1,
                    nested_preview_limit,
                    media_sort,
                    album_sort,
                    media_types,
                    exp,
                ))
                .await?,
            );
        }
        item.children = Some(items);
    }

    Ok(item)
}

async fn load_children(
    state: &AppState,
    album: &Album,
    sorts: &[AlbumSort],
) -> Result<Vec<Album>, AppError> {
    let mut builder = QueryBuilder::<Sqlite>::new(ALBUM_COLUMNS);
    builder.push(" WHERE parent_id = ").push_bind(album.id);
    push_album_order(&mut builder, sorts);

    Ok(builder.build_query_as().fetch_all(&state.db).await?)
}

async fn load_previews(
    state: &AppState,
    album: &Album,
    limit: u32,
    sorts: &[MediaSort],
    types: &[MediaType],
    exp: i64,
) -> Result<Vec<MediaItem>, AppError> {
    let mut builder = QueryBuilder::<Sqlite>::new(MEDIA_SELECT);
    builder.push(" WHERE album_id = ").push_bind(album.id);
    push_media_type_filter(&mut builder, types);
    push_media_order(&mut builder, sorts);
    builder.push(" LIMIT ").push_bind(i64::from(limit));

    let rows: Vec<Media> = builder.build_query_as().fetch_all(&state.db).await?;
    Ok(rows
        .iter()
        .map(|media| MediaItem::new(state, album, media, exp))
        .collect())
}

/// Ancestors in root-first order, i.e. `[root, …, parent]`.
async fn load_ancestors(
    state: &AppState,
    album: &Album,
) -> Result<Option<Vec<AlbumItem>>, AppError> {
    let mut ancestors = Vec::new();
    let mut parent_id = album.parent_id;

    // The schema only prevents self-parenting, so bound the walk defensively.
    for _ in 0..128 {
        let Some(id) = parent_id else {
            break;
        };

        let parent: Option<Album> = sqlx::query_as(ALBUM_BY_ID)
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
        let Some(parent) = parent else {
            break;
        };

        parent_id = parent.parent_id;
        let stats = load_album_stats(&state.db, parent.id).await?;
        ancestors.push(AlbumItem::new(&parent, stats));
    }

    ancestors.reverse();
    Ok((!ancestors.is_empty()).then_some(ancestors))
}
