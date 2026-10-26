use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use symphonia::core::codecs::audio::AudioDecoder;
use symphonia::core::formats::FormatReader;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::default::get_codecs;
use symphonia::default::get_probe;
use tracing::{debug, warn};

pub fn volume_to_gain(volume: i64) -> f32 {
    (volume.clamp(0, 100) as f32 / 100.0).clamp(0.0, 1.0)
}

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

#[derive(Clone, Default)]
pub struct SharedGain(Arc<AtomicU64>);

impl SharedGain {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(1.0f32.to_bits() as u64)))
    }

    pub fn set(&self, gain: f32) {
        self.0
            .store(gain.clamp(0.0, 1.0).to_bits() as u64, Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed) as u32)
    }
}

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

            let _ = hwp.set_buffer_time_near(500_000, ValueOr::Nearest);
            pcm.hw_params(&hwp)?;
        }
        pcm.prepare()?;
        Ok(AlsaOutput { pcm, channels })
    }

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

    pub fn write(&mut self, pcm: &[i16]) {
        let Some(out) = self.output.as_mut() else {
            return;
        };
        let mut scratch = pcm.to_vec();
        apply_gain(&mut scratch, self.gain.get());
        out.write(&scratch);
    }
}

pub enum Command {
    PlayTrack {
        path: Option<String>,
        url: Option<String>,
        offset_ms: u64,
        token: String,
    },
    Pause,
    Resume,
    Seek {
        offset_ms: u64,
    },
}

pub enum PlayerEvent {
    TrackComplete { path: String },
}

#[derive(Clone)]
pub enum TrackSource {
    File(String),
    Stream(StreamConfig),
}

#[derive(Clone)]
pub struct StreamConfig {
    pub url: String,
    pub token: String,
    pub client_id: String,
    pub product: String,
    pub version: String,
}

#[derive(Clone)]
pub struct StreamIdentity {
    pub client_id: String,
    pub product: String,
    pub version: String,
}

struct StreamSource {
    reader: Box<dyn Read + Send + Sync + 'static>,
}

fn stream_io_error(err: ureq::Error) -> std::io::Error {
    match err {
        ureq::Error::Status(code, _) => {
            std::io::Error::other(format!("PMS part stream rejected with HTTP {code}"))
        }
        ureq::Error::Transport(transport) => std::io::Error::other(transport.to_string()),
    }
}

impl StreamSource {
    fn open(cfg: &StreamConfig) -> std::io::Result<StreamSource> {
        let agent = ureq::AgentBuilder::new().build();
        let response = agent
            .get(&cfg.url)
            .set("X-Plex-Token", &cfg.token)
            .set("X-Plex-Client-Identifier", &cfg.client_id)
            .set("X-Plex-Product", &cfg.product)
            .set("X-Plex-Version", &cfg.version)
            .call()
            .map_err(stream_io_error)?;
        Ok(StreamSource {
            reader: response.into_reader(),
        })
    }
}

impl Read for StreamSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
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
    let (mss, label): (Box<dyn MediaSource>, String) = match source {
        TrackSource::File(path) => {
            let file = File::open(path).map_err(|e| {
                symphonia::core::errors::Error::IoError(std::io::Error::other(e.to_string()))
            })?;
            (Box::new(file), path.clone())
        }
        TrackSource::Stream(cfg) => {
            let src = StreamSource::open(cfg).map_err(symphonia::core::errors::Error::IoError)?;
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

pub struct Player {
    tx: mpsc::Sender<Command>,
    handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    pos_ms: Arc<AtomicU64>,
    dur_ms: Arc<AtomicU64>,
    events_rx: mpsc::Receiver<PlayerEvent>,
}

impl Player {
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

    pub fn position_ms(&self) -> u64 {
        self.pos_ms.load(Ordering::Relaxed)
    }

    pub fn duration_ms(&self) -> u64 {
        self.dur_ms.load(Ordering::Relaxed)
    }

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
