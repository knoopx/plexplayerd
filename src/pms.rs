use crate::identity::Player;
use crate::models::{PlexContainer, PlexTrack};
use tracing::{info, warn};

#[derive(Clone)]
pub struct QueueTrack {
    pub key: String,
    /// The play-queue item id (`Track@playQueueItemID`); Plexamp links the
    /// timeline to the play-queue item (and its full metadata) by this id.
    pub play_queue_item_id: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    /// Local media file on the player machine (when the part is local).
    pub path: Option<String>,
    /// PMS part stream endpoint (when the part is on a remote PMS).
    pub url: Option<String>,
    /// The album name (`Track@parentTitle`).
    pub parent_title: String,
    /// The track's artwork (`Track@thumb`), resolved against the server base.
    pub thumb: String,
    /// Audio container format (e.g. "mp3", "flac") from the part.
    pub container: String,
    /// Audio codec (e.g. "mp3", "flac", "aac") from the part.
    pub codec: String,
    /// Nominal bitrate in bps from the part (0 when unknown).
    pub bitrate: u32,
    /// The user's 0-10 rating (0 = unrated).
    pub user_rating: f64,
    /// Number of times the track has been played.
    pub view_count: u32,
    /// The track's rating key (`Track@ratingKey`), used to match play-queue items.
    pub rating_key: String,
}

impl QueueTrack {
    /// Build the engine command for this track (exactly one of `path`/`url`
    /// is Some). The token is carried so the engine can authenticate a remote
    /// part stream (ignored for local files).
    pub fn play_command(&self, offset_ms: u64, token: &str) -> crate::player::Command {
        crate::player::Command::PlayTrack {
            path: self.path.clone(),
            url: self.url.clone(),
            offset_ms,
            token: token.to_string(),
        }
    }

    /// Display label for the timeline's `Part@key`: the local path, or the
    /// stream URL when the track is served from the PMS.
    pub fn label(&self) -> String {
        self.path
            .clone()
            .or_else(|| self.url.clone())
            .unwrap_or_default()
    }
}

/// Fetch `url` with curl, adding the reference player identity headers.
///
/// Grounded in module-4030 (ProviderController.loadJson): server requests go
/// to the device's connection URI with `player.headers()` —
/// X-Plex-Client-Identifier / X-Plex-Product / X-Plex-Version (see the
/// `headers()` in module-43535) — plus `X-Plex-Token`. The token is kept in
/// the URL query string (existing behaviour); the three identity headers are
/// added as `-H` args.
///
/// The PMS HTTP status is captured with `-w '\n%{http_code}'` (curl writes
/// body + newline + 3-digit code, split on the last newline) and returned to
/// the caller alongside the body. The reference's HTTP layer inspects
/// statusCode the same way (module-4030 logs
/// `Error loading: ${verb} ${device.url()}${path}: ${message}` on failure),
/// so callers surface the status + body prefix instead of a bare "empty".
/// Non-2xx responses (token rejection: 401/403) are returned, not
/// swallowed, so callers can tell "PMS said 401/403" from "PMS said 200
/// with an empty queue". Returns None only on a curl failure.
fn http_get(url: &str, player: &Player) -> Option<(u16, String)> {
    info!("fetch: GET {url}");
    let resp = ureq::Agent::new()
        .get(url)
        .set("X-Plex-Client-Identifier", &player.device_id)
        .set("X-Plex-Product", &player.product)
        .set("X-Plex-Version", &player.version)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .ok()?;
    let status = resp.status();
    let body = resp.into_string().ok()?;
    if body.trim().is_empty() {
        warn!("fetch: empty response (HTTP {status}) from {url}");
    }
    Some((status, body))
}

/// POST a URL with curl, adding the reference player identity headers. Same
/// status-capture behaviour as `http_get` (`-w '\n%{http_code}'`).
pub fn http_post(url: &str, player: &Player) -> Option<(u16, String)> {
    info!("fetch: POST {url}");
    let resp = ureq::Agent::new()
        .post(url)
        .set("X-Plex-Client-Identifier", &player.device_id)
        .set("X-Plex-Product", &player.product)
        .set("X-Plex-Version", &player.version)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .ok()?;
    let status = resp.status();
    let body = resp.into_string().ok()?;
    if body.trim().is_empty() {
        warn!("fetch: empty response (HTTP {status}) from {url}");
    }
    Some((status, body))
}

/// Extract the `playQueueID` attribute from a `<MediaContainer ...>` PMS
/// response (the `computePlayKey` result).
pub fn extract_play_queue_id(xml: &str) -> String {
    let start = match xml.find("<MediaContainer") {
        Some(s) => s,
        None => return String::new(),
    };
    let tag_end = match xml[start..].find('>') {
        Some(e) => start + e,
        None => return String::new(),
    };
    let tag = &xml[start..=tag_end];
    let needle = "playQueueID=\"";
    let id_start = match tag.find(needle) {
        Some(k) => k + needle.len(),
        None => return String::new(),
    };
    let id_end = match tag[id_start..].find('"') {
        Some(e) => id_start + e,
        None => return String::new(),
    };
    tag[id_start..id_end].to_string()
}

/// First ~200 chars of a response body, for log lines. PMS bodies can be
/// megabytes, so a diagnostic carries only a prefix (module-4030 logs a
/// message, not the payload). Char-boundary safe.
pub fn body_prefix(body: &str) -> &str {
    let mut end = body.len().min(200);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

/// PMS response diagnostic, mirroring module-4030's
/// `Error loading: ${verb} ${device.url()}${path}: ${message}`: surfaces the
/// HTTP status + a body prefix so a token rejection (401/403) is
/// distinguishable from a genuinely empty queue (200, size=0).
fn log_fetch_response(url: &str, status: u16, body: &str) {
    warn!(
        "fetch: GET {url} -> HTTP {status}; body: {}",
        body_prefix(body)
    );
}

pub fn resolve_media_file(
    base: &str,
    player: &Player,
    key: &str,
    token: &str,
) -> Option<QueueTrack> {
    let path = if key.starts_with('/') {
        key.to_string()
    } else {
        format!("/{key}")
    };
    let url = format!("{base}{path}?X-Plex-Token={token}");
    let (status, xml) = http_get(&url, player)?;
    if !(200..300).contains(&status) || xml.trim().is_empty() {
        log_fetch_response(&url, status, &xml);
    }
    let Ok(container) = serde_xml_rs::from_str::<PlexContainer>(&xml) else {
        log_fetch_response(&url, status, &xml);
        warn!("resolve_media_file: XML parse failure for {url} (HTTP {status})");
        return None;
    };
    // Flat <Track> first (current PMS shape), then the legacy <Meta> wrapper.
    let Some(track) = container.track.first().or_else(|| {
        container.meta.iter().find_map(|m| {
            m.track
                .first()
                .or_else(|| m.album.iter().find_map(|a| a.track.first()))
        })
    }) else {
        warn!(
            "resolve_media_file: no <Track> in response for {url} (HTTP {status}); body: {}",
            body_prefix(&xml)
        );
        return None;
    };
    let Some((path, url)) = track.resolve(base) else {
        // Remote-PMS case: the track exists but has neither a local part
        // nor a part key to stream from.
        warn!(
            "resolve_media_file: track '{}' has no local part and no part key (remote PMS?) for {url} (HTTP {status})",
            track.title
        );
        return None;
    };
    let (container, codec, bitrate) = track.media_info();
    Some(QueueTrack {
        key: key.to_string(),
        play_queue_item_id: track.play_queue_item_id.clone(),
        title: track.title.clone(),
        artist: track.grandparent_title.clone(),
        parent_title: track.parent_title.clone(),
        duration_ms: track.duration,
        path,
        url,
        thumb: track.thumb.clone(),
        container,
        codec,
        bitrate,
        user_rating: track.user_rating,
        view_count: track.view_count,
        rating_key: track.rating_key.clone(),
    })
}

/// Fetch a play queue from the source device (module-97765 PlayQueue:
/// `/playQueues/<id>?window=...`). Returns the parsed tracks (possibly
/// empty); None on a fetch or parse failure.
pub fn fetch_play_queue(
    base: &str,
    player: &Player,
    queue_id: &str,
    token: &str,
) -> Option<Vec<QueueTrack>> {
    // module-97765 documented full-queue fetch: `window=9999` returns the
    // whole queue (not a window around the selected item) and
    // `includeLoudnessRamps=1` keeps the loudness-ramp parts.
    let url = format!(
        "{base}/playQueues/{queue_id}?window=9999&includeLoudnessRamps=1&X-Plex-Token={token}"
    );
    let (status, xml) = http_get(&url, player)?;
    if !(200..300).contains(&status) || xml.trim().is_empty() {
        log_fetch_response(&url, status, &xml);
    }
    let Ok(container) = serde_xml_rs::from_str::<PlexContainer>(&xml) else {
        log_fetch_response(&url, status, &xml);
        warn!("fetch_play_queue: XML parse failure for {url} (HTTP {status})");
        return None;
    };
    // Flat <Track> elements first (current PMS shape), then the legacy
    // <QueueItem> wrapper for older servers.
    let mut all: Vec<&PlexTrack> = container.track.iter().collect();
    for item in &container.queue_items {
        all.extend(item.track.iter());
    }
    let mut tracks = Vec::new();
    let mut total = 0usize;
    for track in all {
        total += 1;
        if track.key.is_empty() {
            continue;
        }
        let Some((path, url)) = track.resolve(base) else {
            continue;
        };
        let (container, codec, bitrate) = track.media_info();
        tracks.push(QueueTrack {
            key: track.key.clone(),
            play_queue_item_id: track.play_queue_item_id.clone(),
            title: track.title.clone(),
            artist: track.grandparent_title.clone(),
            parent_title: track.parent_title.clone(),
            duration_ms: track.duration,
            path,
            url,
            thumb: track.thumb.clone(),
            container,
            codec,
            bitrate,
            user_rating: track.user_rating,
            view_count: track.view_count,
            rating_key: track.rating_key.clone(),
        });
    }
    if tracks.is_empty() {
        // Remote-PMS case: the queue exists but none of its tracks carries a
        // playable part (a local path or a PMS stream URL), so nothing can
        // play. The HTTP status + body prefix distinguishes "PMS said
        // 401/403" (token rejection) from "PMS said 200 with an empty
        // queue" — module-4030's HTTP layer inspects statusCode the same
        // way.
        warn!(
            "fetch_play_queue: queue {queue_id} has {total} track(s) but no playable part (local path or PMS stream URL); base {base}; HTTP {status}; body: {}",
            body_prefix(&xml)
        );
    }
    Some(tracks)
}
