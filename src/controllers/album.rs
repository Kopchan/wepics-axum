use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite};

use super::pagination::Pagination;
use crate::error::AppError;
use crate::indexer::{self, SyncStart};
use crate::models::Album;
use crate::state::AppState;

const ALBUM_SELECT: &str = "SELECT id, name, hash, parent_id, path, indexed_at, sort_name, \
     created_at, updated_at FROM albums WHERE hash = ?1";
const CHILDREN_SELECT: &str = "SELECT id, name, hash, parent_id, path, indexed_at, sort_name, \
     created_at, updated_at FROM albums";

/// `GET /api/albums/{hash}/child?page=1&limit=30&sort=name`
#[derive(Debug, Deserialize)]
pub struct ChildrenQuery {
    pub page: Option<u32>,
    pub limit: Option<u32>,
    /// `name` or `created_at`, optionally prefixed with `-` for descending order.
    pub sort: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AlbumPage {
    pub items: Vec<Album>,
    pub page: u32,
    pub limit: u32,
    pub total: i64,
    pub pages: i64,
}

/// Response of `GET /api/album/{hash}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AlbumInfo {
    pub album: Album,
    /// `"started"` when this request spawned a task, `"running"` when one was
    /// already in flight; absent when the index is fresh.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sync: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortField {
    Name,
    CreatedAt,
}

#[derive(Debug, Clone, Copy)]
struct Sort {
    field: SortField,
    descending: bool,
}

impl Sort {
    fn parse(raw: Option<&str>) -> Result<Self, AppError> {
        let raw = raw.unwrap_or_default();
        let (descending, field) = match raw.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };

        let field = match field {
            // Default sort: natural order by name.
            "" | "name" => SortField::Name,
            "created_at" => SortField::CreatedAt,
            other => {
                return Err(AppError::BadRequest(format!(
                    "invalid sort `{other}`, expected one of: name, -name, created_at, -created_at"
                )));
            }
        };

        Ok(Self { field, descending })
    }
}

/// `GET /api/albums/{hash}/child` — direct children of an album, paginated.
pub async fn list_children(
    State(state): State<AppState>,
    Path(album_hash): Path<String>,
    Query(query): Query<ChildrenQuery>,
) -> Result<Json<AlbumPage>, AppError> {
    let album = resolve_album(&state, &album_hash).await?;
    let pagination = Pagination::new(query.page, query.limit)?;
    let sort = Sort::parse(query.sort.as_deref())?;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM albums WHERE parent_id = ?1")
        .bind(album.id)
        .fetch_one(&state.db)
        .await?;

    let mut builder = QueryBuilder::<Sqlite>::new(CHILDREN_SELECT);
    builder.push(" WHERE parent_id = ");
    builder.push_bind(album.id);
    builder.push(" ORDER BY ");
    match sort.field {
        SortField::Name => builder.push("sort_name"),
        SortField::CreatedAt => builder.push("created_at"),
    };
    builder.push(if sort.descending { " DESC" } else { " ASC" });
    // `id` keeps pages stable when names/timestamps collide.
    builder.push(", id ");
    builder.push(if sort.descending { " DESC" } else { " ASC" });
    builder.push(" LIMIT ");
    builder.push_bind(i64::from(pagination.limit));
    builder.push(" OFFSET ");
    builder.push_bind(pagination.offset);

    let items: Vec<Album> = builder.build_query_as().fetch_all(&state.db).await?;

    Ok(Json(AlbumPage {
        items,
        page: pagination.page,
        limit: pagination.limit,
        total,
        pages: pagination.pages(total),
    }))
}

/// `GET /api/album/{hash}` — `hash` may be the literal `root`.
///
/// Starts a background filesystem sync when the album was never indexed or its
/// index is older than an hour. The response is wrapped in an `album` key.
pub async fn get_album(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<AlbumInfo>, AppError> {
    let album = resolve_album(&state, &hash).await?;
    let sync = match indexer::start_if_stale(&state, &album) {
        SyncStart::Fresh => None,
        SyncStart::Started => Some("started".to_string()),
        SyncStart::Running => Some("running".to_string()),
    };

    Ok(Json(AlbumInfo { album, sync }))
}

/// Looks an album up by hash. The root album is created at startup by
/// [`AppState::initialize`], so this is a pure read.
pub(crate) async fn resolve_album(state: &AppState, hash: &str) -> Result<Album, AppError> {
    sqlx::query_as(ALBUM_SELECT)
        .bind(hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}
