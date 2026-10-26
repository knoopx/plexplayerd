use crate::identity::Player;
use crate::models::{PlexContainer, PlexTrack};
use tracing::{info, warn};

#[derive(Clone)]
pub struct QueueTrack {
    pub key: String,
    pub play_queue_item_id: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    pub path: Option<String>,
    pub url: Option<String>,
    pub parent_title: String,
    pub thumb: String,
    pub container: String,
    pub codec: String,
    pub bitrate: u32,
    pub user_rating: f64,
    pub view_count: u32,
    pub rating_key: String,
}

impl QueueTrack {
    pub fn play_command(&self, offset_ms: u64, token: &str) -> crate::player::Command {
        crate::player::Command::PlayTrack {
            path: self.path.clone(),
            url: self.url.clone(),
            offset_ms,
            token: token.to_string(),
        }
    }

    pub fn label(&self) -> String {
        self.path
            .clone()
            .or_else(|| self.url.clone())
            .unwrap_or_default()
    }
}

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

pub fn body_prefix(body: &str) -> &str {
    let mut end = body.len().min(200);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

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

pub fn fetch_play_queue(
    base: &str,
    player: &Player,
    queue_id: &str,
    token: &str,
) -> Option<Vec<QueueTrack>> {
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
        warn!(
            "fetch_play_queue: queue {queue_id} has {total} track(s) but no playable part (local path or PMS stream URL); base {base}; HTTP {status}; body: {}",
            body_prefix(&xml)
        );
    }
    Some(tracks)
}
