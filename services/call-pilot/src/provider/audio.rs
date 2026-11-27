//! Bounded PCM conversion. No device or media-file discovery.
use crate::engine::{PreparedAudio, ProviderError};
use std::sync::OnceLock;

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;

pub const TTS_RATE: u32 = 24_000;
pub const MAX_TTS_PCM: usize = 60 * TTS_RATE as usize * 2;

/// Causal 3/2 rational resampler: 97-tap Blackman-windowed low-pass at
/// 48 kHz, then decimation by two. State survives every input frame. Its
/// one millisecond group delay is retained, never represented as source timing.
/// Three polyphase rows omit the inserted zero samples without changing the
/// remaining multiply/add order. A reset clears only input history and phase;
/// the immutable coefficient bank is shared across routes and utterances.
pub(super) struct Upsample {
    history: [f64; 33],
    coefficients: &'static [[f64; 33]; 3],
    cursor: usize,
    phase: usize,
}
impl Upsample {
    pub fn new() -> Self {
        Self {
            history: [0.0; 33],
            coefficients: coefficients(),
            cursor: 0,
            phase: 0,
        }
    }
    pub fn frame(&mut self, input: &[u8]) -> Result<Vec<u8>, ProviderError> {
        if input.len() != 640 {
            return Err(ProviderError::Protocol);
        }
        let mut output = Vec::with_capacity(960);
        for bytes in input.chunks_exact(2) {
            let sample = i16::from_le_bytes([bytes[0], bytes[1]]) as f64;
            self.history[self.cursor] = sample;
            for up in 0..3 {
                if (self.phase + up).is_multiple_of(2) {
                    let history = self.history[..=self.cursor]
                        .iter()
                        .rev()
                        .chain(self.history[self.cursor + 1..].iter().rev());
                    let taps = if up == 0 { 33 } else { 32 };
                    let value: f64 = self.coefficients[up][..taps]
                        .iter()
                        .zip(history)
                        .map(|(coefficient, sample)| coefficient * sample)
                        .sum();
                    let value = value.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                    output.extend_from_slice(&value.to_le_bytes());
                }
            }
            self.phase ^= 1;
            self.cursor = (self.cursor + 1) % 33;
        }
        Ok(output)
    }
}

fn coefficients() -> &'static [[f64; 33]; 3] {
    static COEFFICIENTS: OnceLock<[[f64; 33]; 3]> = OnceLock::new();
    COEFFICIENTS.get_or_init(|| {
        let mut dense = [0.0; 97];
        for (i, coefficient) in dense.iter_mut().enumerate() {
            let x = i as f64 - 48.0;
            let cutoff = 7_200.0 / 48_000.0;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let window = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / 96.0).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / 96.0).cos();
            *coefficient = sinc * window;
        }
        let gain = 3.0 / dense.iter().sum::<f64>();
        let mut bank = [[0.0; 33]; 3];
        for (i, coefficient) in dense.iter().enumerate() {
            bank[i % 3][i / 3] = coefficient * gain;
        }
        bank
    })
}

pub(super) fn wav(pcm: Vec<u8>) -> Result<PreparedAudio, ProviderError> {
    if pcm.is_empty() || !pcm.len().is_multiple_of(2) || pcm.len() > MAX_TTS_PCM {
        return Err(ProviderError::Protocol);
    }
    let length = u32::try_from(pcm.len()).map_err(|_| ProviderError::Protocol)?;
    let mut wav = Vec::with_capacity(pcm.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(length + 36).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&TTS_RATE.to_le_bytes());
    wav.extend_from_slice(&(TTS_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&length.to_le_bytes());
    wav.extend_from_slice(&pcm);
    Ok(PreparedAudio {
        wav,
        duration_ms: (u64::from(length) * 1000).div_ceil(u64::from(TTS_RATE) * 2),
        sample_rate: TTS_RATE,
    })
}
