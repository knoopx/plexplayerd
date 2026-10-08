//! The playback engine: decode a local media file to i16 PCM (symphonia) and
//! write it straight to ALSA.
//!
//! Modeled on openairplay1's sink seam (`openairplay1-receiver/src/player.rs`):
//! a dedicated OS thread owns the decode loop and the device; the host hands it
//! commands over a channel and reads position back through atomics. The device
//! half (`AlsaOutput`) paces playback with blocking `writei` and recovers from
//! underruns; the sink applies live gain just before playback. A device that
//! won't open is logged and audio is discarded so the session keeps running.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Child, ChildStdout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use symphonia::core::codecs::audio::AudioDecoder;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::FormatReader;
use symphonia::core::formats::TrackType;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::default::get_codecs;
use symphonia::default::get_probe;
use tracing::{debug, warn};

// ── Live gain, shared between the volume setter and the sink ─────────────

/// Convert a Plex volume (0-100) to a linear gain in `[0.0, 1.0]`.
pub fn volume_to_gain(volume: i64) -> f32 {
    (volume.clamp(0, 100) as f32 / 100.0).clamp(0.0, 1.0)
}

/// Scale interleaved `i16` PCM by `gain` in place, rounding and clamping.
/// `gain >= 1.0` is a no-op fast path.
fn apply_gain(samples: &mut [i16], gain: f32) {
    if gain >= 1.0 {
        return;
    }
    if gain <= 0.0 {
        samples.fill(0);
        return;
    }
    for s in samples.iter_mut() {
        let scaled = (*s as f32 * gain).round();
        *s = scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    }
}

/// The playback gain, shared between the volume setter (host) and the sink
/// (thread). Outlives any single track, so a volume set before a track starts
/// isn't lost.
#[derive(Clone, Default)]
pub struct SharedGain(Arc<AtomicU64>);

impl SharedGain {
    /// Starts at full volume.
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(1.0f32.to_bits() as u64)))
    }

    /// Set the linear gain (`1.0` = full, `0.0` = mute).
    pub fn set(&self, gain: f32) {
        self.0
            .store(gain.clamp(0.0, 1.0).to_bits() as u64, Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed) as u32)
    }
}

// ── The device: ALSA output with blocking pacing + underrun recovery ─────

struct AlsaOutput {
    pcm: PCM,
    channels: usize,
}

impl AlsaOutput {
    fn open(device: &str, rate: u32, channels: usize) -> Result<AlsaOutput, alsa::Error> {
        let pcm = PCM::new(device, Direction::Playback, false)?;
        {
            let hwp = HwParams::any(&pcm)?;
            hwp.set_channels(channels as u32)?;
            hwp.set_rate(rate, ValueOr::Nearest)?;
            hwp.set_format(Format::s16())?;
            hwp.set_access(Access::RWInterleaved)?;
            // ~500 ms device buffer to absorb scheduling jitter; best-effort.
            let _ = hwp.set_buffer_time_near(500_000, ValueOr::Nearest);
            pcm.hw_params(&hwp)?;
        }
        pcm.prepare()?;
        Ok(AlsaOutput { pcm, channels })
    }

    /// Write all interleaved samples, blocking to pace playback and
    /// recovering from underruns.
    fn write(&mut self, samples: &[i16]) {
        let io = match self.pcm.io_i16() {
            Ok(io) => io,
            Err(e) => {
                warn!("player: ALSA io handle lost: {e}");
                return;
            }
        };
        let mut remaining = samples;
        while !remaining.is_empty() {
            match io.writei(remaining) {
                Ok(0) => break,
                Ok(written) => {
                    let advanced = written * self.channels;
                    remaining = &remaining[advanced..];
                }
                Err(e) => {
                    if self.pcm.try_recover(e, true).is_err() {
                        warn!("player: unrecoverable ALSA write error, dropping chunk");
                        return;
                    }
                }
            }
        }
    }
}

// ── The sink: gain-scaled writes to the device ────────────────────────────

/// ALSA playback sink. A device that won't open is logged and audio is
/// discarded (decode-only) so the session keeps running.
pub struct AlsaSink {
    output: Option<AlsaOutput>,
    gain: SharedGain,
}

impl AlsaSink {
    pub fn open(device: &str, rate: u32, channels: u32, gain: SharedGain) -> AlsaSink {
        let output = match AlsaOutput::open(device, rate, channels as usize) {
            Ok(out) => {
                debug!("player: ALSA \"{device}\" {rate} Hz {channels}ch");
                Some(out)
            }
            Err(e) => {
                warn!("player: cannot open ALSA \"{device}\" ({e}); decode-only");
                None
            }
        };
        AlsaSink { output, gain }
    }

    /// Apply the current gain (live) just before playback, then write.
    pub fn write(&mut self, pcm: &[i16]) {
        let Some(out) = self.output.as_mut() else {
            return; // device failed to open → discard
        };
        let mut scratch = pcm.to_vec();
        apply_gain(&mut scratch, self.gain.get());
        out.write(&scratch);
    }
}

// ── Commands, events, and the playback thread ─────────────────────────────

pub enum Command {
    /// Load and play a track from `offset_ms` (0 = start). Exactly one of
    /// `path` (a local media file) or `url` (a PMS part stream endpoint) is
    /// Some. `token` is the PMS `X-Plex-Token` used to authenticate a remote
    /// stream (ignored for local files). Replaces any currently-playing track.
    PlayTrack {
        path: Option<String>,
        url: Option<String>,
        offset_ms: u64,
        token: String,
    },
    Pause,
    Resume,
    /// Seek within the current track by re-opening it at `offset_ms`.
    Seek {
        offset_ms: u64,
    },
}

pub enum PlayerEvent {
    /// The current track reached end-of-stream.
    TrackComplete { path: String },
}

/// Where a track's bytes come from: a local file, or a PMS part streamed over
/// HTTP (the reference player never reads the server's disk paths — it streams
/// the part; grounded in module-26775 `parts`/`getFirstPart()` and the
/// `Part@key` stream endpoint).
#[derive(Clone)]
pub enum TrackSource {
    File(String),
    Stream(StreamConfig),
}

/// Everything needed to open a PMS part stream: the stream endpoint URL (no
/// token in the URL — the token is sent as the `X-Plex-Token` header) plus the
/// player identity headers (module-4030 `player.headers()` + `X-Plex-Token`).
#[derive(Clone)]
pub struct StreamConfig {
    pub url: String,
    pub token: String,
    pub client_id: String,
    pub product: String,
    pub version: String,
}

/// The player identity headers, fixed for the life of the engine.
#[derive(Clone)]
pub struct StreamIdentity {
    pub client_id: String,
    pub product: String,
    pub version: String,
}

/// A non-seekable symphonia `MediaSource` backed by a `curl` child's stdout.
///
/// Symphonia's `MediaSource` (0.6) is the composite `io::Read + io::Seek +
/// Send + Sync` plus `is_seekable()`/`byte_len()`. This source reads the
/// stream once, in order: `Read` pulls from the child's stdout, `Seek`
/// returns `Unsupported`, `is_seekable()` is false and `byte_len()` is
/// `None` (best-effort — a raw part stream has no known length). Symphonia's
/// MP3 demuxer works without seeking, so this is enough to decode.
///
/// The `Child` is owned here so that dropping the source (track change, stop,
/// engine drop) kills the curl child — no orphan processes. While paused the
/// decode loop stops reading, so the child blocks on a full pipe and the
/// stream is held open; resuming continues from where it left off (the
/// simplest grounded pause behaviour — no SIGSTOP, no restart).
struct StreamSource {
    child: Option<Child>,
    stdout: Mutex<ChildStdout>,
}

impl StreamSource {
    fn spawn(cfg: &StreamConfig) -> std::io::Result<StreamSource> {
        let mut cmd = std::process::Command::new("curl");
        let token = format!("X-Plex-Token: {}", cfg.token);
        let client = format!("X-Plex-Client-Identifier: {}", cfg.client_id);
        let product = format!("X-Plex-Product: {}", cfg.product);
        let version = format!("X-Plex-Version: {}", cfg.version);
        // Stream the part over HTTP from the PMS. No `-m` timeout: playback
        // paces the read, and killing the child on track change/stop closes
        // the connection (module-4030 request shape: identity headers +
        // X-Plex-Token, token in the header not the URL).
        cmd.args([
            "-s",
            "-H",
            token.as_str(),
            "-H",
            client.as_str(),
            "-H",
            product.as_str(),
            "-H",
            version.as_str(),
            cfg.url.as_str(),
        ]);
        cmd.stdout(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("curl stdout is piped");
        Ok(StreamSource {
            child: Some(child),
            stdout: Mutex::new(stdout),
        })
    }
}

impl Read for StreamSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.lock().unwrap().read(buf)
    }
}

impl Seek for StreamSource {
    fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "stream source is not seekable",
        ))
    }
}

impl MediaSource for StreamSource {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

impl Drop for StreamSource {
    fn drop(&mut self) {
        // Kill the curl child so no orphan processes are left behind.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One decoded-and-opened track: the symphonia decode state plus its sink.
struct TrackCtx {
    source: TrackSource,
    label: String,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u32,
    duration_ms: u64,
    frames_written: u64,
    sink: AlsaSink,
}

impl TrackCtx {
    /// Decode one packet and write it to the sink. Returns `false` at
    /// end-of-stream.
    fn step(&mut self) -> bool {
        let packet = match self.format.next_packet() {
            Ok(Some(p)) => p,
            _ => return false,
        };
        if packet.track_id != self.track_id {
            return self.step();
        }
        let buf = match self.decoder.decode(&packet) {
            Ok(b) => b,
            Err(symphonia::core::errors::Error::DecodeError(_)) => return true,
            Err(_) => return false,
        };
        let n = buf.samples_interleaved();
        if n > 0 {
            let mut samples = vec![0i16; n];
            buf.copy_to_slice_interleaved(&mut samples);
            self.sink.write(&samples);
            self.frames_written += buf.frames() as u64;
        }
        true
    }
}

fn open_track(
    source: &TrackSource,
    offset_ms: u64,
    device: &str,
    gain: SharedGain,
) -> Result<TrackCtx, symphonia::core::errors::Error> {
    // Local file: seekable MediaSourceStream over the file (existing behaviour).
    // Remote part: a non-seekable StreamSource over the curl child's stdout.
    let (mss, label): (Box<dyn MediaSource>, String) = match source {
        TrackSource::File(path) => {
            let file = File::open(path).map_err(|e| {
                symphonia::core::errors::Error::IoError(std::io::Error::other(e.to_string()))
            })?;
            (Box::new(file), path.clone())
        }
        TrackSource::Stream(cfg) => {
            let src = StreamSource::spawn(cfg).map_err(symphonia::core::errors::Error::IoError)?;
            (Box::new(src), cfg.url.clone())
        }
    };
    let mss = MediaSourceStream::new(mss, Default::default());
    let hint = Hint::new();
    let format = get_probe().probe(&hint, mss, Default::default(), Default::default())?;
    let track = format.default_track(TrackType::Audio).ok_or(
        symphonia::core::errors::Error::Unsupported("no audio track"),
    )?;
    let audio = track
        .codec_params
        .as_ref()
        .ok_or(symphonia::core::errors::Error::Unsupported(
            "no codec params",
        ))?
        .audio()
        .ok_or(symphonia::core::errors::Error::Unsupported(
            "no audio stream",
        ))?;
    let sample_rate = audio
        .sample_rate
        .ok_or(symphonia::core::errors::Error::Unsupported(
            "no sample rate",
        ))?;
    let channels = audio
        .channels
        .as_ref()
        .ok_or(symphonia::core::errors::Error::Unsupported(
            "no channel layout",
        ))?
        .count() as u32;
    let decoder = get_codecs().make_audio_decoder(audio, &Default::default())?;
    let track_id = track.id;
    let media = format.media_info();
    let duration_ms = media
        .duration
        .zip(media.time_base)
        .and_then(|(dur, tb)| tb.calc_duration(dur))
        .map(|t| (t.as_nanos() / 1_000_000) as u64)
        .unwrap_or(0);
    let sink = AlsaSink::open(device, sample_rate, channels, gain);
    let mut ctx = TrackCtx {
        source: source.clone(),
        label: label.clone(),
        format,
        decoder,
        track_id,
        sample_rate,
        channels,
        duration_ms,
        frames_written: 0,
        sink,
    };
    // For a non-seekable stream there is no byte seek: honour `offset_ms` by
    // decode-skip (the existing step_skip loop) — read and discard frames until
    // the target. For a local file the same loop works over the seekable file.
    if offset_ms > 0 {
        let target = offset_ms * sample_rate as u64 / 1000;
        while ctx.frames_written < target {
            let at_end = !ctx.step_skip();
            if at_end {
                break;
            }
        }
    }
    Ok(ctx)
}

impl TrackCtx {
    /// Decode and discard one packet (for seeking). Returns `false` at EOF.
    fn step_skip(&mut self) -> bool {
        let packet = match self.format.next_packet() {
            Ok(Some(p)) => p,
            _ => return false,
        };
        if packet.track_id != self.track_id {
            return self.step_skip();
        }
        let buf = match self.decoder.decode(&packet) {
            Ok(b) => b,
            Err(symphonia::core::errors::Error::DecodeError(_)) => return true,
            Err(_) => return false,
        };
        self.frames_written += buf.frames() as u64;
        true
    }
}

/// Owns the playback thread. Dropping it stops the thread and closes ALSA.
pub struct Player {
    tx: mpsc::Sender<Command>,
    handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    pos_ms: Arc<AtomicU64>,
    dur_ms: Arc<AtomicU64>,
    events_rx: mpsc::Receiver<PlayerEvent>,
}

impl Player {
    /// Spawn the playback thread. Never fails; a missing device degrades to
    /// decode-only. `identity` supplies the headers a remote part stream
    /// carries (module-4030 `player.headers()`); ignored for local files.
    pub fn spawn(device: &str, gain: SharedGain, identity: StreamIdentity) -> Player {
        let (tx, rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let pos_ms = Arc::new(AtomicU64::new(0));
        let dur_ms = Arc::new(AtomicU64::new(0));
        let thread_stop = stop.clone();
        let thread_device = device.to_string();
        let thread_gain = gain.clone();
        let thread_pos = pos_ms.clone();
        let thread_dur = dur_ms.clone();
        let thread_identity = identity;
        let handle = std::thread::Builder::new()
            .name("plexplayerd-audio".into())
            .spawn(move || {
                run(
                    thread_device,
                    thread_gain,
                    thread_pos,
                    thread_dur,
                    thread_stop,
                    events_tx,
                    rx,
                    thread_identity,
                )
            })
            .expect("spawn playback thread");
        Player {
            tx,
            handle: Some(handle),
            stop,
            pos_ms,
            dur_ms,
            events_rx,
        }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Elapsed playback position in ms (frozen while paused).
    pub fn position_ms(&self) -> u64 {
        self.pos_ms.load(Ordering::Relaxed)
    }

    /// Current track duration in ms (0 until a track is loaded).
    pub fn duration_ms(&self) -> u64 {
        self.dur_ms.load(Ordering::Relaxed)
    }

    /// Non-blocking check for a pending event (track completion).
    pub fn take_event(&self) -> Option<PlayerEvent> {
        self.events_rx.try_recv().ok()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// The flat parameter list mirrors the Plex playMedia command params 1:1; the
// extra arg is not worth a config struct at this call depth.
#[allow(clippy::too_many_arguments)]
fn run(
    device: String,
    gain: SharedGain,
    pos_ms: Arc<AtomicU64>,
    dur_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    events_tx: mpsc::Sender<PlayerEvent>,
    rx: mpsc::Receiver<Command>,
    identity: StreamIdentity,
) {
    let mut current: Option<TrackCtx> = None;
    let mut paused = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // Decode one step unless paused (position freezes while paused).
        // For a stream, pausing stops the read: the curl child blocks on a
        // full pipe and the stream is held open; resuming continues in place.
        if !paused {
            if let Some(ctx) = current.as_mut() {
                let at_end = !ctx.step();
                pos_ms.store(
                    ctx.frames_written * 1000 / ctx.sample_rate as u64,
                    Ordering::Relaxed,
                );
                if at_end {
                    let path = ctx.label.clone();
                    current = None;
                    let _ = events_tx.send(PlayerEvent::TrackComplete { path });
                }
            }
        }
        match rx.try_recv() {
            Ok(Command::PlayTrack {
                path,
                url,
                offset_ms,
                token,
            }) => {
                // Exactly one of path/url is Some (enforced by the caller).
                let source = match (path, url) {
                    (Some(p), _) => TrackSource::File(p),
                    (None, Some(u)) => TrackSource::Stream(StreamConfig {
                        url: u,
                        token,
                        client_id: identity.client_id.clone(),
                        product: identity.product.clone(),
                        version: identity.version.clone(),
                    }),
                    (None, None) => {
                        warn!("player: PlayTrack with neither path nor url; ignoring");
                        continue;
                    }
                };
                let label = match &source {
                    TrackSource::File(p) => p.clone(),
                    TrackSource::Stream(c) => c.url.clone(),
                };
                match open_track(&source, offset_ms, &device, gain.clone()) {
                    Ok(ctx) => {
                        debug!(
                            "player: playing '{}' ({} Hz {}ch, {} ms, offset {offset_ms}ms)",
                            ctx.label, ctx.sample_rate, ctx.channels, ctx.duration_ms
                        );
                        dur_ms.store(ctx.duration_ms, Ordering::Relaxed);
                        pos_ms.store(offset_ms, Ordering::Relaxed);
                        paused = false;
                        current = Some(ctx);
                    }
                    Err(e) => {
                        warn!("player: cannot open '{label}': {e}");
                        current = None;
                    }
                }
            }
            Ok(Command::Pause) => paused = true,
            Ok(Command::Resume) => paused = false,
            Ok(Command::Seek { offset_ms }) => {
                // Re-open the current source at `offset_ms`. A local file
                // re-opens and decode-skips; a stream re-fetches the part and
                // decode-skips (a non-seekable stream cannot seek backwards).
                if let Some(src) = current.as_ref().map(|c| c.source.clone()) {
                    match open_track(&src, offset_ms, &device, gain.clone()) {
                        Ok(ctx) => {
                            dur_ms.store(ctx.duration_ms, Ordering::Relaxed);
                            pos_ms.store(offset_ms, Ordering::Relaxed);
                            current = Some(ctx);
                        }
                        Err(e) => warn!("player: seek failed: {e}"),
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                if current.is_none() {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    }
    // Dropping `current` closes the ALSA device and (for a stream) kills the
    // curl child via StreamSource's Drop.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_volume_is_unity() {
        assert_eq!(volume_to_gain(100), 1.0);
    }

    #[test]
    fn zero_volume_is_mute() {
        assert_eq!(volume_to_gain(0), 0.0);
        assert_eq!(volume_to_gain(-10), 0.0);
    }

    #[test]
    fn half_volume_is_half_gain() {
        assert!((volume_to_gain(50) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn unity_gain_leaves_samples_untouched() {
        let mut s = [100i16, -200, 30000];
        apply_gain(&mut s, 1.0);
        assert_eq!(s, [100, -200, 30000]);
    }

    #[test]
    fn zero_gain_silences() {
        let mut s = [100i16, -200, 30000];
        apply_gain(&mut s, 0.0);
        assert_eq!(s, [0, 0, 0]);
    }

    #[test]
    fn half_gain_scales_and_rounds() {
        let mut s = [100i16, -100, 32767];
        apply_gain(&mut s, 0.5);
        assert_eq!(s, [50, -50, 16384]);
    }

    #[test]
    fn scaling_the_extremes_stays_in_range() {
        let mut s = [i16::MIN, i16::MAX];
        apply_gain(&mut s, 0.999);
        assert_eq!(s, [-32735, 32734]);
    }

    #[test]
    fn shared_gain_clamps_and_round_trips() {
        let gain = SharedGain::new();
        assert_eq!(gain.get(), 1.0);
        gain.set(0.1);
        assert!((gain.get() - 0.1).abs() < 1e-6);
        gain.set(2.0);
        assert_eq!(gain.get(), 1.0);
        gain.set(-1.0);
        assert_eq!(gain.get(), 0.0);
    }
}
