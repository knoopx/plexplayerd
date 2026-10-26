use crate::identity::Player;
use crate::models::{MediaXml, PartXml, TimelineContainer, TimelineXml, TrackXml};
use crate::pms::{QueueTrack, fetch_play_queue};
use anyhow::Result;
use tracing::{debug, info, warn};

fn alsa_device() -> String {
    std::env::var("PLEX_ALSA_DEVICE").unwrap_or_else(|_| "default".into())
}

pub struct Playback {
    pub state: String,
    pub title: String,
    pub artist: String,
    pub track_key: String,
    pub media_path: String,
    pub playlist: Vec<QueueTrack>,
    pub play_queue_id: String,
    pub machine_identifier: String,
    pub token: String,
    pub server_base: String,
    pub shuffle: u32,
    pub repeat: u32,
    pub volume: i64,
    pub rng: u64,
    pub gain: crate::player::SharedGain,
    pub player: Option<crate::player::Player>,
}

impl Default for Playback {
    fn default() -> Self {
        Self::new()
    }
}

impl Playback {
    pub fn new() -> Self {
        Self {
            state: "stopped".into(),
            title: String::new(),
            artist: String::new(),
            track_key: String::new(),
            media_path: String::new(),
            playlist: Vec::new(),
            play_queue_id: String::new(),
            machine_identifier: String::new(),
            token: String::new(),
            server_base: String::new(),
            shuffle: 0,
            repeat: 0,
            volume: 100,
            rng: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            gain: crate::player::SharedGain::new(),
            player: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        playlist: Vec<QueueTrack>,
        track_key: &str,
        offset_ms: u64,
        play_queue_id: &str,
        machine_id: &str,
        token: &str,
        paused: bool,
        server_base: &str,
        client_id: &str,
        product: &str,
        version: &str,
    ) -> Result<()> {
        self.player = None;
        let player = crate::player::Player::spawn(
            &alsa_device(),
            self.gain.clone(),
            crate::player::StreamIdentity {
                client_id: client_id.to_string(),
                product: product.to_string(),
                version: version.to_string(),
            },
        );
        let start_index = playlist
            .iter()
            .position(|t| t.key == track_key)
            .unwrap_or(0);
        let ordered: Vec<QueueTrack> = playlist[start_index..]
            .iter()
            .chain(playlist[..start_index].iter())
            .cloned()
            .collect();
        if ordered.is_empty() {
            anyhow::bail!("playMedia: empty playlist");
        }
        let first = &ordered[0];
        player.send(first.play_command(offset_ms, token));
        if paused {
            player.send(crate::player::Command::Pause);
        }
        self.player = Some(player);
        self.state = if paused { "paused" } else { "playing" }.into();
        self.track_key = first.key.clone();
        self.title = first.title.clone();
        self.artist = first.artist.clone();
        self.media_path = first.label();
        self.playlist = ordered;
        self.play_queue_id = play_queue_id.to_string();
        self.machine_identifier = machine_id.to_string();
        self.token = token.to_string();
        self.server_base = server_base.to_string();
        Ok(())
    }

    pub fn stop(&mut self) {
        self.player = None;
        self.track_key.clear();
        self.media_path.clear();
        self.playlist.clear();
        self.state = "stopped".into();
    }

    pub fn set_play_queue_id(&mut self, id: &str) {
        self.play_queue_id = id.to_string();
    }

    pub fn refresh_queue(&mut self, player: &Player) {
        if self.play_queue_id.is_empty() || self.token.is_empty() {
            return;
        }
        let base = if self.server_base.is_empty() {
            std::env::var("PLEX_SERVER").unwrap_or_else(|_| "http://127.0.0.1:32400".to_string())
        } else {
            self.server_base.clone()
        };
        let Some(tracks) = fetch_play_queue(&base, player, &self.play_queue_id, &self.token) else {
            warn!(
                "refreshPlayQueue: fetch of {} from {base} failed, keeping old playlist",
                self.play_queue_id
            );
            return;
        };
        if tracks.is_empty() {
            return;
        }
        let current = self.track_key.clone();
        let start_index = tracks.iter().position(|t| t.key == current).unwrap_or(0);
        let ordered: Vec<QueueTrack> = tracks[start_index..]
            .iter()
            .chain(tracks[..start_index].iter())
            .cloned()
            .collect();
        info!(
            "refreshPlayQueue: playlist updated ({} tracks)",
            ordered.len()
        );
        self.playlist = ordered;
    }

    pub fn skip_to_next_album(&mut self) {
        if self.playlist.len() < 2 {
            return;
        }
        let cur = self
            .playlist
            .iter()
            .position(|t| t.key == self.track_key)
            .unwrap_or(0);
        let len = self.playlist.len();
        for step in 1..len {
            let idx = (cur + step) % len;
            if self.playlist[idx].artist != self.playlist[cur].artist {
                self.play_index(idx);
                return;
            }
        }
    }

    pub fn pause(&mut self) {
        self.state = "paused".into();
        if let Some(p) = &self.player {
            p.send(crate::player::Command::Pause);
        }
    }

    pub fn resume(&mut self) {
        self.state = "playing".into();
        if let Some(p) = &self.player {
            p.send(crate::player::Command::Resume);
        }
    }

    pub fn play_pause(&mut self) {
        let paused = self.state == "paused";
        self.state = if paused {
            "playing".into()
        } else {
            "paused".into()
        };
        if let Some(p) = &self.player {
            p.send(if paused {
                crate::player::Command::Resume
            } else {
                crate::player::Command::Pause
            });
        }
    }

    pub fn next_track(&mut self) {
        self.advance_manual(1);
    }

    pub fn prev_track(&mut self, force: bool) {
        if force {
            let Some(t) = self.playlist.iter().find(|t| t.key == self.track_key) else {
                return;
            };
            if let Some(p) = &self.player {
                p.send(t.play_command(0, &self.token));
            }
            self.state = "playing".into();
            return;
        }
        self.advance_manual(-1);
    }

    pub fn skip_to_key(&mut self, key: &str) {
        if self.playlist.iter().any(|t| t.key == key) {
            let idx = self.playlist.iter().position(|t| t.key == key).unwrap();
            self.play_index(idx);
        }
    }

    pub fn advance_manual(&mut self, dir: i32) {
        if self.playlist.is_empty() {
            return;
        }
        let cur = self
            .playlist
            .iter()
            .position(|t| t.key == self.track_key)
            .unwrap_or(0);
        let len = self.playlist.len() as i32;
        let next = (cur as i32 + dir).rem_euclid(len) as usize;
        self.play_index(next);
    }

    pub fn complete_current(&mut self) {
        if self.repeat == 2 {
            let Some(t) = self.playlist.iter().find(|t| t.key == self.track_key) else {
                return;
            };
            if let Some(p) = &self.player {
                p.send(t.play_command(0, &self.token));
            }
            self.state = "playing".into();
            return;
        }
        if self.playlist.is_empty() {
            return;
        }
        let cur = self
            .playlist
            .iter()
            .position(|t| t.key == self.track_key)
            .unwrap_or(0);
        let len = self.playlist.len();
        if self.shuffle == 1 {
            let mut x = self.rng;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if x == 0 {
                x = 0x9e3779b97f4a7c15;
            }
            self.rng = x;
            let next = if len > 1 {
                cur + 1 + (x % (len - 1) as u64) as usize
            } else {
                cur
            };
            self.play_index(next);
            return;
        }
        let mut next = cur + 1;
        if next >= len {
            if self.repeat != 1 {
                self.player = None;
                self.track_key.clear();
                self.media_path.clear();
                self.state = "stopped".into();
                return;
            }
            next = 0;
        }
        self.play_index(next);
    }

    pub fn play_index(&mut self, idx: usize) {
        let Some(t) = self.playlist.get(idx) else {
            return;
        };
        if let Some(p) = &self.player {
            p.send(t.play_command(0, &self.token));
        }
        self.track_key = t.key.clone();
        self.title = t.title.clone();
        self.artist = t.artist.clone();
        self.media_path = t.label();
        self.state = "playing".into();
    }

    pub fn seek(&mut self, offset_ms: u64) {
        if let Some(p) = &self.player {
            p.send(crate::player::Command::Seek { offset_ms });
        }
    }

    pub fn set_volume(&mut self, volume: i64) {
        let vol = volume.clamp(0, 100);
        self.volume = vol;
        self.gain.set(crate::player::volume_to_gain(vol));
    }

    pub fn timeline_xml(&mut self, player: &Player, command_id: &str) -> String {
        while let Some(crate::player::PlayerEvent::TrackComplete { path }) =
            self.player.as_ref().and_then(|p| p.take_event())
        {
            debug!("timeline: track complete: {path}");
            self.complete_current();
        }
        let state = if self.player.is_none() {
            "stopped".into()
        } else {
            self.state.clone()
        };
        let time_ms = self.player.as_ref().map(|p| p.position_ms()).unwrap_or(0);
        let track_duration_ms = self
            .playlist
            .iter()
            .find(|t| t.key == self.track_key)
            .map(|t| t.duration_ms)
            .unwrap_or(0);
        let duration_ms = self
            .player
            .as_ref()
            .map(|p| p.duration_ms())
            .filter(|d| *d > 0)
            .unwrap_or(track_duration_ms);
        let track_key = self.track_key.clone();
        let title = self.title.clone();
        let artist = self.artist.clone();
        let media_path = self.media_path.clone();

        let current = self.playlist.iter().find(|t| t.key == self.track_key);
        let track_container = current.map(|t| t.container.clone()).unwrap_or_default();
        let track_codec = current.map(|t| t.codec.clone()).unwrap_or_default();
        let track_bitrate = current.map(|t| t.bitrate).unwrap_or(0);
        let track_thumb = current.map(|t| t.thumb.clone()).unwrap_or_default();
        let track_parent_title = current.map(|t| t.parent_title.clone()).unwrap_or_default();
        let track_user_rating = current.map(|t| t.user_rating).unwrap_or(0.0);
        let track_view_count = current.map(|t| t.view_count).unwrap_or(0);
        let track_rating_key = current.map(|t| t.rating_key.clone()).unwrap_or_default();
        let tl_codec = track_codec.clone();

        let tl_container = track_container.clone();
        let machine_id = if self.machine_identifier.is_empty() {
            player.device_id.as_str()
        } else {
            self.machine_identifier.as_str()
        };
        let timeline_key = track_key.clone();

        let play_queue_item_id = current
            .map(|t| t.play_queue_item_id.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| timeline_key.clone());
        let track = if timeline_key.is_empty() {
            None
        } else {
            let part_key = if media_path.is_empty() {
                track_key.clone()
            } else {
                media_path.clone()
            };
            Some(TrackXml {
                r#type: "track".to_string(),
                key: track_key,
                play_queue_item_id: play_queue_item_id.clone(),
                title,
                grandparent_title: artist,
                parent_title: track_parent_title,
                container: track_container.clone(),
                thumb: track_thumb,
                user_rating: (track_user_rating > 0.0).then_some(track_user_rating),
                view_count: (track_view_count > 0).then_some(track_view_count),
                rating_key: track_rating_key,
                source: machine_id.to_string(),
                love: 0,
                hate: 0,
                duration: duration_ms,
                media: MediaXml {
                    id: 1,
                    container: track_container.clone(),
                    bitrate: track_bitrate,
                    bit_depth: 16,
                    channels: 2,
                    codec: track_codec.clone(),
                    duration: duration_ms,
                    height: 0,
                    width: 0,
                    part: PartXml {
                        id: 1,
                        key: part_key,
                        container: track_container,
                        bitrate: track_bitrate,
                        audio_channels: 2,
                        audio_codec: track_codec,
                        duration: duration_ms,
                        size: 0,
                    },
                },
            })
        };
        let play_queue_container = if self.play_queue_id.is_empty() {
            String::new()
        } else {
            format!("/playQueues/{}?own=1", self.play_queue_id)
        };
        let container = TimelineContainer {
            command_id: command_id.to_string(),
            timeline: TimelineXml {
                r#type: "music".to_string(),
                item_type: "music".to_string(),
                state,
                time: time_ms,
                duration: duration_ms,
                volume: self.volume as u32,
                shuffle: self.shuffle,
                repeat: self.repeat,
                play_queue_id: self.play_queue_id.clone(),
                play_queue_item_id: play_queue_item_id.clone(),
                container: tl_container,
                container_key: play_queue_container,
                codec: tl_codec,
                bitrate: (track_bitrate > 0).then_some(track_bitrate),
                key: timeline_key,
                machine_identifier: machine_id.to_string(),
                source: machine_id.to_string(),
                play_queue_source: machine_id.to_string(),
                rate: 1,
                buffered: 0,
                transcoding: 0,
                dynamic_boost: 0,
                silence_compression: 0,
                controllable: "volume,repeat,skipPrevious,seekTo,stepBack,stepForward,stop,playPause,shuffle,skipNext".to_string(),
                track,
            },
        };
        serde_xml_rs::to_string(&container).unwrap_or_default()
    }
}
