//! The desktop's own sounds: a notification chime and a camera shutter. They are synthesised here (no sound files to
//! ship or to find on the system) and played through whichever command-line player the system has: `pw-play`
//! (PipeWire), `paplay` (PulseAudio), `aplay` (ALSA) or sox's `play`.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const RATE: u32 = 44_100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// A short two-note chime for a notification.
    Notification,
    /// A camera shutter for a screenshot.
    Screenshot,
}

impl Sound {
    fn name(self) -> &'static str {
        match self {
            Sound::Notification => "notification",
            Sound::Screenshot => "screenshot",
        }
    }

    /// Mono samples at [`RATE`], between -1 and 1.
    fn samples(self) -> Vec<f32> {
        match self {
            Sound::Notification => chime(),
            Sound::Screenshot => shutter(),
        }
    }
}

/// A sine note with a soft second harmonic that dies away, starting at `at` seconds into `out`.
fn note(out: &mut [f32], at: f32, freq: f32, len: f32, gain: f32) {
    let start = (at * RATE as f32) as usize;
    let n = (len * RATE as f32) as usize;
    for i in 0..n {
        let Some(slot) = out.get_mut(start + i) else { break };
        let t = i as f32 / RATE as f32;
        let attack = (t / 0.004).min(1.0); // no click at the start
        let decay = (-t * 7.0 / len).exp();
        let tau = std::f32::consts::TAU;
        *slot += gain * attack * decay * ((tau * freq * t).sin() + 0.25 * (tau * freq * 2.0 * t).sin());
    }
}

fn chime() -> Vec<f32> {
    let mut out = vec![0.0; (0.55 * RATE as f32) as usize];
    note(&mut out, 0.0, 880.0, 0.22, 0.30); // A5, then a fifth above
    note(&mut out, 0.11, 1318.5, 0.42, 0.30);
    out
}

fn shutter() -> Vec<f32> {
    let mut out = vec![0.0; (0.30 * RATE as f32) as usize];
    let mut seed: u32 = 0x1234_5678;
    let mut prev = 0.0f32;
    // two clicks: the mirror going up, then the curtain closing (a little softer and lower)
    for (at, gain, thump) in [(0.0f32, 0.9f32, 190.0f32), (0.075, 0.6, 140.0)] {
        let start = (at * RATE as f32) as usize;
        for i in 0..(0.09 * RATE as f32) as usize {
            let Some(slot) = out.get_mut(start + i) else { break };
            let t = i as f32 / RATE as f32;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / 8_388_608.0 - 1.0;
            let high = noise - prev; // differencing leaves the high frequencies: a crisp tick
            prev = noise;
            let tick = high * 0.5 * (-t * 220.0).exp();
            let body = (std::f32::consts::TAU * thump * t).sin() * (-t * 55.0).exp() * 0.6;
            *slot += gain * (tick + body);
        }
    }
    out
}

/// 16-bit mono PCM in a WAV container.
fn wav(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32_000.0) as i16).to_le_bytes());
    }
    out
}

/// The sound as a file (written once per login, in the runtime dir).
fn file(sound: Sound) -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("vesper-sound-{}-v1.wav", sound.name()));
    if !path.is_file() {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, wav(&sound.samples())).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
    }
    Some(path)
}

/// Play `sound` in the background. Does nothing, quietly, if no player is installed.
pub fn play(sound: Sound) {
    std::thread::spawn(move || {
        let Some(path) = file(sound) else { return };
        for player in [&["pw-play"][..], &["paplay"], &["aplay", "-q"], &["play", "-q"]] {
            let spawned = Command::new(player[0]).args(&player[1..]).arg(&path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            if let Ok(mut child) = spawned {
                let _ = child.wait();
                return;
            }
        }
    });
}

/// Rate limit for [`play`]: true at most once per `gap`, so a burst of notifications chimes once.
pub struct Throttle(Option<Instant>);

impl Throttle {
    pub fn new() -> Self {
        Self(None)
    }

    pub fn ready(&mut self, gap: Duration) -> bool {
        let now = Instant::now();
        if self.0.is_some_and(|t| now.duration_since(t) < gap) {
            return false;
        }
        self.0 = Some(now);
        true
    }
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_has_a_consistent_header() {
        for s in [Sound::Notification, Sound::Screenshot] {
            let samples = s.samples();
            assert!(samples.iter().any(|v| v.abs() > 0.05), "{s:?} is not silent");
            assert!(samples.iter().all(|v| v.abs() <= 1.2), "{s:?} stays near full scale");
            let bytes = wav(&samples);
            assert_eq!(&bytes[..4], b"RIFF");
            assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len() - 8);
            assert_eq!(bytes.len(), 44 + samples.len() * 2);
        }
    }

    #[test]
    fn throttle_lets_one_through_per_gap() {
        let mut t = Throttle::new();
        assert!(t.ready(Duration::from_secs(60)));
        assert!(!t.ready(Duration::from_secs(60)));
        assert!(t.ready(Duration::ZERO));
    }
}
