//! Deterministic, unvoiced PCM for exercising the actual detector and transport.
//! No microphone input, voice model, recorded speech, or external assets.
pub(crate) fn signal(frames: usize, variant: usize) -> Vec<u8> {
    // Vary pulse width, pitch and amplitude every few frames. The detector
    // sees a changing spectrum even when a test supplies several seconds.
    let mut pcm = Vec::with_capacity(frames * 640);
    let mut phase = 0;
    for frame in 0..frames {
        let pulse = (frame / 4 + variant) % 11;
        let frequency = 120 + 21 * pulse;
        let amplitude = 3_200 + 700 * ((frame / 3 + variant) % 8) as i16;
        for _ in 0..320 {
            phase = (phase + frequency) % 16_000;
            let sample = if frame % 13 < 2 {
                0
            } else if phase < 3_000 + 300 * pulse {
                amplitude
            } else {
                -amplitude / 2
            };
            pcm.extend(sample.to_le_bytes());
        }
    }
    pcm
}
