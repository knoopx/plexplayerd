use serde::{Deserialize, Serialize};

// ── Plex XML data records: deserialized from server responses ────────────
// (attribute fields use the serde-xml-rs 0.8 "@name" rename convention)

#[derive(Debug, Deserialize)]
#[serde(rename = "MediaContainer")]
pub struct PlexContainer {
    // Current PMS shape (captured /playQueues and /library/metadata
    // responses are flat): <Track> elements sit directly under
    // <MediaContainer>, with no <QueueItem> or <Meta> wrapper.
    #[serde(rename = "Track", default)]
    pub track: Vec<PlexTrack>,
    #[serde(rename = "Meta", default)]
    pub meta: Vec<PlexMeta>,
    #[serde(rename = "QueueItem", default)]
    pub queue_items: Vec<PlexQueueItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "Meta")]
pub struct PlexMeta {
    #[serde(rename = "Track", default)]
    pub track: Vec<PlexTrack>,
    #[serde(rename = "Album", default)]
    pub album: Vec<PlexAlbum>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "Album")]
pub struct PlexAlbum {
    #[serde(rename = "Track", default)]
    pub track: Vec<PlexTrack>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "QueueItem")]
pub struct PlexQueueItem {
    #[serde(rename = "Track", default)]
    pub track: Vec<PlexTrack>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "Track")]
pub struct PlexTrack {
    #[serde(rename = "@title", default)]
    pub title: String,
    #[serde(rename = "@grandparentTitle", default)]
    pub grandparent_title: String,
    #[serde(rename = "@parentTitle", default)]
    pub parent_title: String,
    #[serde(rename = "@key", default)]
    pub key: String,
    #[serde(rename = "@ratingKey", default)]
    pub rating_key: String,
    #[serde(rename = "@playQueueItemID", default)]
    pub play_queue_item_id: String,
    #[serde(rename = "@duration", default)]
    pub duration: u64,
    #[serde(rename = "@thumb", default)]
    pub thumb: String,
    #[serde(rename = "@userRating", default)]
    pub user_rating: f64,
    #[serde(rename = "@viewCount", default)]
    pub view_count: u32,
    #[serde(rename = "Media", default)]
    pub media: Vec<PlexMedia>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "Media")]
pub struct PlexMedia {
    #[serde(rename = "Part", default)]
    pub parts: Vec<PlexPart>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "Part")]
pub struct PlexPart {
    #[serde(rename = "@key", default)]
    pub key: String,
    #[serde(rename = "@file", default)]
    pub file: String,
    #[serde(rename = "@container", default)]
    pub container: String,
    #[serde(rename = "@codec", default)]
    pub codec: String,
    #[serde(rename = "@audioCodec", default)]
    pub audio_codec: String,
    #[serde(rename = "@bitrate", default)]
    pub bitrate: u32,
}

impl PlexTrack {
    /// Resolve this track to a local file path or a PMS stream URL.
    ///
    /// For each part: if `Part@file` (or `Part@key` when `file` is empty)
    /// exists as a LOCAL path on the player machine -> `Some(path)`; otherwise,
    /// if `Part@key` is present -> a PMS stream URL (`base + key`; the token
    /// goes in the request header, not the URL). A track with neither a local
    /// part nor a part key is dropped (`None`).
    ///
    /// Grounded in module-26775 (`parts` = `t.Part || t.parts`) and the
    /// reference player, which streams parts over HTTP and never reads the
    /// server's disk paths (`Part@file` is a path on the PMS host, not the
    /// player machine).
    pub fn resolve(&self, base: &str) -> Option<(Option<String>, Option<String>)> {
        for media in &self.media {
            for part in &media.parts {
                // Local candidate: Part@file, falling back to Part@key.
                let local_raw = if part.file.is_empty() {
                    &part.key
                } else {
                    &part.file
                };
                let local = local_raw.trim_start_matches('/').to_string();
                if !local.is_empty() && std::path::Path::new(&local).exists() {
                    return Some((Some(local), None));
                }
                // Remote: stream the part over HTTP from the PMS.
                if !part.key.is_empty() {
                    let key = if part.key.starts_with('/') {
                        part.key.clone()
                    } else {
                        format!("/{}", part.key)
                    };
                    let url = format!("{base}{key}");
                    return Some((None, Some(url)));
                }
            }
        }
        None
    }

    /// The audio container/codec/bitrate from the first part, for the
    /// timeline's codec display. Container falls back to the part file
    /// extension; codec falls back to the part's audioCodec.
    pub fn media_info(&self) -> (String, String, u32) {
        let Some(part) = self.media.iter().find_map(|m| m.parts.first()) else {
            return (String::new(), String::new(), 0);
        };
        let container = if part.container.is_empty() {
            part.file.rsplit('.').next().unwrap_or("").to_lowercase()
        } else {
            part.container.clone()
        };
        let codec = if part.codec.is_empty() {
            part.audio_codec.clone()
        } else {
            part.codec.clone()
        };
        // Plex Part @bitrate is in kbps; the timeline expects bps.
        (container, codec, part.bitrate * 1000)
    }
}

// ── Plex XML data records: serialized to the server ───────────────────────

#[derive(Serialize)]
#[serde(rename = "MediaContainer")]
pub struct ResourcesXml {
    #[serde(rename = "@size")]
    pub size: u32,
    #[serde(rename = "Player")]
    pub player: PlayerXml,
}

#[derive(Serialize)]
#[serde(rename = "Player")]
pub struct PlayerXml {
    #[serde(rename = "@machineIdentifier")]
    pub machine_identifier: String,
    #[serde(rename = "@deviceClass")]
    pub device_class: String,
    #[serde(rename = "@platform")]
    pub platform: String,
    #[serde(rename = "@platformVersion")]
    pub platform_version: String,
    #[serde(rename = "@product")]
    pub product: String,
    #[serde(rename = "@protocol")]
    pub protocol: String,
    #[serde(rename = "@protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "@protocolCapabilities")]
    pub protocol_capabilities: String,
    #[serde(rename = "@title")]
    pub title: String,
    #[serde(rename = "@version")]
    pub version: String,
}

#[derive(Serialize)]
#[serde(rename = "MediaContainer")]
pub struct TimelineContainer {
    #[serde(rename = "@commandID")]
    pub command_id: String,
    #[serde(rename = "Timeline")]
    pub timeline: TimelineXml,
}

#[derive(Serialize)]
#[serde(rename = "Timeline")]
pub struct TimelineXml {
    #[serde(rename = "@type")]
    pub r#type: String,
    #[serde(rename = "@itemType")]
    pub item_type: String,
    #[serde(rename = "@state")]
    pub state: String,
    #[serde(rename = "@time")]
    pub time: u64,
    #[serde(rename = "@duration")]
    pub duration: u64,
    #[serde(rename = "@volume")]
    pub volume: u32,
    #[serde(rename = "@shuffle")]
    pub shuffle: u32,
    #[serde(rename = "@repeat")]
    pub repeat: u32,
    #[serde(rename = "@playQueueID")]
    pub play_queue_id: String,
    #[serde(rename = "@playQueueItemID")]
    pub play_queue_item_id: String,
    #[serde(rename = "@container")]
    pub container: String,
    #[serde(rename = "@containerKey")]
    pub container_key: String,
    #[serde(rename = "@codec")]
    pub codec: String,
    #[serde(rename = "@bitrate", skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    #[serde(rename = "@key")]
    pub key: String,
    #[serde(rename = "@machineIdentifier")]
    pub machine_identifier: String,
    #[serde(rename = "@source")]
    pub source: String,
    #[serde(rename = "@playQueueSource")]
    pub play_queue_source: String,
    #[serde(rename = "@rate")]
    pub rate: u32,
    #[serde(rename = "@buffered")]
    pub buffered: u64,
    #[serde(rename = "@transcoding")]
    pub transcoding: u32,
    #[serde(rename = "@dynamicBoost")]
    pub dynamic_boost: u32,
    #[serde(rename = "@silenceCompression")]
    pub silence_compression: u32,
    #[serde(rename = "@controllable")]
    pub controllable: String,
    #[serde(rename = "Track", default)]
    pub track: Option<TrackXml>,
}

#[derive(Serialize)]
#[serde(rename = "Track")]
pub struct TrackXml {
    #[serde(rename = "@type")]
    pub r#type: String,
    #[serde(rename = "@key")]
    pub key: String,
    #[serde(rename = "@playQueueItemID")]
    pub play_queue_item_id: String,
    #[serde(rename = "@title")]
    pub title: String,
    #[serde(rename = "@grandparentTitle")]
    pub grandparent_title: String,
    #[serde(rename = "@parentTitle")]
    pub parent_title: String,
    #[serde(rename = "@container")]
    pub container: String,
    #[serde(rename = "@thumb")]
    pub thumb: String,
    #[serde(rename = "@userRating", skip_serializing_if = "Option::is_none")]
    pub user_rating: Option<f64>,
    #[serde(rename = "@viewCount", skip_serializing_if = "Option::is_none")]
    pub view_count: Option<u32>,
    #[serde(rename = "@ratingKey")]
    pub rating_key: String,
    #[serde(rename = "@source")]
    pub source: String,
    #[serde(rename = "@love")]
    pub love: u32,
    #[serde(rename = "@hate")]
    pub hate: u32,
    #[serde(rename = "@duration")]
    pub duration: u64,
    #[serde(rename = "Media")]
    pub media: MediaXml,
}

#[derive(Serialize)]
#[serde(rename = "Media")]
pub struct MediaXml {
    #[serde(rename = "@id")]
    pub id: u32,
    #[serde(rename = "@container")]
    pub container: String,
    #[serde(rename = "@bitrate")]
    pub bitrate: u32,
    #[serde(rename = "@bitDepth")]
    pub bit_depth: u32,
    #[serde(rename = "@channels")]
    pub channels: u32,
    #[serde(rename = "@codec")]
    pub codec: String,
    #[serde(rename = "@duration")]
    pub duration: u64,
    #[serde(rename = "@height")]
    pub height: u32,
    #[serde(rename = "@width")]
    pub width: u32,
    #[serde(rename = "Part")]
    pub part: PartXml,
}

#[derive(Serialize)]
#[serde(rename = "Part")]
pub struct PartXml {
    #[serde(rename = "@id")]
    pub id: u32,
    #[serde(rename = "@key")]
    pub key: String,
    #[serde(rename = "@container")]
    pub container: String,
    #[serde(rename = "@bitrate")]
    pub bitrate: u32,
    #[serde(rename = "@audioChannels")]
    pub audio_channels: u32,
    #[serde(rename = "@audioCodec")]
    pub audio_codec: String,
    #[serde(rename = "@duration")]
    pub duration: u64,
    #[serde(rename = "@size")]
    pub size: u64,
}

#[cfg(test)]
mod xml_records_test {
    use super::*;

    #[test]
    fn serialize_resources() {
        let xml = ResourcesXml {
            size: 1,
            player: PlayerXml {
                machine_identifier: "abc123".into(),
                device_class: "pc".into(),
                platform: "linux".into(),
                platform_version: String::new(),
                product: "plexplayerd".into(),
                protocol: "plex".into(),
                protocol_version: "1".into(),
                protocol_capabilities: "timeline,playback".into(),
                title: "Test Player".into(),
                version: "1.0.0".into(),
            },
        };
        let s = serde_xml_rs::to_string(&xml).unwrap();
        assert!(s.contains("<MediaContainer size=\"1\">"));
        assert!(s.contains("machineIdentifier=\"abc123\""));
        assert!(s.contains("title=\"Test Player\""));
        assert!(s.contains("protocol=\"plex\""));
        assert!(!s.contains("<Name>"));
        println!("resources: {s}");
    }

    #[test]
    fn serialize_timeline() {
        let container = TimelineContainer {
            command_id: "7".into(),
            timeline: TimelineXml {
                r#type: "music".into(),
                item_type: "music".into(),
                state: "playing".into(),
                time: 12345,
                duration: 200000,
                volume: 100,
                shuffle: 0,
                repeat: 0,
                play_queue_id: "pq-1".into(),
                play_queue_item_id: "12345".into(),
                container: "mp3".into(),
                container_key: "/playQueues/pq-1?own=1".into(),
                codec: "mp3".into(),
                bitrate: Some(128000),
                key: "12345".into(),
                machine_identifier: "abc123".into(),
                source: "abc123".into(),
                play_queue_source: "abc123".into(),
                rate: 1,
                buffered: 0,
                transcoding: 0,
                dynamic_boost: 0,
                silence_compression: 0,
                controllable: "volume,playPause".into(),
                track: Some(TrackXml {
                    r#type: "track".into(),
                    key: "12345".into(),
                    play_queue_item_id: "12345".into(),
                    title: "Song & Title".into(),
                    grandparent_title: "Artist <X>".into(),
                    parent_title: "Album <Y>".into(),
                    container: "mp3".into(),
                    thumb: "/library/metadata/1/thumb".into(),
                    user_rating: Some(8.0),
                    view_count: Some(3),
                    rating_key: "12345".into(),
                    source: "abc123".into(),
                    love: 0,
                    hate: 0,
                    duration: 200000,
                    media: MediaXml {
                        id: 1,
                        container: "mp3".into(),
                        bitrate: 128000,
                        bit_depth: 16,
                        channels: 2,
                        codec: "mp3".into(),
                        duration: 200000,
                        height: 0,
                        width: 0,
                        part: PartXml {
                            id: 1,
                            key: "/library/metadata/1/12345".into(),
                            container: "mp3".into(),
                            bitrate: 128000,
                            audio_channels: 2,
                            audio_codec: "mp3".into(),
                            duration: 200000,
                            size: 0,
                        },
                    },
                }),
            },
        };
        let s = serde_xml_rs::to_string(&container).unwrap();
        assert!(s.contains("<MediaContainer commandID=\"7\">"));
        assert!(s.contains("<Timeline "));
        assert!(s.contains("<Track "));
        assert!(s.contains("<Media "));
        assert!(s.contains("<Part "));
        assert!(s.contains("title=\"Song &amp; Title\""));
        assert!(s.contains("grandparentTitle=\"Artist &lt;X&gt;\""));
        println!("timeline: {s}");
    }

    #[test]
    fn deserialize_track_metadata() {
        let xml = r#"<MediaContainer size="1" allowSync="1"><Meta type="track" key="1"><Track type="track" title="Song" grandparentTitle="Artist" key="12345" duration="200000"><Media id="1" container="mp3"><Part id="1" key="/library/metadata/1/12345.mp3" container="mp3" duration="200000"/></Media></Track></Meta></MediaContainer>"#;
        let c: PlexContainer = serde_xml_rs::from_str(xml).unwrap();
        let track = c.meta.iter().find_map(|m| m.track.first()).unwrap();
        let part = &track.media[0].parts[0];
        assert_eq!(track.title, "Song");
        assert_eq!(track.grandparent_title, "Artist");
        assert_eq!(track.duration, 200000);
        assert_eq!(part.key, "/library/metadata/1/12345.mp3");
    }

    #[test]
    fn deserialize_play_queue() {
        let xml = r#"<MediaContainer size="2"><QueueItem key="100" index="0"><Track type="track" title="A" grandparentTitle="ArtA" key="100" duration="100000"><Media id="1"><Part id="1" key="/library/metadata/1/100.mp3"/></Media></Track></QueueItem><QueueItem key="101" index="1"><Track type="track" title="B" grandparentTitle="ArtB" key="101" duration="200000"><Media id="1"><Part id="1" key="/library/metadata/1/101.mp3"/></Media></Track></QueueItem></MediaContainer>"#;
        let c: PlexContainer = serde_xml_rs::from_str(xml).unwrap();
        assert_eq!(c.queue_items.len(), 2);
        let t0 = &c.queue_items[0].track[0];
        assert_eq!(t0.title, "A");
        assert_eq!(t0.media[0].parts[0].key, "/library/metadata/1/100.mp3");
        let t1 = &c.queue_items[1].track[0];
        assert_eq!(t1.title, "B");
        assert_eq!(t1.duration, 200000);
    }

    #[test]
    fn deserialize_flat_play_queue() {
        // Compact FLAT queue shape matching the captured /tmp/queue917.xml:
        // <Track> elements sit directly under <MediaContainer> (no
        // <QueueItem> wrapper). The second part is a PMS-only part (no local
        // file on the player), so it must resolve to a stream URL.
        let xml = r#"<MediaContainer size="2" playQueueID="917"><Track type="track" title="The Thread" grandparentTitle="Lysistrata" key="/library/metadata/530889" duration="186331"><Media id="462842" container="mp3"><Part id="462842" key="/library/parts/462842/1567618582/file.mp3" file="/mnt/music/x.mp3" container="mp3" duration="186331"/></Media></Track><Track type="track" title="Remote Track" grandparentTitle="Remote Artist" key="/library/metadata/762232" duration="370259"><Media id="666282" container="mp3"><Part id="666282" key="/library/parts/462842/1567618582/file.mp3" container="mp3" duration="370259"/></Media></Track></MediaContainer>"#;
        let c: PlexContainer = serde_xml_rs::from_str(xml).unwrap();
        assert_eq!(c.track.len(), 2);
        assert!(c.queue_items.is_empty());
        let remote = &c.track[1];
        assert_eq!(
            remote.resolve("http://192.168.1.42:32400"),
            Some((
                None,
                Some("http://192.168.1.42:32400/library/parts/462842/1567618582/file.mp3".into())
            ))
        );
    }

    #[test]
    fn deserialize_flat_metadata() {
        // Compact FLAT metadata shape matching the captured
        // /tmp/meta762232.xml: a <Track> directly under <MediaContainer> (no
        // <Meta> wrapper), with a PMS part key (not a local path).
        let xml = r#"<MediaContainer size="1" allowSync="1"><Track type="track" title="Grab It From the Garden" grandparentTitle="ovlov" key="/library/metadata/762232" duration="370259"><Media id="666282" container="mp3"><Part id="666282" key="/library/parts/666282/1779752979/file.mp3" container="mp3" duration="370259"/></Media></Track></MediaContainer>"#;
        let c: PlexContainer = serde_xml_rs::from_str(xml).unwrap();
        assert_eq!(c.track.len(), 1);
        assert!(c.meta.is_empty());
        let track = &c.track[0];
        assert_eq!(track.title, "Grab It From the Garden");
        assert_eq!(track.grandparent_title, "ovlov");
        assert_eq!(
            track.resolve("http://192.168.1.42:32400"),
            Some((
                None,
                Some("http://192.168.1.42:32400/library/parts/666282/1779752979/file.mp3".into())
            ))
        );
    }
}
