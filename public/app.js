"use strict";

const MEDIA_LIMIT = 30;
const CHILD_LIMIT = 100;
const MEDIA_SORTS = new Set(["date", "-date", "name", "-name", "size", "-size"]);
const ALBUM_SORTS = new Set(["name", "-name", "created_at", "-created_at"]);

const els = {
  status: document.getElementById("status"),
  albumHead: document.getElementById("album-head"),
  albumTitle: document.getElementById("album-title"),
  albumMeta: document.getElementById("album-meta"),
  childrenSection: document.getElementById("children-section"),
  childrenList: document.getElementById("children-list"),
  childrenCount: document.getElementById("children-count"),
  mediaSection: document.getElementById("media-section"),
  mediaList: document.getElementById("media-list"),
  mediaCount: document.getElementById("media-count"),
  mediaEmpty: document.getElementById("media-empty"),
  loadMore: document.getElementById("load-more"),
  mediaSort: document.getElementById("media-sort"),
  albumSort: document.getElementById("album-sort"),
  indexButton: document.getElementById("index-button"),
  indexButtonLabel: document.getElementById("index-button-label"),
  nestedIndex: document.getElementById("nested-index"),
  indexProgress: document.getElementById("index-progress"),
  indexProgressFill: document.getElementById("index-progress-fill"),
  indexProgressText: document.getElementById("index-progress-text"),
  indexResult: document.getElementById("index-result"),
  indexResultText: document.getElementById("index-result-text"),
  indexRefresh: document.getElementById("index-refresh"),
  indexDismiss: document.getElementById("index-dismiss"),
};

const state = {
  hash: "root",
  sort: "date",
  asort: "name",
  page: 1,
  pages: 0,
  loading: false,
  album: null,
  requestId: 0,
  indexBusy: false,
  mediaTotal: 0,
  childrenTotal: 0,
  // `infoReady` is false while the album info and first pages load; a job can
  // finish during that window, so its `done` event is kept for later.
  infoReady: false,
  pendingDone: null,
  // Lists that were empty when indexing started are refreshed while the job
  // runs, so a folder indexed for the first time fills up live.
  liveMedia: false,
  liveChildren: false,
};

let eventSource = null;
let eventsReady = Promise.resolve();
let liveRefreshTimer = null;

function pathHash() {
  try {
    const parts = decodeURIComponent(window.location.pathname)
      .split("/")
      .filter(Boolean);
    return parts.length === 1 ? parts[0] : "root";
  } catch {
    return "root";
  }
}

function readQuery() {
  const params = new URLSearchParams(window.location.search);
  const sort = params.get("sort");
  const asort = params.get("asort");

  state.sort = MEDIA_SORTS.has(sort) ? sort : "date";
  state.asort = ALBUM_SORTS.has(asort) ? asort : "name";
}

function syncUrl() {
  const params = new URLSearchParams(window.location.search);
  params.set("sort", state.sort);
  params.set("asort", state.asort);

  const query = params.toString();
  const url = window.location.pathname + (query ? `?${query}` : "");
  window.history.replaceState(null, "", url);
}

function syncSortControls() {
  els.mediaSort.value = state.sort;
  els.albumSort.value = state.asort;
}

function setStatus(message, isError = false) {
  els.status.textContent = message ?? "";
  els.status.hidden = !message;
  els.status.classList.toggle("error", Boolean(isError));
}

const MEDIA_EMPTY_TEXT = "В альбоме пока нет медиа.";
const MEDIA_INDEXING_TEXT = "Идёт индексация — медиа появятся автоматически.";

function updateMediaEmptyHint() {
  els.mediaEmpty.textContent = state.liveMedia
    ? MEDIA_INDEXING_TEXT
    : MEDIA_EMPTY_TEXT;
}

function clearAlbum() {
  els.albumHead.hidden = true;
  els.childrenSection.hidden = true;
  els.childrenList.replaceChildren();
  els.childrenCount.textContent = "";
  els.mediaSection.hidden = true;
  els.mediaList.replaceChildren();
  els.mediaCount.textContent = "";
  els.mediaEmpty.hidden = true;
  els.loadMore.hidden = true;
  els.loadMore.disabled = false;
}

function setIndexBusy(busy) {
  state.indexBusy = busy;
  els.indexButton.disabled = busy;
  els.indexButton.classList.toggle("is-busy", busy);
  els.indexButtonLabel.textContent = busy ? "Индексация…" : "Индексировать";
}

function resetIndexUi() {
  setIndexBusy(false);
  els.indexProgress.hidden = true;
  els.indexProgressFill.style.width = "0%";
  els.indexProgressText.textContent = "";
  els.indexResult.hidden = true;
  els.indexResult.classList.remove("error");
  els.indexResultText.textContent = "";
}

function showIndexProgress(filesDone, filesTotal, dirsDone, dirsTotal) {
  const total = filesTotal + dirsTotal;
  const done = filesDone + dirsDone;
  const percent = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;

  els.indexProgress.hidden = false;
  els.indexProgressFill.style.width = `${percent}%`;

  if (total === 0) {
    els.indexProgressText.textContent = "Сканирование…";
  } else {
    els.indexProgressText.textContent =
      `${filesDone}/${filesTotal} файлов · ${dirsDone}/${dirsTotal} папок`;
  }
}

function showIndexResult(message, isError) {
  els.indexProgress.hidden = true;
  els.indexResult.hidden = false;
  els.indexResult.classList.toggle("error", Boolean(isError));
  els.indexResultText.textContent = message;
}

function stopLiveRefresh() {
  if (liveRefreshTimer) {
    clearTimeout(liveRefreshTimer);
    liveRefreshTimer = null;
  }
  state.liveMedia = false;
  state.liveChildren = false;
  updateMediaEmptyHint();
}

const LIVE_REFRESH_MS = 1000;

async function refreshLiveLists() {
  if (!state.liveMedia && !state.liveChildren) return;
  if (state.loading) return;

  const id = state.requestId;
  const jobs = [];
  if (state.liveChildren) jobs.push(loadChildren(id));
  if (state.liveMedia) jobs.push(loadMedia(1, id, false));
  await Promise.all(jobs);
}

// Shows the first batch immediately, then coalesces the frequent progress
// events so the grid is re-rendered at most once per second.
function scheduleLiveRefresh() {
  if ((!state.liveMedia && !state.liveChildren) || liveRefreshTimer) return;
  liveRefreshTimer = setTimeout(() => {
    liveRefreshTimer = null;
  }, LIVE_REFRESH_MS);
  refreshLiveLists();
}

// Final refresh after the job finished, including lists that were empty.
async function finishLiveRefresh() {
  const refreshMedia = state.liveMedia;
  const refreshChildren = state.liveChildren;
  stopLiveRefresh();
  if (!refreshMedia && !refreshChildren) return;

  // A progress refresh may still be in flight; wait for it so the last batch
  // is not lost when its (slightly older) response wins the race.
  const id = state.requestId;
  for (let i = 0; state.loading && i < 40; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }

  await Promise.all([
    refreshChildren ? loadChildren(id) : Promise.resolve(),
    refreshMedia ? loadMedia(1, id, false) : Promise.resolve(),
  ]);
}

function handleIndexEvent(data) {
  if (!data || data.root_hash !== state.hash) return;

  const filesDone = Number(data.files_done) || 0;
  const filesTotal = Number(data.files_total) || 0;
  const dirsDone = Number(data.dirs_done) || 0;
  const dirsTotal = Number(data.dirs_total) || 0;

  if (data.phase === "error") {
    stopLiveRefresh();
    setIndexBusy(false);
    setStatus("");
    showIndexResult(
      `Ошибка индексации: ${data.error || "неизвестная ошибка"}`,
      true,
    );
    return;
  }

  if (data.done || data.phase === "done") {
    if (!state.infoReady) {
      // Small folders finish in milliseconds, before the first page response
      // is rendered. Defer the result so the final refresh is not skipped.
      state.pendingDone = { data, id: state.requestId };
      return;
    }

    const live = state.liveMedia || state.liveChildren;
    setIndexBusy(false);
    setStatus("");
    if (live) {
      finishLiveRefresh();
    }
    showIndexResult(
      `Найдено ${Number(data.new_albums) || 0} новых альбомов и ` +
        `${Number(data.new_media) || 0} новых медиа.` +
        (live ? "" : " Обновить?"),
      false,
    );
    return;
  }

  // Only the indexing phase adds rows; scanning has nothing to show yet.
  if (data.phase === "indexing") {
    scheduleLiveRefresh();
  }

  setIndexBusy(true);
  setStatus("");
  showIndexProgress(filesDone, filesTotal, dirsDone, dirsTotal);
}

function openEvents() {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }

  if (typeof EventSource === "undefined") {
    eventsReady = Promise.resolve();
    return;
  }

  const hash = state.hash;
  const source = new EventSource(
    `/api/albums/${encodeURIComponent(hash)}/index/events`,
  );
  eventSource = source;

  source.onmessage = (event) => {
    let data;
    try {
      data = JSON.parse(event.data);
    } catch {
      return;
    }
    if (data.root_hash !== state.hash) return;
    handleIndexEvent(data);
  };

  eventsReady = new Promise((resolve) => {
    const timer = setTimeout(resolve, 1000);
    source.onopen = () => {
      clearTimeout(timer);
      resolve();
    };
  });
}

async function startIndex() {
  if (!state.album || state.indexBusy) return;

  els.indexResult.hidden = true;
  els.indexResult.classList.remove("error");
  setIndexBusy(true);
  showIndexProgress(0, 0, 0, 0);
  setStatus("");

  // Decide what to refresh before the job starts: a small folder can finish
  // before the POST response is parsed, making a late decision too late.
  const neverIndexed = Boolean(state.album && !state.album.indexed_at);
  state.liveMedia = neverIndexed || state.mediaTotal === 0;
  state.liveChildren = neverIndexed || state.childrenTotal === 0;
  updateMediaEmptyHint();

  try {
    const nested = els.nestedIndex.checked ? "1" : "0";
    await eventsReady;

    const response = await fetch(
      `${albumApi(state.hash)}/index?force=1&nested=${nested}`,
      { method: "POST", headers: { Accept: "application/json" } },
    );
    const body = await response.json().catch(() => ({}));

    if (!response.ok) {
      throw new Error(body.error || response.statusText || `HTTP ${response.status}`);
    }

    if (body.sync === "fresh") {
      setIndexBusy(false);
      els.indexProgress.hidden = true;
      // Nothing was started, so do not wait for events that will not come.
      stopLiveRefresh();
      setStatus("Индекс уже свежий.");
    }
  } catch (error) {
    stopLiveRefresh();
    setIndexBusy(false);
    els.indexProgress.hidden = true;
    setStatus(`Не удалось запустить индексацию: ${error.message}`, true);
  }
}


async function getJson(url) {
  const response = await fetch(url, { headers: { Accept: "application/json" } });
  if (!response.ok) {
    let message = response.statusText || `HTTP ${response.status}`;
    try {
      const body = await response.json();
      if (body && typeof body.error === "string") {
        message = body.error;
      }
    } catch {
      // not JSON
    }
    throw new Error(message);
  }
  return response.json();
}

function albumApi(hash) {
  return `/api/albums/${encodeURIComponent(hash)}`;
}

function childrenApi(hash, asort) {
  return `/api/albums/${encodeURIComponent(hash)}/child?page=1&limit=${CHILD_LIMIT}&sort=${encodeURIComponent(asort)}`;
}

function mediaApi(hash, sort, page) {
  return `${albumApi(hash)}/media?page=${page}&limit=${MEDIA_LIMIT}&sort=${encodeURIComponent(sort)}`;
}

function fileUrl(media) {
  const albumHash = state.album ? state.album.hash : state.hash;
  return `/files/${encodeURIComponent(albumHash)}/${encodeURIComponent(media.hash)}`;
}

function renderChildren(page) {
  els.childrenList.replaceChildren();
  state.childrenTotal = page.total;
  els.childrenSection.hidden = page.items.length === 0;
  els.childrenCount.textContent =
    page.total > page.items.length
      ? `${page.items.length} из ${page.total}`
      : String(page.total);

  for (const album of page.items) {
    const link = document.createElement("a");
    link.className = "album-card";
    link.href = album.hash === "root" ? "/" : `/${encodeURIComponent(album.hash)}`;
    link.dataset.album = album.hash;

    const glyph = document.createElement("span");
    glyph.className = "album-glyph";
    glyph.setAttribute("aria-hidden", "true");
    glyph.textContent = "▤";

    const name = document.createElement("span");
    name.className = "album-name";
    name.textContent = album.name;

    link.append(glyph, name);
    els.childrenList.append(link);
  }
}

function renderMedia(media) {
  const link = document.createElement("a");
  link.className = "media-item";
  link.href = fileUrl(media);
  link.target = "_blank";
  link.rel = "noopener";
  link.title = media.name;

  if (media.type === "image" || media.type === "animated") {
    if (Number(media.width) > 0 && Number(media.height) > 0) {
      link.style.aspectRatio = `${media.width} / ${media.height}`;
    }

    const image = document.createElement("img");
    image.loading = "lazy";
    image.decoding = "async";
    image.alt = media.name;
    image.src = link.href;
    link.append(image);
  } else {
    link.classList.add("media-file");

    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = media.type === "video" ? "видео" : "аудио";

    const name = document.createElement("span");
    name.className = "file-name";
    name.textContent = media.name;

    link.append(badge, name);
  }

  return link;
}

async function loadChildren(id) {
  try {
    const page = await getJson(childrenApi(state.hash, state.asort));
    if (id !== state.requestId) return;
    renderChildren(page);
  } catch (error) {
    if (id === state.requestId) {
      setStatus(`Не удалось загрузить вложенные альбомы: ${error.message}`, true);
    }
  }
}

async function loadMedia(pageNumber, id, append) {
  if (state.loading) return;

  state.loading = true;
  els.loadMore.disabled = true;

  try {
    const page = await getJson(mediaApi(state.hash, state.sort, pageNumber));
    if (id !== state.requestId) return;

    if (!append) {
      els.mediaList.replaceChildren();
    }

    for (const media of page.items) {
      els.mediaList.append(renderMedia(media));
    }

    state.page = page.page;
    state.pages = page.pages;
    state.mediaTotal = page.total;
    els.mediaCount.textContent = String(page.total);
    els.mediaEmpty.hidden = page.total !== 0;
    els.mediaSection.hidden = false;
    els.loadMore.hidden = page.page >= page.pages;
  } catch (error) {
    if (id === state.requestId) {
      setStatus(`Не удалось загрузить медиа: ${error.message}`, true);
    }
  } finally {
    state.loading = false;
    els.loadMore.disabled = false;
  }
}

async function loadAll() {
  const id = ++state.requestId;

  state.hash = pathHash();
  readQuery();
  syncSortControls();
  state.page = 1;
  state.pages = 0;
  state.album = null;
  state.loading = false;
  state.mediaTotal = 0;
  state.childrenTotal = 0;
  state.infoReady = false;
  state.pendingDone = null;
  stopLiveRefresh();

  clearAlbum();
  resetIndexUi();
  openEvents();
  setStatus("Загрузка…");

  const ready = eventsReady;
  await ready;
  if (id !== state.requestId) return;

  try {
    const info = await getJson(albumApi(state.hash));
    if (id !== state.requestId) return;

    state.album = info.album;

    const isRoot = state.hash === "root";
    els.albumTitle.textContent = isRoot ? "wepics" : info.album.name;
    els.albumMeta.textContent = isRoot
      ? "Все альбомы и медиа"
      : `/${info.album.hash}`;
    els.albumHead.hidden = false;
    document.title = isRoot ? "wepics" : `${info.album.name} — wepics`;

    if (info.sync) {
      if (!state.indexBusy) {
        setIndexBusy(true);
        showIndexProgress(0, 0, 0, 0);
        setStatus("Идёт синхронизация альбома…");
      }
    } else if (!state.indexBusy) {
      setStatus("");
    }

    await Promise.all([loadChildren(id), loadMedia(1, id, false)]);
    if (id !== state.requestId) return;

    if (info.sync) {
      // Albums that had nothing to show when the job started are refreshed
      // from the progress events, so their contents appear as they are
      // indexed instead of requiring a manual reload. `indexed_at` is checked
      // as well: a never-indexed album may already have its first rows by the
      // time this response arrives.
      const neverIndexed = !info.album.indexed_at;
      state.liveChildren = neverIndexed || state.childrenTotal === 0;
      state.liveMedia = neverIndexed || state.mediaTotal === 0;
      updateMediaEmptyHint();
    }

    state.infoReady = true;
    if (state.pendingDone && state.pendingDone.id === id) {
      // The job already finished while the page was loading: apply its result
      // now, with the live flags set, so the lists get their final refresh.
      const pending = state.pendingDone.data;
      state.pendingDone = null;
      handleIndexEvent(pending);
    }
  } catch (error) {
    if (id !== state.requestId) return;
    clearAlbum();
    setIndexBusy(false);
    if (eventSource) {
      eventSource.close();
      eventSource = null;
    }
    setStatus(
      error.message === "not found"
        ? "Альбом не найден."
        : `Не удалось открыть альбом: ${error.message}`,
      true,
    );
    document.title = "Альбом не найден — wepics";
  }
}

function navigate(albumHash) {
  const url = albumHash === "root" ? "/" : `/${encodeURIComponent(albumHash)}`;
  const query = window.location.search;
  window.history.pushState(null, "", url + query);
  loadAll();
}

document.addEventListener("click", (event) => {
  const link = event.target.closest("a[data-album]");
  if (!link) return;
  if (
    event.defaultPrevented ||
    event.button !== 0 ||
    event.metaKey ||
    event.ctrlKey ||
    event.shiftKey ||
    event.altKey
  ) {
    return;
  }

  event.preventDefault();
  navigate(link.dataset.album);
});

window.addEventListener("popstate", () => {
  loadAll();
});

els.mediaSort.addEventListener("change", () => {
  state.sort = els.mediaSort.value;
  syncUrl();
  loadAll();
});

els.albumSort.addEventListener("change", () => {
  state.asort = els.albumSort.value;
  syncUrl();
  loadAll();
});

els.loadMore.addEventListener("click", () => {
  loadMedia(state.page + 1, state.requestId, true);
});

els.indexButton.addEventListener("click", startIndex);

els.indexRefresh.addEventListener("click", () => {
  els.indexResult.hidden = true;
  loadAll();
});

els.indexDismiss.addEventListener("click", () => {
  els.indexResult.hidden = true;
});

try {
  els.nestedIndex.checked = localStorage.getItem("wepics:nested") === "1";
} catch {
  // localStorage can be unavailable in private mode
}

els.nestedIndex.addEventListener("change", () => {
  try {
    localStorage.setItem("wepics:nested", els.nestedIndex.checked ? "1" : "0");
  } catch {
    // ignore
  }
});

loadAll();
