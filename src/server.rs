use std::sync::{Arc, Mutex};

use crate::identity::{HTTP_PORT, Player};
use crate::playback::Playback;
use crate::pms::{self, QueueTrack};
use tracing::{info, warn};

pub fn resources_server(player: Player, playback: Arc<Mutex<Playback>>) {
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http(("0.0.0.0", HTTP_PORT)) {
            Ok(s) => s,
            Err(err) => {
                warn!("HTTP: failed to bind {HTTP_PORT}: {err}");
                return;
            }
        };
        info!("HTTP listening on 0.0.0.0:{HTTP_PORT} (resources + command + timeline)");
        for mut request in server.incoming_requests() {
            let url = request.url().to_string();
            let (status, body, ctype) = route(&url, &player, &playback, &mut request);
            let path: String = url.chars().take_while(|c| *c != '?').collect();
            if path != "/player/timeline/poll" {
                info!("HTTP: {url} -> {status}");
            }
            let mut response = tiny_http::Response::from_string(body)
                .with_status_code(tiny_http::StatusCode(status));
            for (k, v) in ctype {
                let header = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap();
                response = response.with_header(header);
            }
            let _ = request.respond(response);
        }
    });
}

fn route(
    url: &str,
    player: &Player,
    playback: &Arc<Mutex<Playback>>,
    request: &mut tiny_http::Request,
) -> (u16, String, Vec<(String, String)>) {
    let path: String = url.chars().take_while(|c| *c != '?').collect();
    let query = url
        .split_once('?')
        .map(|(_, q)| q.to_string())
        .unwrap_or_default();
    match path.as_str() {
        "/resources" => {
            let target_id = request
                .headers()
                .iter()
                .find(|h| h.field.as_str() == "X-Plex-Target-Client-Identifier")
                .map(|h| h.value.as_str().to_string())
                .unwrap_or_default();
            info!(
                "HTTP /resources target-client-id={target_id} (my id={})",
                player.device_id
            );
            (
                200,
                player.resources_xml(),
                vec![("Content-Type".into(), "text/xml; charset=utf-8".into())],
            )
        }
        "/player/playback/playMedia" => {
            let status = handle_play_media(&query, player, playback);
            (status, String::new(), vec![])
        }
        "/player/playback/createPlayQueue" => {
            let (status, body) = handle_create_play_queue(&query, player, playback);
            (
                status,
                body,
                vec![("Content-Type".into(), "text/xml; charset=utf-8".into())],
            )
        }
        "/player/playback/stop" | "/player/command" => {
            playback.lock().unwrap().stop();
            (200, String::new(), vec![])
        }
        "/player/playback/pause" => {
            playback.lock().unwrap().pause();
            (200, String::new(), vec![])
        }
        "/player/playback/play" => {
            playback.lock().unwrap().resume();
            (200, String::new(), vec![])
        }
        "/player/playback/playPause" => {
            playback.lock().unwrap().play_pause();
            (200, String::new(), vec![])
        }
        "/player/playback/skipNext" => {
            playback.lock().unwrap().next_track();
            (200, String::new(), vec![])
        }
        "/player/playback/skipPrevious" => {
            let params = parse_query(&query);
            let force = params.get("force").map(|v| v == "1").unwrap_or(false);
            playback.lock().unwrap().prev_track(force);
            (200, String::new(), vec![])
        }
        "/player/playback/skipTo" => {
            let params = parse_query(&query);
            let key = params.get("key").cloned().unwrap_or_default();

            let target = if key.is_empty() {
                params.get("playQueueItemID").cloned().unwrap_or_default()
            } else {
                key
            };
            if !target.is_empty() {
                playback.lock().unwrap().skip_to_key(&target);
            }
            (200, String::new(), vec![])
        }
        "/player/playback/skipToNextAlbum" => {
            playback.lock().unwrap().skip_to_next_album();
            (200, String::new(), vec![])
        }
        "/player/playback/refreshPlayQueue" => {
            let params = parse_query(&query);
            if let Some(id) = params.get("playQueueID").cloned() {
                if !id.is_empty() {
                    playback.lock().unwrap().set_play_queue_id(&id);
                }
            }
            playback.lock().unwrap().refresh_queue(player);
            (200, String::new(), vec![])
        }
        "/player/playback/seekTo" => {
            let params = parse_query(&query);
            let offset_ms: u64 = params
                .get("offset")
                .cloned()
                .unwrap_or_default()
                .parse()
                .unwrap_or(0);
            playback.lock().unwrap().seek(offset_ms);
            (200, String::new(), vec![])
        }
        "/player/playback/setParameters" => {
            let params = parse_query(&query);
            let mut pb = playback.lock().unwrap();
            if let Some(vol) = params.get("volume") {
                if let Ok(v) = vol.parse::<i64>() {
                    pb.set_volume(v);
                }
            }
            if let Some(rep) = params.get("repeat") {
                if let Ok(v) = rep.parse::<u32>() {
                    pb.repeat = v;
                }
            }
            if let Some(sh) = params.get("shuffle") {
                if let Ok(v) = sh.parse::<u32>() {
                    pb.shuffle = v;
                }
            }
            (200, String::new(), vec![])
        }
        "/player/timeline/poll" => {
            let params = parse_query(&query);

            let Some(command_id) = params.get("commandID").cloned() else {
                return (400, String::new(), vec![]);
            };
            (
                200,
                playback.lock().unwrap().timeline_xml(player, &command_id),
                vec![("Content-Type".into(), "text/xml".into())],
            )
        }

        "/player/timeline/subscribe" | "/player/timeline/unsubscribe" => {
            (200, String::new(), vec![])
        }
        _ => (404, String::from("not found"), vec![]),
    }
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    use url::form_urlencoded;
    form_urlencoded::parse(query.as_ref())
        .into_owned()
        .collect()
}

fn handle_play_media(query: &str, player: &Player, playback: &Arc<Mutex<Playback>>) -> u16 {
    let params = parse_query(query);
    let get = |k: &str| params.get(k).cloned().unwrap_or_default();
    let key = get("key");
    let token = get("token");
    let offset_ms: u64 = get("offset").parse().unwrap_or(0);
    let paused = get("paused") == "1";
    let container_key = get("containerKey");
    let play_queue_id = container_key
        .split("/playQueues/")
        .nth(1)
        .map(|s| s.split('?').next().unwrap_or("").to_string())
        .unwrap_or_default();
    let machine_id = get("machineIdentifier");

    let protocol = get("protocol");
    let address = get("address");
    let port = get("port");
    let mut base = if address.is_empty() {
        std::env::var("PLEX_SERVER").unwrap_or_else(|_| "http://127.0.0.1:32400".to_string())
    } else {
        let proto = if protocol.is_empty() {
            "http"
        } else {
            protocol.as_str()
        };

        if port.is_empty() {
            format!("{proto}://{address}:32400")
        } else {
            format!("{proto}://{address}:{port}")
        }
    };

    if address.ends_with(".plex.direct") {
        let first = address.split('.').next().unwrap_or("");
        let is_dash_ipv4 =
            !first.is_empty() && first.chars().all(|c| c.is_ascii_digit() || c == '-');
        if is_dash_ipv4 {
            let dotted = first.replace('-', ".");
            let p = if port.is_empty() {
                "32400"
            } else {
                port.as_str()
            };
            base = format!("http://{dotted}:{p}");
            info!("playMedia: plex.direct {address} -> base {base}");
        }
    };
    info!(
        "playMedia: key={key} queue={play_queue_id} offset={offset_ms}ms source={machine_id} base={base}"
    );
    let mut playlist: Vec<QueueTrack> = if !play_queue_id.is_empty() {
        pms::fetch_play_queue(&base, player, &play_queue_id, &token).unwrap_or_default()
    } else {
        Vec::new()
    };

    let start_key = if key.is_empty() {
        playlist.first().map(|t| t.key.clone()).unwrap_or_default()
    } else {
        if !playlist.iter().any(|t| t.key == key) {
            if let Some(t) = pms::resolve_media_file(&base, player, &key, &token) {
                playlist.insert(0, t);
            }
        }
        key.clone()
    };
    if start_key.is_empty() {
        warn!(
            "playMedia: no key and no playable queue item (queue={play_queue_id}, server base {base})"
        );
        return 400;
    }
    let current_title = playlist
        .iter()
        .find(|t| t.key == start_key)
        .map(|t| t.title.as_str())
        .unwrap_or("?");
    info!(
        "playMedia: playing '{current_title}' (playlist of {} tracks)",
        playlist.len()
    );
    let mut pb = playback.lock().unwrap();
    match pb.start(
        playlist,
        &start_key,
        offset_ms,
        &play_queue_id,
        &machine_id,
        &token,
        paused,
        &base,
        &player.device_id,
        &player.product,
        &player.version,
    ) {
        Ok(()) => 200,
        Err(err) => {
            warn!("playMedia: failed to start: {err}");
            500
        }
    }
}

fn handle_create_play_queue(
    query: &str,
    player: &Player,
    playback: &Arc<Mutex<Playback>>,
) -> (u16, String) {
    let params = parse_query(query);
    let get = |k: &str| params.get(k).cloned().unwrap_or_default();
    let token = get("token");
    let machine_id = get("machineIdentifier");
    let protocol = get("protocol");
    let address = get("address");
    let port = get("port");
    let mut base = if address.is_empty() {
        std::env::var("PLEX_SERVER").unwrap_or_else(|_| "http://127.0.0.1:32400".to_string())
    } else {
        let proto = if protocol.is_empty() {
            "http"
        } else {
            protocol.as_str()
        };
        if port.is_empty() {
            format!("{proto}://{address}:32400")
        } else {
            format!("{proto}://{address}:{port}")
        }
    };
    if address.ends_with(".plex.direct") {
        let first = address.split('.').next().unwrap_or("");
        if !first.is_empty() && first.chars().all(|c| c.is_ascii_digit() || c == '-') {
            let dotted = first.replace('-', ".");
            let p = if port.is_empty() {
                "32400"
            } else {
                port.as_str()
            };
            base = format!("http://{dotted}:{p}");
        }
    };
    info!("createPlayQueue: source={machine_id} base={base}");

    let url = format!("{base}/playQueues?{query}&X-Plex-Token={token}");
    let Some((status, body)) = pms::http_post(&url, player) else {
        warn!("createPlayQueue: POST to PMS failed for {url}");
        return (502, String::new());
    };
    if !(200..300).contains(&status) {
        warn!(
            "createPlayQueue: PMS returned HTTP {status}: {}",
            pms::body_prefix(&body)
        );
        return (status, body);
    }
    let play_queue_id = pms::extract_play_queue_id(&body);
    if play_queue_id.is_empty() {
        warn!(
            "createPlayQueue: PMS returned no playQueueID (HTTP {status}): {}",
            pms::body_prefix(&body)
        );
        return (status, body);
    }
    info!("createPlayQueue: created playQueueID={play_queue_id}");

    let playlist = pms::fetch_play_queue(&base, player, &play_queue_id, &token).unwrap_or_default();
    if let Some(first) = playlist.first() {
        let first_key = first.key.clone();
        let current_title = first.title.clone();
        info!("createPlayQueue: playing '{current_title}'");
        let mut pb = playback.lock().unwrap();
        let _ = pb.start(
            playlist,
            &first_key,
            0,
            &play_queue_id,
            &machine_id,
            &token,
            false,
            &base,
            &player.device_id,
            &player.product,
            &player.version,
        );
    }
    (200, body)
}
