//! Voice: microphone → Opus → encrypted datagrams, and back out to speakers.
//!
//! Everything runs at 48 kHz mono in 20 ms frames (960 samples), Opus's
//! native rate. Devices at other rates are resampled at the edges.

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use hearth_core::{KIND_AUDIO, MediaPacket, MediaSender, PeerId, VoiceSession};

pub const RATE: u32 = 48_000;
const FRAME: usize = 960;
const BITRATE: i32 = 64_000;
/// Jitter buffer: start playing a speaker once this much audio is queued...
const PRIME: usize = FRAME * 2;
/// ...and drop old audio if the queue grows past this, to cap latency.
const MAX_QUEUE: usize = RATE as usize / 5;
const GATE_HOLD: Duration = Duration::from_millis(300);

pub enum Input {
    Microphone,
    /// A sine tone instead of a microphone, for testing.
    Tone { hz: f32 },
}

pub enum Output {
    Speakers,
    /// Write what we hear to a WAV file instead of playing it.
    Wav(PathBuf),
}

pub struct AudioOptions {
    pub input: Input,
    pub output: Output,
    /// Frames quieter than this (dBFS) aren't sent.
    pub gate_db: f32,
    /// Receives non-audio media (screen-share video) arriving on the session.
    pub on_media: Option<Box<dyn Fn(MediaPacket) + Send>>,
}

impl Default for AudioOptions {
    fn default() -> Self {
        Self { input: Input::Microphone, output: Output::Speakers, gate_db: -50.0, on_media: None }
    }
}

/// Live controls and meters, shared with the UI.
#[derive(Default)]
pub struct Controls {
    pub muted: AtomicBool,
    pub deafened: AtomicBool,
    /// Our input level in dBFS × 100 (as bits of an f32), and whether the gate is open.
    level: AtomicU32,
    pub transmitting: AtomicBool,
}

impl Controls {
    pub fn input_db(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

/// A running voice connection. Dropping it stops all audio.
pub struct Audio {
    pub controls: Arc<Controls>,
    sender: MediaSender,
    /// Non-fatal problems, e.g. no microphone (we still listen).
    pub warnings: Vec<String>,
    mixer: Arc<Mutex<Mixer>>,
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Audio {
    pub fn start(session: VoiceSession, opts: AudioOptions) -> Result<Self> {
        let controls = Arc::new(Controls::default());
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let (sender, incoming) = session.split();
        let on_media = opts.on_media;
        let mut threads = Vec::new();
        let mut warnings = Vec::new();

        // Device streams aren't Send on every platform, so one thread owns them.
        let needs_devices = matches!(opts.input, Input::Microphone) || matches!(opts.output, Output::Speakers);
        if needs_devices {
            let (ready_tx, ready_rx) = mpsc::channel();
            let (mic, speakers) = (matches!(opts.input, Input::Microphone), matches!(opts.output, Output::Speakers));
            let (captured, mixer, stop) = (captured.clone(), mixer.clone(), stop.clone());
            threads.push(thread::Builder::new().name("hearth-audio-io".into()).spawn(move || {
                let streams = open_devices(mic, speakers, captured, mixer);
                let ok = streams.is_ok();
                let _ = ready_tx.send(streams.as_ref().map(|s| s.2.clone()).map_err(|e| format!("{e:#}")));
                if ok {
                    while !stop.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(50));
                    }
                }
                drop(streams);
            })?);
            warnings = ready_rx.recv().context("audio thread died")?.map_err(|e| anyhow!(e))?;
        }

        // Encoder: pulls 20 ms frames (from the mic buffer or a tone) and sends them.
        {
            let (controls, stop, sender) = (controls.clone(), stop.clone(), sender.clone());
            let tone = match opts.input {
                Input::Tone { hz } => Some(hz),
                Input::Microphone => None,
            };
            let gate_db = opts.gate_db;
            threads.push(thread::Builder::new().name("hearth-encode".into()).spawn(move || {
                if let Err(e) = encode_loop(&sender, &captured, tone, gate_db, &controls, &stop) {
                    eprintln!("voice encoder stopped: {e:#}");
                }
            })?);
        }

        // Decoder: turns packets into per-speaker queues in the mixer.
        {
            let (mixer, stop) = (mixer.clone(), stop.clone());
            threads.push(thread::Builder::new().name("hearth-decode".into()).spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match incoming.recv_timeout(Duration::from_millis(100)) {
                        Ok(p) if p.kind == KIND_AUDIO => mixer.lock().expect("mixer").push(p.from, p.seq, &p.payload),
                        Ok(p) => {
                            if let Some(f) = &on_media {
                                f(p);
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?);
        }

        if let Output::Wav(path) = opts.output {
            let (mixer, stop, controls) = (mixer.clone(), stop.clone(), controls.clone());
            threads.push(thread::Builder::new().name("hearth-wav".into()).spawn(move || {
                if let Err(e) = wav_loop(&path, &mixer, &controls, &stop) {
                    eprintln!("wav writer stopped: {e:#}");
                }
            })?);
        }

        Ok(Self { controls, sender, warnings, mixer, stop, threads })
    }

    /// For sending other media (screen share) into the same voice session.
    pub fn sender(&self) -> MediaSender {
        self.sender.clone()
    }

    /// Recent loudness per remote speaker (dBFS), for "who's talking" indicators.
    pub fn speaking(&self) -> Vec<(PeerId, f32)> {
        self.mixer.lock().expect("mixer").peers.iter().map(|(p, s)| (*p, s.level_db)).collect()
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn rms_db(frame: &[f32]) -> f32 {
    let mean = frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32;
    10.0 * mean.max(1e-10).log10()
}

fn encode_loop(
    session: &MediaSender,
    captured: &Mutex<VecDeque<f32>>,
    tone: Option<f32>,
    gate_db: f32,
    controls: &Controls,
    stop: &AtomicBool,
) -> Result<()> {
    let mut enc = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)?;
    enc.set_bitrate(opus::Bitrate::Bits(BITRATE))?;
    enc.set_inband_fec(true)?;
    enc.set_packet_loss_perc(10)?;

    let mut frame = vec![0f32; FRAME];
    let mut packet = vec![0u8; 4000];
    let mut phase = 0f32;
    let mut gate_open_until = Instant::now();
    let mut next_tick = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        if let Some(hz) = tone {
            // Pace the synthetic source in real time.
            next_tick += Duration::from_millis(20);
            if let Some(wait) = next_tick.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            for s in frame.iter_mut() {
                *s = 0.3 * phase.sin();
                phase = (phase + std::f32::consts::TAU * hz / RATE as f32) % std::f32::consts::TAU;
            }
        } else {
            let mut buf = captured.lock().expect("capture buffer");
            if buf.len() < FRAME {
                drop(buf);
                thread::sleep(Duration::from_millis(3));
                continue;
            }
            for s in frame.iter_mut() {
                *s = buf.pop_front().expect("length checked");
            }
        }

        let db = rms_db(&frame);
        controls.level.store(db.to_bits(), Ordering::Relaxed);
        if db > gate_db {
            gate_open_until = Instant::now() + GATE_HOLD;
        }
        let transmit = Instant::now() < gate_open_until && !controls.muted.load(Ordering::Relaxed);
        controls.transmitting.store(transmit, Ordering::Relaxed);
        if transmit {
            let n = enc.encode_float(&frame, &mut packet)?;
            session.send(KIND_AUDIO, &packet[..n]);
        }
    }
    Ok(())
}

fn wav_loop(path: &PathBuf, mixer: &Mutex<Mixer>, controls: &Controls, stop: &AtomicBool) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut wav = hound::WavWriter::create(path, spec)?;
    let mut frame = vec![0f32; FRAME];
    let mut next_tick = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        next_tick += Duration::from_millis(20);
        if let Some(wait) = next_tick.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        mixer.lock().expect("mixer").pull(&mut frame, controls.deafened.load(Ordering::Relaxed));
        for s in &frame {
            wav.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
    }
    wav.finalize()?;
    Ok(())
}

// ---- mixing ----

struct Speaker {
    decoder: opus::Decoder,
    last_seq: Option<u32>,
    queue: VecDeque<f32>,
    primed: bool,
    level_db: f32,
}

#[derive(Default)]
struct Mixer {
    peers: HashMap<PeerId, Speaker>,
    scratch: Vec<f32>,
}

impl Mixer {
    fn push(&mut self, from: PeerId, seq: u32, packet: &[u8]) {
        if self.scratch.len() < FRAME * 6 {
            self.scratch.resize(FRAME * 6, 0.0);
        }
        let speaker = match self.peers.entry(from) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let Ok(decoder) = opus::Decoder::new(RATE, opus::Channels::Mono) else { return };
                v.insert(Speaker { decoder, last_seq: None, queue: VecDeque::new(), primed: false, level_db: -100.0 })
            }
        };
        let gap = speaker.last_seq.map(|l| seq.wrapping_sub(l)).unwrap_or(1);
        if gap == 0 || gap > u32::MAX / 2 {
            return; // duplicate or late
        }
        // The gate stops packets during silence; only conceal genuine short losses.
        if (2..=5).contains(&gap) {
            for _ in 0..gap - 2 {
                if let Ok(n) = speaker.decoder.decode_float(&[], &mut self.scratch[..FRAME], false) {
                    speaker.queue.extend(&self.scratch[..n]);
                }
            }
            // The current packet carries a low-bitrate copy of the one before it.
            if let Ok(n) = speaker.decoder.decode_float(packet, &mut self.scratch[..FRAME], true) {
                speaker.queue.extend(&self.scratch[..n]);
            }
        }
        speaker.last_seq = Some(seq);
        if let Ok(n) = speaker.decoder.decode_float(packet, &mut self.scratch, false) {
            speaker.level_db = rms_db(&self.scratch[..n]);
            speaker.queue.extend(&self.scratch[..n]);
        }
        if speaker.queue.len() > MAX_QUEUE {
            let excess = speaker.queue.len() - PRIME;
            speaker.queue.drain(..excess);
        }
    }

    /// Fills `out` (48 kHz mono) with everyone mixed together.
    fn pull(&mut self, out: &mut [f32], deafened: bool) {
        out.fill(0.0);
        for s in self.peers.values_mut() {
            if !s.primed {
                if s.queue.len() < PRIME {
                    continue;
                }
                s.primed = true;
            }
            for o in out.iter_mut() {
                match s.queue.pop_front() {
                    Some(v) => *o += v,
                    None => {
                        s.primed = false; // underrun: rebuild the cushion
                        s.level_db = -100.0;
                        break;
                    }
                }
            }
        }
        if deafened {
            out.fill(0.0);
        }
        for o in out.iter_mut() {
            // Soft clip so several loud speakers don't wrap.
            *o = o.tanh();
        }
    }
}

// ---- devices ----

/// Linear-interpolating resampler; `step` is input samples per output sample.
struct Resampler {
    step: f64,
    t: f64,
    last: f32,
    cur: f32,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, t: 0.0, last: 0.0, cur: 0.0 }
    }

    /// Push-style: feed input, append resampled output.
    fn process(&mut self, input: &[f32], out: &mut VecDeque<f32>) {
        for &x in input {
            while self.t < 1.0 {
                out.push_back(self.last + (x - self.last) * self.t as f32);
                self.t += self.step;
            }
            self.t -= 1.0;
            self.last = x;
        }
    }

    /// Pull-style: produce one output sample, drawing input as needed.
    fn next(&mut self, mut input: impl FnMut() -> f32) -> f32 {
        while self.t >= 1.0 {
            self.last = self.cur;
            self.cur = input();
            self.t -= 1.0;
        }
        let y = self.last + (self.cur - self.last) * self.t as f32;
        self.t += self.step;
        y
    }
}

type Streams = (Option<cpal::Stream>, Option<cpal::Stream>, Vec<String>);

fn open_devices(mic: bool, speakers: bool, captured: Arc<Mutex<VecDeque<f32>>>, mixer: Arc<Mutex<Mixer>>) -> Result<Streams> {
    let host = cpal::default_host();
    let mut warnings = Vec::new();
    // Without a usable microphone you can still listen, like joining muted.
    let input = match mic.then(|| open_mic(&host, captured)).transpose() {
        Ok(s) => s,
        Err(e) => {
            warnings.push(format!("listening only: {e:#}"));
            None
        }
    };
    let output = if speakers { Some(open_speakers(&host, mixer)?) } else { None };
    Ok((input, output, warnings))
}

fn open_mic(host: &cpal::Host, captured: Arc<Mutex<VecDeque<f32>>>) -> Result<cpal::Stream> {
    let dev = host.default_input_device().context("no microphone found")?;
    let cfg = dev.default_input_config().context("microphone")?;
    let s = match cfg.sample_format() {
        SampleFormat::F32 => build_input::<f32>(&dev, cfg.into(), captured),
        SampleFormat::I16 => build_input::<i16>(&dev, cfg.into(), captured),
        SampleFormat::I32 => build_input::<i32>(&dev, cfg.into(), captured),
        other => bail!("unsupported microphone sample format {other}"),
    }
    .context("opening microphone")?;
    s.play().context("starting microphone")?;
    Ok(s)
}

fn open_speakers(host: &cpal::Host, mixer: Arc<Mutex<Mixer>>) -> Result<cpal::Stream> {
    let dev = host.default_output_device().context("no speakers found")?;
    let cfg = dev.default_output_config().context("speakers")?;
    let s = match cfg.sample_format() {
        SampleFormat::F32 => build_output::<f32>(&dev, cfg.into(), mixer),
        SampleFormat::I16 => build_output::<i16>(&dev, cfg.into(), mixer),
        SampleFormat::I32 => build_output::<i32>(&dev, cfg.into(), mixer),
        other => bail!("unsupported speaker sample format {other}"),
    }
    .context("opening speakers")?;
    s.play().context("starting speakers")?;
    Ok(s)
}

fn build_input<T>(dev: &cpal::Device, cfg: StreamConfig, captured: Arc<Mutex<VecDeque<f32>>>) -> Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = cfg.channels as usize;
    let mut resampler = Resampler::new(cfg.sample_rate, RATE);
    let mut mono = Vec::new();
    let stream = dev.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            mono.clear();
            mono.extend(data.chunks(channels).map(|c| c.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / channels as f32));
            if let Ok(mut buf) = captured.lock() {
                resampler.process(&mono, &mut buf);
                let cap = RATE as usize; // never hold more than a second
                if buf.len() > cap {
                    let excess = buf.len() - cap;
                    buf.drain(..excess);
                }
            }
        },
        |e| eprintln!("microphone error: {e}"),
        None,
    )?;
    Ok(stream)
}

fn build_output<T>(dev: &cpal::Device, cfg: StreamConfig, mixer: Arc<Mutex<Mixer>>) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = cfg.channels as usize;
    let mut resampler = Resampler::new(RATE, cfg.sample_rate);
    let mut pending: VecDeque<f32> = VecDeque::new();
    let mut block = vec![0f32; FRAME];
    let stream = dev.build_output_stream(
        cfg,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / channels;
            let need = (frames as f64 * resampler.step).ceil() as usize + 2;
            while pending.len() < need {
                if let Ok(mut m) = mixer.lock() {
                    m.pull(&mut block, false);
                }
                pending.extend(&block);
            }
            for frame in data.chunks_mut(channels) {
                let v = resampler.next(|| pending.pop_front().unwrap_or(0.0));
                frame.fill(T::from_sample(v));
            }
        },
        |e| eprintln!("speaker error: {e}"),
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_preserves_duration() {
        let mut r = Resampler::new(44_100, RATE);
        let mut out = VecDeque::new();
        r.process(&vec![0.5; 44_100], &mut out);
        assert!((out.len() as i64 - RATE as i64).abs() <= 2, "{}", out.len());
    }

    #[test]
    fn mixer_decodes_and_conceals_loss() {
        let mut enc = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let tone: Vec<f32> = (0..FRAME).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect();
        let mut pkt = vec![0u8; 4000];
        let peer = hearth_core::SecretKey::generate().public();
        let mut m = Mixer::default();
        for seq in [1u32, 2, 4, 5] {
            let n = enc.encode_float(&tone, &mut pkt).unwrap();
            m.push(peer, seq, &pkt[..n]);
        }
        // 4 received + 1 concealed frame.
        assert_eq!(m.peers[&peer].queue.len(), FRAME * 5);
        let mut out = vec![0f32; FRAME];
        m.pull(&mut out, false);
        assert!(rms_db(&out) > -30.0, "should hear the tone");
        m.pull(&mut out, true);
        assert!(out.iter().all(|s| *s == 0.0), "deafened");
    }
}
