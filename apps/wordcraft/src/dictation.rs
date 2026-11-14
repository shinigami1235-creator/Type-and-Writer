//! Dictate (Type and Writer): the microphone's sound, cut into phrases at the pauses, written
//! down by whisper.cpp running on this computer (nothing leaves it). The speech model and
//! `whisper-server` live in a `whisper` folder beside the program; the server starts the first
//! time Dictate is used and stays up until the program closes.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Sample rate whisper takes.
pub const RATE: u32 = 16_000;

/// Where the speech model and server are: a `whisper` folder beside the program, or
/// `TYPE_AND_WRITER_WHISPER_DIR`.
pub fn whisper_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("TYPE_AND_WRITER_WHISPER_DIR") {
        return Some(PathBuf::from(d));
    }
    Some(std::env::current_exe().ok()?.parent()?.join("whisper"))
}

fn server_exe(dir: &Path) -> PathBuf {
    dir.join(if cfg!(windows) { "whisper-server.exe" } else { "whisper-server" })
}

/// The speech model to use: an English "small" model if there is one, else "base", else any.
pub fn model_in(dir: &Path) -> Option<PathBuf> {
    let mut models: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bin") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("ggml-")))
        .collect();
    let rank = |p: &PathBuf| {
        let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        (
            if n.contains("small") {
                0
            } else if n.contains("base") {
                1
            } else {
                2
            },
            n,
        )
    };
    models.sort_by_key(rank);
    models.into_iter().next()
}

/// Cuts a 16 kHz mono stream into phrases: speech from its first sound until the speaker pauses
/// (or a phrase runs long), with a little sound kept before it so first syllables aren't clipped.
pub struct Segmenter {
    frame: Vec<f32>,
    preroll: Vec<f32>,
    current: Vec<f32>,
    in_speech: bool,
    silent_frames: usize,
    speech_frames: usize,
    floor: f32,
}

/// 30 ms frames.
const FRAME: usize = (RATE as usize * 30) / 1000;
/// A pause this long ends a phrase.
const PAUSE_FRAMES: usize = 27; // ~0.8 s
/// Sound kept before speech starts.
const PREROLL_FRAMES: usize = 10; // 0.3 s
/// The longest phrase.
const MAX_FRAMES: usize = 20_000 / 30; // 20 s
/// Phrases with less speech than this are coughs and clicks.
const MIN_SPEECH_FRAMES: usize = 10; // 0.3 s

impl Default for Segmenter {
    fn default() -> Self {
        Segmenter {
            frame: Vec::with_capacity(FRAME),
            preroll: Vec::new(),
            current: Vec::new(),
            in_speech: false,
            silent_frames: 0,
            speech_frames: 0,
            floor: 0.002,
        }
    }
}

impl Segmenter {
    /// Feed samples; returns the phrases they finished.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        for &s in samples {
            self.frame.push(s);
            if self.frame.len() == FRAME {
                let frame = std::mem::replace(&mut self.frame, Vec::with_capacity(FRAME));
                if let Some(p) = self.frame_done(frame) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// End of the stream: the phrase being spoken, if any.
    pub fn finish(&mut self) -> Option<Vec<f32>> {
        let speech = self.speech_frames;
        self.in_speech = false;
        self.speech_frames = 0;
        self.silent_frames = 0;
        let p = std::mem::take(&mut self.current);
        (speech >= MIN_SPEECH_FRAMES).then_some(p)
    }

    fn frame_done(&mut self, frame: Vec<f32>) -> Option<Vec<f32>> {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        // The noise floor follows quiet stretches down quickly and rises only slowly.
        if !self.in_speech {
            self.floor = if rms < self.floor { rms.max(1e-4) } else { self.floor * 0.995 + rms * 0.005 };
        }
        let loud = rms > (self.floor * 3.5).max(0.006);
        if self.in_speech {
            self.current.extend_from_slice(&frame);
            if loud {
                self.speech_frames += 1;
                self.silent_frames = 0;
            } else {
                self.silent_frames += 1;
            }
            let frames = self.current.len() / FRAME;
            if self.silent_frames >= PAUSE_FRAMES || frames >= MAX_FRAMES {
                return self.finish();
            }
        } else if loud {
            self.in_speech = true;
            self.speech_frames = 1;
            self.silent_frames = 0;
            self.current = std::mem::take(&mut self.preroll);
            self.current.extend_from_slice(&frame);
        } else {
            self.preroll.extend_from_slice(&frame);
            let keep = PREROLL_FRAMES * FRAME;
            if self.preroll.len() > keep {
                let cut = self.preroll.len() - keep;
                self.preroll.drain(..cut);
            }
        }
        None
    }
}

/// 16-bit mono WAV bytes of `samples` at [`RATE`].
pub fn wav(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.extend_from_slice(&v.to_le_bytes());
    }
    w
}

/// Whisper's notes about the sound rather than words (`[BLANK_AUDIO]`, `(music)`) taken out.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0i32;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A running `whisper-server`.
pub struct Server {
    child: Child,
    addr: SocketAddr,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    pub fn start(dir: &Path) -> Result<Server, String> {
        let exe = server_exe(dir);
        if !exe.is_file() {
            return Err(format!("The speech engine is missing from {}. Reinstall Type and Writer.", dir.display()));
        }
        let model = model_in(dir).ok_or_else(|| format!("The speech model is missing from {}. Reinstall Type and Writer.", dir.display()))?;
        // A free port on this computer only.
        let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).map_err(|e| e.to_string())?;
        let threads = std::thread::available_parallelism().map(|n| n.get().clamp(2, 8)).unwrap_or(4);
        let mut cmd = Command::new(&exe);
        cmd.arg("-m").arg(&model).args(["--host", "127.0.0.1", "--port", &port.to_string(), "-t", &threads.to_string()]);
        cmd.current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // No console window.
            cmd.creation_flags(0x0800_0000);
        }
        let child = cmd.spawn().map_err(|e| format!("The speech engine didn't start: {e}"))?;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let mut server = Server { child, addr };
        // Loading the model takes a few seconds.
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
                return Ok(server);
            }
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(format!("The speech engine stopped while starting ({status})."));
            }
            if Instant::now() > until {
                return Err("The speech engine took too long to start.".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Write down one phrase.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, String> {
        let boundary = "TypeAndWriterBoundary7f3a";
        let mut body = Vec::new();
        for (name, value) in [("temperature", "0.0"), ("response_format", "json")] {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
        }
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"phrase.wav\"\r\nContent-Type: audio/wav\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(&wav(samples));
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let mut s = TcpStream::connect_timeout(&self.addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(120))).map_err(|e| e.to_string())?;
        let head = format!(
            "POST /inference HTTP/1.1\r\nHost: {}\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.addr,
            body.len()
        );
        s.write_all(head.as_bytes()).and_then(|_| s.write_all(&body)).map_err(|e| e.to_string())?;
        let mut resp = Vec::new();
        s.take(8 << 20).read_to_end(&mut resp).map_err(|e| e.to_string())?;
        let split = resp.windows(4).position(|w| w == b"\r\n\r\n").ok_or("the speech engine gave no answer")?;
        let (headers, rest) = resp.split_at(split + 4);
        let headers = String::from_utf8_lossy(headers);
        if !headers.starts_with("HTTP/1.1 200") && !headers.starts_with("HTTP/1.0 200") {
            return Err(format!("the speech engine said: {}", headers.lines().next().unwrap_or("")));
        }
        let body = if headers.to_ascii_lowercase().contains("transfer-encoding: chunked") { dechunk(rest) } else { rest.to_vec() };
        let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        Ok(clean(v.get("text").and_then(serde_json::Value::as_str).unwrap_or("")))
    }
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(eol) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..eol]).trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = eol + 2;
        let Some(chunk) = b.get(start..start + size) else { break };
        out.extend_from_slice(chunk);
        b = b.get(start + size + 2..).unwrap_or_default();
    }
    out
}

/// What the listening thread tells the window.
enum Heard {
    Text(String),
    Error(String),
}

/// Dictate for the desktop app.
pub struct Desktop {
    on: Arc<AtomicBool>,
    errors: Vec<String>,
    rx: Option<Receiver<Heard>>,
    server: Arc<Mutex<Option<Server>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Desktop {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Desktop {
        Desktop { on: Arc::new(AtomicBool::new(false)), errors: Vec::new(), rx: None, server: Arc::new(Mutex::new(None)), wake }
    }
}

impl wordcraft_ui_egui::Dictation for Desktop {
    fn start(&mut self) -> Result<(), String> {
        if self.on.load(Ordering::SeqCst) {
            return Ok(());
        }
        let dir = whisper_dir().ok_or("The program's folder couldn't be found.")?;
        if !server_exe(&dir).is_file() || model_in(&dir).is_none() {
            return Err(format!("The speech engine or model is missing from {}. Reinstall Type and Writer.", dir.display()));
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.on.store(true, Ordering::SeqCst);
        let (on, server, wake) = (self.on.clone(), self.server.clone(), self.wake.clone());
        std::thread::Builder::new().name("dictate".into()).spawn(move || listen(&dir, &on, &server, &tx, &*wake)).map_err(|e| e.to_string())?;
        Ok(())
    }
    fn stop(&mut self) {
        self.on.store(false, Ordering::SeqCst);
    }
    fn listening(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }
    fn take_text(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok(h) = rx.try_recv() {
                match h {
                    Heard::Text(t) => out.push(t),
                    Heard::Error(e) => self.errors.push(e),
                }
            }
        }
        out
    }
    fn take_error(&mut self) -> Option<String> {
        (!self.errors.is_empty()).then(|| self.errors.remove(0))
    }
}

/// The listening thread: microphone → phrases → text, until Dictate is turned off.
fn listen(dir: &Path, on: &AtomicBool, server: &Mutex<Option<Server>>, tx: &Sender<Heard>, wake: &(dyn Fn() + Send + Sync)) {
    let fail = |msg: String| {
        on.store(false, Ordering::SeqCst);
        let _ = tx.send(Heard::Error(msg));
        wake();
    };
    // Start the speech engine first (once per run of the program).
    {
        let mut s = server.lock().unwrap_or_else(|e| e.into_inner());
        if s.is_none() {
            match Server::start(dir) {
                Ok(srv) => *s = Some(srv),
                Err(e) => return fail(e),
            }
        }
    }
    let (atx, arx) = channel::<Vec<f32>>();
    let mic = match mic::open(atx) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let mut seg = Segmenter::default();
    listen_loop(on, server, tx, wake, &arx, &mut seg);
    mic.close();
    // What was being said when Dictate was turned off.
    if let Some(p) = seg.finish() {
        let s = server.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(srv) = s.as_ref()
            && let Ok(t) = srv.transcribe(&p)
            && !t.is_empty()
        {
            let _ = tx.send(Heard::Text(t));
        }
    }
    wake();
}

/// Phrases heard until Dictate is turned off, each written down as it ends.
fn listen_loop(
    on: &AtomicBool,
    server: &Mutex<Option<Server>>,
    tx: &Sender<Heard>,
    wake: &(dyn Fn() + Send + Sync),
    arx: &Receiver<Vec<f32>>,
    seg: &mut Segmenter,
) {
    let mut phrases: Vec<Vec<f32>> = Vec::new();
    while on.load(Ordering::SeqCst) {
        match arx.recv_timeout(Duration::from_millis(100)) {
            Ok(samples) => phrases.extend(seg.push(&samples)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
        for p in phrases.drain(..) {
            let s = server.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(srv) = s.as_ref() {
                match srv.transcribe(&p) {
                    Ok(t) if !t.is_empty() => {
                        let _ = tx.send(Heard::Text(t));
                        wake();
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("dictate: {e}"),
                }
            }
        }
    }
}

/// The microphone, as 16 kHz mono samples sent to a channel.
#[cfg(any(windows, target_os = "macos"))]
mod mic {
    use super::RATE;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::mpsc::Sender;

    pub struct Mic(cpal::Stream);

    impl Mic {
        /// Stop listening.
        pub fn close(self) {
            let _ = self.0.pause();
        }
    }

    /// Mono, then from the device's rate to 16 kHz (linear: speech needs no better).
    struct Resample {
        step: f64,
        pos: f64,
        last: f32,
    }

    impl Resample {
        fn run(&mut self, mono: &[f32]) -> Vec<f32> {
            let mut out = Vec::with_capacity((mono.len() as f64 / self.step) as usize + 1);
            let mut i = self.pos;
            while (i as usize) < mono.len() {
                let k = i as usize;
                let a = if k == 0 { self.last } else { mono.get(k - 1).copied().unwrap_or(0.0) };
                let b = mono.get(k).copied().unwrap_or(0.0);
                let f = (i - k as f64) as f32;
                out.push(a + (b - a) * f);
                i += self.step;
            }
            self.pos = i - mono.len() as f64;
            self.last = mono.last().copied().unwrap_or(self.last);
            out
        }
    }

    pub fn open(tx: Sender<Vec<f32>>) -> Result<Mic, String> {
        let host = cpal::default_host();
        let dev = host.default_input_device().ok_or("No microphone found. Plug one in or turn it on in Windows sound settings.")?;
        let cfg = dev.default_input_config().map_err(|e| format!("The microphone can't be used: {e}"))?;
        let channels = usize::from(cfg.channels()).max(1);
        let rate = cfg.sample_rate().0.max(1);
        let mut rs = Resample { step: f64::from(rate) / f64::from(RATE), pos: 0.0, last: 0.0 };
        let mut send = move |mono: Vec<f32>| {
            let _ = tx.send(rs.run(&mono));
        };
        let err = |e: cpal::StreamError| log::warn!("microphone: {e}");
        let stream_cfg: cpal::StreamConfig = cfg.clone().into();
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => dev.build_input_stream(
                &stream_cfg,
                move |d: &[f32], _: &cpal::InputCallbackInfo| send(d.chunks(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect()),
                err,
                None,
            ),
            cpal::SampleFormat::I16 => dev.build_input_stream(
                &stream_cfg,
                move |d: &[i16], _: &cpal::InputCallbackInfo| {
                    send(d.chunks(channels).map(|c| c.iter().map(|&s| f32::from(s) / 32768.0).sum::<f32>() / channels as f32).collect())
                },
                err,
                None,
            ),
            cpal::SampleFormat::U16 => dev.build_input_stream(
                &stream_cfg,
                move |d: &[u16], _: &cpal::InputCallbackInfo| {
                    send(d.chunks(channels).map(|c| c.iter().map(|&s| (f32::from(s) - 32768.0) / 32768.0).sum::<f32>() / channels as f32).collect())
                },
                err,
                None,
            ),
            other => return Err(format!("The microphone's sound format ({other:?}) isn't supported.")),
        }
        .map_err(|e| format!("The microphone can't be used: {e}"))?;
        stream.play().map_err(|e| format!("The microphone didn't start: {e}"))?;
        Ok(Mic(stream))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod mic {
    use std::sync::mpsc::Sender;
    pub struct Mic;
    impl Mic {
        pub fn close(self) {}
    }
    pub fn open(_tx: Sender<Vec<f32>>) -> Result<Mic, String> {
        Err("Dictation isn't available on Linux yet.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize).map(|i| amp * (i as f32 * 0.05).sin()).collect()
    }

    #[test]
    fn phrases_end_at_pauses() {
        let mut s = Segmenter::default();
        let mut audio = tone(0.5, 0.0005);
        audio.extend(tone(1.2, 0.3));
        audio.extend(tone(1.0, 0.0005));
        audio.extend(tone(0.8, 0.3));
        audio.extend(tone(1.0, 0.0005));
        let phrases = s.push(&audio);
        assert_eq!(phrases.len(), 2, "two phrases split by the pause");
        assert!(phrases[0].len() as f32 / RATE as f32 > 1.2);
    }

    #[test]
    fn clicks_are_not_phrases() {
        let mut s = Segmenter::default();
        let mut audio = tone(0.5, 0.0005);
        audio.extend(tone(0.1, 0.4));
        audio.extend(tone(1.5, 0.0005));
        assert!(s.push(&audio).is_empty());
    }

    #[test]
    fn notes_about_sound_are_dropped() {
        assert_eq!(clean(" [BLANK_AUDIO] "), "");
        assert_eq!(clean(" Hello there. (coughs) How are you?"), "Hello there. How are you?");
    }

    #[test]
    fn wav_header() {
        let w = wav(&[0.0; 160]);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(w.len(), 44 + 320);
    }
}

#[cfg(test)]
mod real_engine {
    use super::*;

    /// With TYPE_AND_WRITER_WHISPER_DIR set to a folder holding whisper-server and a model, and
    /// TAW_TEST_WAV to a 16 kHz mono 16-bit recording: cut it at its pauses and write it down.
    #[test]
    #[ignore]
    fn writes_down_a_recording() {
        let dir = whisper_dir().unwrap();
        let wav_bytes = std::fs::read(std::env::var("TAW_TEST_WAV").unwrap()).unwrap();
        let samples: Vec<f32> = wav_bytes[44..].chunks_exact(2).map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0).collect();
        let mut seg = Segmenter::default();
        let mut phrases = Vec::new();
        for chunk in samples.chunks(1600) {
            phrases.extend(seg.push(chunk));
        }
        phrases.extend(seg.finish());
        let server = Server::start(&dir).unwrap();
        let t = std::time::Instant::now();
        for p in &phrases {
            println!("{:.1}s -> {:?}", p.len() as f32 / RATE as f32, server.transcribe(p).unwrap());
        }
        println!("{} phrases in {:?}", phrases.len(), t.elapsed());
        assert!(!phrases.is_empty());
    }
}
