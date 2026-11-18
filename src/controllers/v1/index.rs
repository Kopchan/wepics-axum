//! `GET|POST /api/v1/album/{hash}/index` and its SSE progress stream.

use std::convert::Infallible;
use std::pin::Pin;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::error::AppError;
use crate::helpers::flags::flag;
use crate::hubs::indexer::IndexEvent;
use crate::indexer::{self, IndexOptions};
use crate::resolvers::resolve_album;
use crate::state::AppState;

/// `GET /api/v1/album/{hash}/index?force&nested`
///
/// Bare `?force`/`?nested` mean `true`; `0`, `false`, `no`, `off` mean false.
#[derive(Debug, Deserialize)]
pub struct IndexQuery {
    pub force: Option<String>,
    pub nested: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IndexResponse {
    /// `started`, `running` or `fresh`.
    pub sync: String,
}

/// Starts a background index job and reports whether this request spawned it.
pub async fn start_index(
    State(state): State<AppState>,
    Path(hash): Path<String>,
    Query(query): Query<IndexQuery>,
) -> Result<Json<IndexResponse>, AppError> {
    let album = resolve_album(&state, &hash).await?;
    let options = IndexOptions {
        force: flag(query.force.as_deref()),
        nested: flag(query.nested.as_deref()),
    };

    let status = indexer::start(&state, &album, options);
    Ok(Json(IndexResponse {
        sync: status.as_str().to_string(),
    }))
}

/// `GET /api/v1/album/{hash}/index/events` — SSE progress for this album's jobs.
type EventStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;
type SseStream = axum::response::sse::KeepAliveStream<EventStream>;

pub async fn index_events(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Sse<SseStream>, AppError> {
    let album = resolve_album(&state, &hash).await?;

    let hub = state.index_hub();
    let receiver = hub.subscribe();
    // Jobs are keyed by canonical hash, so an alias link must be resolved
    // first; otherwise the snapshot and the filter would both come up empty.
    let snapshot = hub.latest(&album.hash);
    let filter_hash = album.hash.clone();

    let initial = tokio_stream::iter(snapshot.into_iter().map(event_to_sse));
    let live = BroadcastStream::new(receiver).filter_map(move |message| {
        let event = message.ok()?;
        if event.root_hash != filter_hash {
            return None;
        }
        Some(event_to_sse(event))
    });

    let stream: EventStream = Box::pin(initial.chain(live));
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// `GET /api/v1/index/events` — SSE progress for all running index jobs.
///
/// One subscription per client covers navigation between albums and jobs that
/// were started elsewhere (another tab, a lazy sync, a nested parent job).
pub async fn all_index_events(State(state): State<AppState>) -> Sse<SseStream> {
    let hub = state.index_hub();
    let receiver = hub.subscribe();
    let snapshots = hub.latest_all();

    let initial = tokio_stream::iter(snapshots.into_iter().map(event_to_sse));
    let live = BroadcastStream::new(receiver).filter_map(|message| message.ok().map(event_to_sse));

    let stream: EventStream = Box::pin(initial.chain(live));
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn event_to_sse(event: IndexEvent) -> Result<Event, Infallible> {
    Ok(Event::default()
        .json_data(event)
        .unwrap_or_else(|_| Event::default().comment("index event serialization failed")))
}
