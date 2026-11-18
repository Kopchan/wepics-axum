//! `GET /api/v1/album/{hash}` and `/childs` — album info, children and media
//! previews.
//!
//! Requests are assembled in a constant number of batched queries: children for
//! a whole layer, media previews for a whole layer, and stats for every album
//! in the response. No query is issued per album.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use sqlx::{QueryBuilder, Sqlite};

use crate::error::AppError;
use crate::helpers::flags::flag;
use crate::indexer::{self, SyncStart};
use crate::models::{Album, MediaType};
use crate::resolvers::resolve_album;
use crate::state::AppState;

use super::secure::link_expiry;
use super::types::{
    ALBUM_FIELDS_QUALIFIED, AlbumItem, AlbumQuery, AlbumSort, AlbumStatsPair, Limit, MediaItem,
    MediaPreview, MediaSort, PaginateRes, load_album_stats_map, load_children_batch,
    load_content_sort_map, load_media_previews, parse_album_sort, parse_layer_bools,
    parse_layer_limits, parse_limit, parse_media_sort, parse_media_types, push_album_order,
};

/// Album info defaults: no children and no media unless the client asks.
const DEFAULT_ALIMITS: &[Limit] = &[Limit::Fixed(0)];
const DEFAULT_MLIMITS: &[Limit] = &[Limit::Fixed(0)];
/// `/childs` defaults mirror the client: a page of children without media.
const DEFAULT_CHILD_ALIMITS: &[Limit] = &[Limit::Fixed(10)];
const DEFAULT_CHILD_MLIMITS: &[Limit] = &[Limit::Fixed(0)];

/// Per-layer options shared by every album in one response tree.
struct AlbumBuildConfig<'a> {
    alimits: &'a [Limit],
    mlimits: &'a [Limit],
    mnests: &'a [bool],
    media_sort: &'a [MediaSort],
    album_sort: &'a [AlbumSort],
    media_types: &'a [MediaType],
    exp: i64,
}

/// A fully loaded album response tree: rows, relations and counters.
struct AlbumTree {
    albums: HashMap<i64, Album>,
    /// Child ids per album, already ordered and limited by `alimit`.
    children: HashMap<i64, Vec<i64>>,
    /// Previews per album, already ordered and limited by `mlimit`.
    media: HashMap<i64, Vec<MediaItem>>,
    stats: HashMap<i64, AlbumStatsPair>,
    /// `contentSort` value per album for a `content:<field>` sort.
    content: HashMap<i64, String>,
}

impl AlbumTree {
    /// Loads all albums, previews and stats needed for one response.
    ///
    /// `context_album_id` is the album from the request path; media owned by it
    /// are serialized without their `album`.
    async fn load(
        state: &AppState,
        roots: &[Album],
        context_album_id: i64,
        config: &AlbumBuildConfig<'_>,
        extra_stats: &[i64],
    ) -> Result<Self, AppError> {
        let mut albums = HashMap::new();
        let mut layers: Vec<Vec<i64>> = vec![Vec::new()];
        for root in roots {
            albums.insert(root.id, root.clone());
            layers[0].push(root.id);
        }

        let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut media: HashMap<i64, Vec<MediaItem>> = HashMap::new();

        // `alimit[0]` describes children of layer 0, so the number of album
        // layers is one more than the number of consecutive enabled limits.
        let depth = config
            .alimits
            .iter()
            .take_while(|limit| !is_disabled(**limit))
            .count();

        for level in 0..depth {
            let parent_ids = layers[level].clone();
            for id in &parent_ids {
                children.entry(*id).or_default();
            }

            let rows = load_children_batch(
                &state.db,
                &parent_ids,
                config.album_sort,
                limit_option(config.alimits[level]),
            )
            .await?;

            let mut next = Vec::with_capacity(rows.len());
            for row in rows {
                if let Some(parent_id) = row.parent_id {
                    children.entry(parent_id).or_default().push(row.id);
                }
                next.push(row.id);
                albums.insert(row.id, row);
            }
            layers.push(next);
        }

        for (level, ids) in layers.iter().enumerate() {
            let Some(limit) = config.mlimits.get(level).copied() else {
                continue;
            };
            if is_disabled(limit) {
                continue;
            }

            let nested = config.mnests.get(level).copied().unwrap_or(false);
            media.extend(
                load_media_previews(
                    state,
                    ids,
                    &MediaPreview {
                        context_album_id,
                        nested,
                        types: config.media_types,
                        sorts: config.media_sort,
                        limit: limit_option(limit),
                        exp: config.exp,
                    },
                )
                .await?,
            );
        }

        let mut stats_ids: Vec<i64> = albums.keys().copied().collect();
        stats_ids.extend_from_slice(extra_stats);
        stats_ids.sort_unstable();
        stats_ids.dedup();
        let stats = load_album_stats_map(&state.db, &stats_ids).await?;
        let content = load_content_sort_map(&state.db, &stats_ids, config.album_sort).await?;

        Ok(Self {
            albums,
            children,
            media,
            stats,
            content,
        })
    }

    fn build_item(&self, id: i64) -> AlbumItem {
        let album = self.albums.get(&id).expect("album tree is complete");
        let mut item = AlbumItem::new(album, self.stats_for(id));
        item.content_sort = self.content_for(id);

        if let Some(previews) = self.media.get(&id) {
            item.media = Some(previews.clone());
        }

        if let Some(child_ids) = self.children.get(&id) {
            item.children = Some(
                child_ids
                    .iter()
                    .map(|child_id| self.build_item(*child_id))
                    .collect(),
            );
        }

        item
    }

    fn stats_for(&self, id: i64) -> AlbumStatsPair {
        self.stats.get(&id).copied().unwrap_or_default()
    }

    fn content_for(&self, id: i64) -> Option<String> {
        self.content.get(&id).cloned()
    }
}

/// `GET /api/v1/album/{hash}`.
pub async fn get_album(
    State(state): State<AppState>,
    Path(hash): Path<String>,
    Query(query): Query<AlbumQuery>,
) -> Result<Json<AlbumItem>, AppError> {
    let album = resolve_album(&state, &hash).await?;

    // A stale or never-indexed album starts a background scan and the current
    // (possibly empty) index is returned immediately.
    let sync = match indexer::start_if_stale(&state, &album) {
        SyncStart::Fresh => None,
        SyncStart::Started => Some("started".to_string()),
        SyncStart::Running => Some("running".to_string()),
    };

    let album_sort = parse_album_sort(query.asort.as_deref())?;
    let media_sort = parse_media_sort(query.msort.as_deref())?;
    let media_types = parse_media_types(query.mtype.as_deref())?;
    let alimits = parse_layer_limits(query.alimit.as_deref(), DEFAULT_ALIMITS)?;
    let mlimits = parse_layer_limits(query.mlimit.as_deref(), DEFAULT_MLIMITS)?;
    let mnests = parse_layer_bools(query.mnest.as_deref());
    let exp = link_expiry();

    let ancestors = if flag(query.ancests.as_deref()) {
        load_ancestors(&state, &album).await?
    } else {
        Vec::new()
    };
    let ancestor_ids: Vec<i64> = ancestors.iter().map(|ancestor| ancestor.id).collect();

    let config = AlbumBuildConfig {
        alimits: &alimits,
        mlimits: &mlimits,
        mnests: &mnests,
        media_sort: &media_sort,
        album_sort: &album_sort,
        media_types: &media_types,
        exp,
    };
    let tree = AlbumTree::load(
        &state,
        std::slice::from_ref(&album),
        album.id,
        &config,
        &ancestor_ids,
    )
    .await?;

    let mut item = tree.build_item(album.id);
    item.sync = sync;

    if !ancestors.is_empty() {
        item.ancestors = Some(
            ancestors
                .iter()
                .map(|ancestor| {
                    let mut item = AlbumItem::new(ancestor, tree.stats_for(ancestor.id));
                    item.content_sort = tree.content_for(ancestor.id);
                    item
                })
                .collect(),
        );
    }

    Ok(Json(item))
}

/// `GET /api/v1/album/{hash}/childs` — a page of direct children, each with
/// media previews and optionally grandchildren of its own.
pub async fn list_children(
    State(state): State<AppState>,
    Path(hash): Path<String>,
    Query(query): Query<AlbumQuery>,
) -> Result<Json<PaginateRes<AlbumItem>>, AppError> {
    let album = resolve_album(&state, &hash).await?;

    let album_sort = parse_album_sort(query.asort.as_deref())?;
    let media_sort = parse_media_sort(query.msort.as_deref())?;
    let media_types = parse_media_types(query.mtype.as_deref())?;
    let alimits = parse_layer_limits(query.alimit.as_deref(), DEFAULT_CHILD_ALIMITS)?;
    let mlimits = parse_layer_limits(query.mlimit.as_deref(), DEFAULT_CHILD_MLIMITS)?;
    let mnests = parse_layer_bools(query.mnest.as_deref());
    let limit = parse_limit(query.limit.as_deref(), Limit::All)?;
    let page = query.page.unwrap_or(1);

    if page == 0 {
        return Err(AppError::BadRequest("page must be >= 1".into()));
    }

    let (limit, offset) = match limit {
        Limit::All => (None, 0),
        Limit::Fixed(0) => {
            return Err(AppError::BadRequest("limit must be >= 1".into()));
        }
        Limit::Fixed(limit) => (Some(limit), i64::from(page - 1) * i64::from(limit)),
    };

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM albums WHERE parent_id = ?1")
        .bind(album.id)
        .fetch_one(&state.db)
        .await?;

    let children = load_children(&state, &album, &album_sort, limit, offset).await?;

    let config = AlbumBuildConfig {
        alimits: &alimits,
        mlimits: &mlimits,
        mnests: &mnests,
        media_sort: &media_sort,
        album_sort: &album_sort,
        media_types: &media_types,
        exp: link_expiry(),
    };
    let tree = AlbumTree::load(&state, &children, album.id, &config, &[]).await?;
    let items = children
        .iter()
        .map(|child| tree.build_item(child.id))
        .collect();

    Ok(Json(PaginateRes { total, items }))
}

fn is_disabled(limit: Limit) -> bool {
    matches!(limit, Limit::Fixed(0))
}

/// SQL limit value: `None` inside means unlimited.
fn limit_option(limit: Limit) -> Option<u32> {
    match limit {
        Limit::All => None,
        Limit::Fixed(limit) => Some(limit),
    }
}

async fn load_children(
    state: &AppState,
    album: &Album,
    sorts: &[AlbumSort],
    limit: Option<u32>,
    offset: i64,
) -> Result<Vec<Album>, AppError> {
    let mut builder = QueryBuilder::<Sqlite>::new("SELECT ");
    builder.push(ALBUM_FIELDS_QUALIFIED);
    builder.push(
        " FROM albums LEFT JOIN album_stats ON album_stats.album_id = albums.id \
         WHERE albums.parent_id = ",
    );
    builder.push_bind(album.id);
    push_album_order(&mut builder, sorts);

    match limit {
        Some(limit) => {
            builder.push(" LIMIT ").push_bind(i64::from(limit));
            if offset > 0 {
                builder.push(" OFFSET ").push_bind(offset);
            }
        }
        None if offset > 0 => {
            builder.push(" LIMIT -1 OFFSET ").push_bind(offset);
        }
        None => {}
    }

    Ok(builder.build_query_as().fetch_all(&state.db).await?)
}

/// Ancestors in root-first order, i.e. `[root, …, parent]`.
async fn load_ancestors(state: &AppState, album: &Album) -> Result<Vec<Album>, AppError> {
    let mut builder = QueryBuilder::<Sqlite>::new(
        "WITH RECURSIVE chain(id, depth) AS ( \
             SELECT parent_id, 0 FROM albums WHERE id = ",
    );
    builder.push_bind(album.id);
    builder.push(
        " AND parent_id IS NOT NULL \
         UNION ALL \
         SELECT albums.parent_id, chain.depth + 1 \
         FROM albums JOIN chain ON albums.id = chain.id \
         WHERE albums.parent_id IS NOT NULL AND chain.depth < 128\
         ) SELECT ",
    );
    builder.push(ALBUM_FIELDS_QUALIFIED);
    builder.push(" FROM chain JOIN albums ON albums.id = chain.id ORDER BY chain.depth DESC");

    Ok(builder.build_query_as().fetch_all(&state.db).await?)
}
