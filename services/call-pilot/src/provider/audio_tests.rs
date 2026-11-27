//! The dense reference is the pre-optimization production 97-tap resampler.
//! Keep it independent of the polyphase bank: byte equality is the acceptance
//! boundary, including integer quantization and state across 20ms frames.
use super::Upsample;
use crate::engine::ProviderError;

struct DenseReference {
    history: [f64; 97],
    coefficients: [f64; 97],
    cursor: usize,
    phase: usize,
}
impl DenseReference {
    fn new() -> Self {
        let mut coefficients = [0.0; 97];
        for (i, c) in coefficients.iter_mut().enumerate() {
            let x = i as f64 - 48.0;
            let cutoff = 7_200.0 / 48_000.0;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / 96.0).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / 96.0).cos();
            *c = sinc * w;
        }
        let gain = 3.0 / coefficients.iter().sum::<f64>();
        for c in &mut coefficients {
            *c *= gain;
        }
        Self {
            history: [0.0; 97],
            coefficients,
            cursor: 0,
            phase: 0,
        }
    }
    fn frame(&mut self, input: &[u8]) -> Vec<u8> {
        assert_eq!(input.len(), 640);
        let mut output = Vec::with_capacity(960);
        for bytes in input.chunks_exact(2) {
            let sample = i16::from_le_bytes([bytes[0], bytes[1]]) as f64;
            for up in 0..3 {
                self.history[self.cursor] = if up == 0 { sample } else { 0.0 };
                if self.phase == 0 {
                    let value: f64 = self
                        .coefficients
                        .iter()
                        .enumerate()
                        .map(|(i, c)| c * self.history[(self.cursor + 97 - i) % 97])
                        .sum();
                    let value = value.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                    output.extend_from_slice(&value.to_le_bytes());
                }
                self.phase ^= 1;
                self.cursor = (self.cursor + 1) % 97;
            }
        }
        output
    }
}

fn frame(samples: impl Iterator<Item = i16>) -> Vec<u8> {
    samples.take(320).flat_map(i16::to_le_bytes).collect()
}
fn assert_equivalent(frames: impl Iterator<Item = Vec<u8>>) {
    let mut dense = DenseReference::new();
    let mut optimized = Upsample::new();
    for (index, input) in frames.enumerate() {
        let expected = dense.frame(&input);
        let actual = optimized.frame(&input).unwrap();
        assert_eq!(actual.len(), 960);
        assert_eq!(actual, expected, "different quantized PCM at frame {index}");
    }
}

#[test]
fn polyphase_is_byte_identical_for_all_pcm16_values_and_repeated_ring_wraps() {
    // Every possible input value, plus a full-scale discontinuity at the end.
    let values: Vec<_> = (i16::MIN as i32..=i16::MAX as i32)
        .map(|sample| sample as i16)
        .chain(std::iter::repeat_n(i16::MIN, 64))
        .collect();
    assert_eq!(values.len() % 320, 0);
    assert_equivalent(
        values
            .chunks_exact(320)
            .map(|chunk| frame(chunk.iter().copied())),
    );
}

#[test]
fn polyphase_preserves_silence_impulses_saturation_and_phase_at_frame_edges() {
    for sample in [i16::MIN, -1, 0, 1, i16::MAX] {
        assert_equivalent((0..4).map(|_| frame(std::iter::repeat(sample))));
    }
    for position in [0, 1, 2, 31, 32, 33, 318, 319, 320, 321, 639, 640] {
        for amplitude in [i16::MIN, i16::MAX] {
            assert_equivalent((0..4).map(|part| {
                frame((0..320).map(|offset| {
                    if part * 320 + offset == position {
                        amplitude
                    } else {
                        0
                    }
                }))
            }));
        }
    }
    assert_equivalent((0..10).map(|part| {
        frame((0..320).map(|offset| {
            if (part * 320 + offset) % 2 == 0 {
                i16::MIN
            } else {
                i16::MAX
            }
        }))
    }));
}

#[test]
fn polyphase_random_stream_and_new_utterance_resets_preserve_pcm_identity() {
    let mut seed = 0xe82e_0c64_034e_2159_u64;
    for utterance in 0..24 {
        assert_equivalent((0..utterance + 1).map(|_| {
            frame((0..320).map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as i16
            }))
        }));
    }
}

#[test]
fn invalid_frame_lengths_never_advance_history_or_phase() {
    let mut control = Upsample::new();
    let mut actual = Upsample::new();
    let prefix = frame((0..320).map(|i| (i * 71 - 15000) as i16));
    assert_eq!(
        actual.frame(&prefix).unwrap(),
        control.frame(&prefix).unwrap()
    );
    for length in [0, 1, 2, 319, 320, 638, 639, 641, 642, 960, 1280] {
        assert!(matches!(
            actual.frame(&vec![0; length]),
            Err(ProviderError::Protocol)
        ));
    }
    for _ in 0..4 {
        assert_eq!(
            actual.frame(&[0; 640]).unwrap(),
            control.frame(&[0; 640]).unwrap()
        );
    }
}

#[test]
fn coefficient_bank_is_shared_without_sharing_input_or_utterance_history() {
    let mut a = Upsample::new();
    let mut b = Upsample::new();
    assert!(std::ptr::eq(a.coefficients, b.coefficients));
    assert!(std::mem::size_of::<Upsample>() <= 296);
    a.frame(&frame(std::iter::repeat(i16::MAX))).unwrap();
    assert_eq!(b.frame(&[0; 640]).unwrap(), [0; 960]);
    let dense = DenseReference::new();
    for (index, expected) in dense.coefficients.iter().enumerate() {
        assert_eq!(
            a.coefficients[index % 3][index / 3].to_bits(),
            expected.to_bits()
        );
    }
}

#[test]
fn impulse_delay_and_frame_output_count_are_unchanged() {
    let mut upsample = Upsample::new();
    let input = frame((0..320).map(|index| if index == 0 { 12000 } else { 0 }));
    let output = upsample.frame(&input).unwrap();
    assert_eq!(output.len(), 960);
    let samples: Vec<_> = output
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    let peak = samples
        .iter()
        .enumerate()
        .max_by_key(|(_, sample)| sample.unsigned_abs())
        .unwrap()
        .0;
    assert_eq!(peak, 24, "97-tap filter retains its 1ms delay at 24kHz");
    assert!(
        upsample
            .frame(&[0; 640])
            .unwrap()
            .iter()
            .all(|byte| *byte == 0)
    );
}

fn tone(hz: f64) -> Vec<f64> {
    let mut upsample = Upsample::new();
    let mut output = Vec::new();
    for part in 0..51 {
        let input = frame((0..320).map(|offset| {
            (12000.0 * (std::f64::consts::TAU * hz * (part * 320 + offset) as f64 / 16000.0).sin())
                as i16
        }));
        output.extend(
            upsample
                .frame(&input)
                .unwrap()
                .chunks_exact(2)
                .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]) as f64),
        );
    }
    output[480..].to_vec()
}
fn amplitude(samples: &[f64], hz: f64) -> f64 {
    let (real, imaginary) =
        samples
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(real, imaginary), (i, sample)| {
                let phase = std::f64::consts::TAU * hz * i as f64 / 24000.0;
                (
                    real + sample * phase.cos(),
                    imaginary + sample * phase.sin(),
                )
            });
    2.0 * real.hypot(imaginary) / samples.len() as f64
}

#[test]
fn polyphase_retains_speech_band_and_suppresses_interpolation_images() {
    for hz in (300..=6000).step_by(300) {
        let output = tone(hz as f64);
        let gain_db = 20.0 * (amplitude(&output, hz as f64) / 12000.0).log10();
        assert!(gain_db.abs() < 0.05, "{hz}Hz gain: {gain_db}dB");
    }
    for hz in [300.0, 1000.0, 3000.0, 6000.0] {
        let output = tone(hz);
        let image_hz = if hz < 4000.0 {
            8000.0 + hz
        } else {
            16000.0 - hz
        };
        let rejection_db = 20.0 * (amplitude(&output, image_hz) / amplitude(&output, hz)).log10();
        assert!(rejection_db < -60.0, "{hz}Hz image: {rejection_db}dB");
    }
}

#[test]
#[ignore = "explicit optimized local CPU observation; no timing acceptance threshold"]
fn server_resampler_cpu_observation() {
    use std::{hint::black_box, time::Instant};
    let input = frame((0..320).map(|i| (12000.0 * (i as f64 * 0.29).sin()) as i16));
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut construct_before = Vec::new();
    let mut construct_after = Vec::new();
    black_box(Upsample::new()); // Warm the process-wide immutable bank once.
    for trial in 0..9 {
        let mut dense = DenseReference::new();
        let mut sparse = Upsample::new();
        let mut measure_dense = || {
            let start = Instant::now();
            for _ in 0..150 {
                black_box(dense.frame(black_box(&input)));
            }
            before.push(start.elapsed().as_secs_f64() * 1000.0 / 3.0);
        };
        let mut measure_sparse = || {
            let start = Instant::now();
            for _ in 0..150 {
                black_box(sparse.frame(black_box(&input)).unwrap());
            }
            after.push(start.elapsed().as_secs_f64() * 1000.0 / 3.0);
        };
        if trial % 2 == 0 {
            measure_dense();
            measure_sparse();
        } else {
            measure_sparse();
            measure_dense();
        }
        let start = Instant::now();
        for _ in 0..2048 {
            black_box(DenseReference::new());
        }
        construct_before.push(start.elapsed().as_secs_f64() * 1_000_000.0 / 2048.0);
        let start = Instant::now();
        for _ in 0..2048 {
            black_box(Upsample::new());
        }
        construct_after.push(start.elapsed().as_secs_f64() * 1_000_000.0 / 2048.0);
    }
    for values in [
        &mut before,
        &mut after,
        &mut construct_before,
        &mut construct_after,
    ] {
        values.sort_by(f64::total_cmp);
    }
    println!(
        "{{\"trials\":9,\"audio_seconds_per_trial\":3,\"before_median_ms_per_audio_second\":{},\"after_median_ms_per_audio_second\":{},\"before_median_reset_us\":{},\"after_median_reset_us\":{},\"dense_multiplications_per_frame\":46560,\"polyphase_multiplications_per_frame\":15520,\"unchanged_group_delay_ms\":1}}",
        before[4], after[4], construct_before[4], construct_after[4]
    );
    println!(
        "{{\"sorted_before_ms_per_audio_second\":{before:?},\"sorted_after_ms_per_audio_second\":{after:?},\"sorted_before_reset_us\":{construct_before:?},\"sorted_after_reset_us\":{construct_after:?}}}"
    );
}
