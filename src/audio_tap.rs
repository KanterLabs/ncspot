use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;

/// Samples kept for analysis. One FFT window at 44.1kHz is about 23ms of audio,
/// short enough to track a beat and long enough to resolve a bass note.
const WINDOW: usize = 1024;
/// Audio older than this counts as silence, so the bars settle when playback stops
/// or when a backend hands us nothing to look at.
const STALE_AFTER: Duration = Duration::from_millis(400);
/// Quietest level the bars still show, in decibels below full scale.
const FLOOR_DB: f32 = -62.0;
/// Bands cover this range in Hz. Below is rumble, above is mostly air.
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 15_000.0;
const SAMPLE_RATE: f32 = 44_100.0;

/// Where the beat detector stops listening. Kick drums and bass notes live below
/// this; everything above is what the rest of the mix is doing.
const BASS_HZ: f32 = 160.0;
/// How much louder than its own recent average the bass has to get to count as a
/// beat. Relative rather than absolute, so a quiet track still pulses.
const BEAT_SENSITIVITY: f32 = 1.5;
/// Bass this quiet is silence or hiss, whatever the recent average says.
const BEAT_FLOOR: f32 = 1e-5;
/// How fast the running average of bass energy forgets, roughly in seconds.
const ENERGY_MEMORY: f32 = 1.5;
/// The shortest gap between beats, which caps detection at about 270 BPM and stops
/// one kick being counted twice.
const REFRACTORY: Duration = Duration::from_millis(220);
/// How long a beat takes to fade out of the card.
const PULSE_DECAY: Duration = Duration::from_millis(180);

/// The most recent audio on its way to the speakers.
///
/// The playback sink is wrapped so that every packet is seen on its way through;
/// the now playing visualizer then reads the spectrum out of it. Analysis happens
/// on the drawing thread, so the audio path only pays for a memcpy.
#[derive(Default)]
pub struct AudioTap {
    samples: Mutex<Window>,
    /// When the last packet came through, if one ever has.
    last_packet: Mutex<Option<Instant>>,
    beat: Mutex<BeatDetector>,
}

/// Finds the beat in the bass, packet by packet.
///
/// Onsets are looked for on the audio thread rather than while drawing, because
/// packets arrive steadily and frames do not; a dropped frame would otherwise cost
/// a beat. The work is a filter and a sum over each packet, no transform.
#[derive(Default)]
struct BeatDetector {
    /// Carried between packets so the filter does not restart at every boundary.
    filtered: f32,
    /// The running average of recent bass energy, which a beat has to beat.
    average: f32,
    last_beat: Option<Instant>,
    strength: f32,
}

/// A fixed window of mono samples, overwritten oldest first.
struct Window {
    samples: [f32; WINDOW],
    next: usize,
    filled: bool,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            samples: [0.0; WINDOW],
            next: 0,
            filled: false,
        }
    }
}

impl AudioTap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Take a copy of a packet on its way to the speakers.
    ///
    /// Stereo is folded to mono, and only the tail of a long packet is kept: it is
    /// the part closest to what is being heard.
    pub fn push(&self, samples: &[f64]) {
        if samples.is_empty() {
            return;
        }
        let mut window = self.samples.lock().unwrap();
        // Two interleaved channels, averaged into one.
        for frame in samples.as_chunks::<2>().0 {
            let index = window.next;
            window.samples[index] = ((frame[0] + frame[1]) / 2.0) as f32;
            window.next = (index + 1) % WINDOW;
            if window.next == 0 {
                window.filled = true;
            }
        }
        drop(window);
        *self.last_packet.lock().unwrap() = Some(Instant::now());
        self.beat.lock().unwrap().feed(samples);
    }

    /// How hard the music is hitting right now, 0 to 1.
    ///
    /// Rises on a beat and falls away over the following fraction of a second, so
    /// drawing it straight onto a colour makes the card pulse in time.
    pub fn pulse(&self) -> f32 {
        let beat = self.beat.lock().unwrap();
        let Some(last) = beat.last_beat else {
            return 0.0;
        };
        let elapsed = last.elapsed().as_secs_f32() / PULSE_DECAY.as_secs_f32();
        (beat.strength * (-elapsed).exp()).clamp(0.0, 1.0)
    }

    /// Whether audio has arrived recently enough to be worth drawing.
    pub fn is_live(&self) -> bool {
        self.last_packet
            .lock()
            .unwrap()
            .is_some_and(|last| last.elapsed() < STALE_AFTER)
    }

    /// The current spectrum in `count` bands, each 0 to 1, or None when there is no
    /// recent audio to analyse.
    ///
    /// Bands are spaced logarithmically, the way pitch is heard, and levels are in
    /// decibels for the same reason.
    pub fn bands(&self, count: usize) -> Option<Vec<f32>> {
        if count == 0 || !self.is_live() {
            return None;
        }

        let samples = {
            let window = self.samples.lock().unwrap();
            if !window.filled {
                return None;
            }
            // Read out oldest first, so the window is one continuous stretch of audio.
            let mut ordered = Vec::with_capacity(WINDOW);
            ordered.extend_from_slice(&window.samples[window.next..]);
            ordered.extend_from_slice(&window.samples[..window.next]);
            ordered
        };

        let magnitudes = spectrum(&samples);
        let bin_hz = SAMPLE_RATE / WINDOW as f32;
        let mut bands = Vec::with_capacity(count);
        for band in 0..count {
            let edge = |index: usize| {
                let fraction = index as f32 / count as f32;
                LOW_HZ * (HIGH_HZ / LOW_HZ).powf(fraction)
            };
            let first = (edge(band) / bin_hz).floor().max(1.0) as usize;
            let last = ((edge(band + 1) / bin_hz).ceil() as usize).max(first + 1);

            // The loudest bin in the band, so a narrow tone is not averaged away.
            let peak = magnitudes
                .get(first..last.min(magnitudes.len()))
                .unwrap_or_default()
                .iter()
                .fold(0.0f32, |peak, &magnitude| peak.max(magnitude));
            let decibels = 20.0 * peak.max(1e-9).log10();
            bands.push(((decibels - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0));
        }
        Some(bands)
    }
}

impl BeatDetector {
    /// Take in a packet and decide whether a beat just landed in it.
    fn feed(&mut self, samples: &[f64]) {
        let frames = samples.as_chunks::<2>().0;
        if frames.is_empty() {
            return;
        }

        // A one pole low pass, so only the bass reaches the energy sum.
        let coefficient = 1.0 - (-std::f32::consts::TAU * BASS_HZ / SAMPLE_RATE).exp();
        let mut sum = 0.0;
        for frame in frames {
            let mono = ((frame[0] + frame[1]) / 2.0) as f32;
            self.filtered += coefficient * (mono - self.filtered);
            sum += self.filtered * self.filtered;
        }
        // Mean rather than total, so packet size does not change what a beat is.
        let energy = sum / frames.len() as f32;

        let beat = energy > BEAT_FLOOR
            && self.average > 0.0
            && energy > self.average * BEAT_SENSITIVITY
            && self
                .last_beat
                .is_none_or(|last| last.elapsed() >= REFRACTORY);
        if beat {
            // How far past the average it got, so a gentle beat pulses gently.
            self.strength =
                ((energy / self.average - BEAT_SENSITIVITY) / BEAT_SENSITIVITY).clamp(0.35, 1.0);
            self.last_beat = Some(Instant::now());
        }

        // The average follows the music, so loud and quiet passages both pulse.
        let seconds = frames.len() as f32 / SAMPLE_RATE;
        let weight = (seconds / ENERGY_MEMORY).clamp(0.0, 1.0);
        self.average += weight * (energy - self.average);
    }
}

/// Magnitudes of the first half of the spectrum of `samples`, Hann windowed.
fn spectrum(samples: &[f32]) -> Vec<f32> {
    let length = samples.len();
    let mut real: Vec<f32> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            // A Hann window, so the ends of the window do not ring across the
            // whole spectrum.
            let angle = std::f32::consts::TAU * index as f32 / length as f32;
            sample * 0.5 * (1.0 - angle.cos())
        })
        .collect();
    let mut imaginary = vec![0.0f32; length];
    fft(&mut real, &mut imaginary);

    let scale = 2.0 / length as f32;
    (0..length / 2)
        .map(|bin| (real[bin] * real[bin] + imaginary[bin] * imaginary[bin]).sqrt() * scale)
        .collect()
}

/// In-place radix 2 fast Fourier transform. `real` and `imaginary` must be the
/// same length, and that length must be a power of two.
fn fft(real: &mut [f32], imaginary: &mut [f32]) {
    let length = real.len();
    debug_assert!(length.is_power_of_two());

    // Reorder into bit reversed order, which is where the butterflies expect their
    // inputs to be.
    let mut target = 0usize;
    for source in 1..length {
        let mut bit = length >> 1;
        while target & bit != 0 {
            target ^= bit;
            bit >>= 1;
        }
        target |= bit;
        if source < target {
            real.swap(source, target);
            imaginary.swap(source, target);
        }
    }

    let mut span = 2;
    while span <= length {
        let angle = -std::f32::consts::TAU / span as f32;
        let (step_sin, step_cos) = angle.sin_cos();
        for start in (0..length).step_by(span) {
            let (mut twiddle_real, mut twiddle_imaginary) = (1.0f32, 0.0f32);
            for offset in 0..span / 2 {
                let (low, high) = (start + offset, start + offset + span / 2);
                let product_real = real[high] * twiddle_real - imaginary[high] * twiddle_imaginary;
                let product_imaginary =
                    real[high] * twiddle_imaginary + imaginary[high] * twiddle_real;

                real[high] = real[low] - product_real;
                imaginary[high] = imaginary[low] - product_imaginary;
                real[low] += product_real;
                imaginary[low] += product_imaginary;

                let next_real = twiddle_real * step_cos - twiddle_imaginary * step_sin;
                twiddle_imaginary = twiddle_real * step_sin + twiddle_imaginary * step_cos;
                twiddle_real = next_real;
            }
        }
        span <<= 1;
    }
}

/// The playback sink with a tap on it: packets are copied for analysis and then
/// handed on to the real sink untouched.
pub struct TapSink {
    inner: Box<dyn Sink>,
    tap: Arc<AudioTap>,
}

impl TapSink {
    pub fn new(inner: Box<dyn Sink>, tap: Arc<AudioTap>) -> Self {
        Self { inner, tap }
    }
}

impl Sink for TapSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        // Passthrough packets are compressed audio, which there is nothing to see in.
        if let Ok(samples) = packet.samples() {
            self.tap.push(samples);
        }
        self.inner.write(packet, converter)
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioTap, BeatDetector, HIGH_HZ, LOW_HZ, SAMPLE_RATE, WINDOW};
    use std::time::Duration;

    /// Interleaved stereo of a sine at `hz`, long enough to fill the window.
    fn tone(hz: f32, amplitude: f32) -> Vec<f64> {
        (0..WINDOW)
            .flat_map(|index| {
                let angle = std::f32::consts::TAU * hz * index as f32 / SAMPLE_RATE;
                let sample = (amplitude * angle.sin()) as f64;
                [sample, sample]
            })
            .collect()
    }

    /// Which band a frequency belongs to, for the same log spacing `bands` uses.
    fn band_of(hz: f32, count: usize) -> usize {
        let fraction = (hz / LOW_HZ).log10() / (HIGH_HZ / LOW_HZ).log10();
        ((fraction * count as f32) as usize).min(count - 1)
    }

    #[test]
    fn a_tone_lights_the_band_it_belongs_to() {
        let tap = AudioTap::new();
        tap.push(&tone(1000.0, 0.8));

        let bands = tap.bands(24).expect("audio just arrived");
        let expected = band_of(1000.0, 24);
        let loudest = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index)
            .unwrap();
        assert!(
            loudest.abs_diff(expected) <= 1,
            "a 1kHz tone lit band {loudest}, expected around {expected}: {bands:?}"
        );
    }

    #[test]
    fn a_louder_tone_reads_higher() {
        let quiet = AudioTap::new();
        quiet.push(&tone(1000.0, 0.05));
        let loud = AudioTap::new();
        loud.push(&tone(1000.0, 0.9));

        let band = band_of(1000.0, 24);
        let level = |tap: &AudioTap| tap.bands(24).unwrap()[band];
        assert!(level(&quiet) < level(&loud));
    }

    #[test]
    fn silence_reads_as_nothing() {
        let tap = AudioTap::new();
        tap.push(&vec![0.0; WINDOW * 2]);
        let bands = tap.bands(24).expect("packets did arrive");
        assert!(
            bands.iter().all(|level| *level < 0.05),
            "silence should sit on the floor: {bands:?}"
        );
    }

    #[test]
    fn nothing_is_reported_until_a_window_has_been_heard() {
        let tap = AudioTap::new();
        assert!(tap.bands(24).is_none(), "no audio has arrived yet");
        // A short packet leaves the window part filled, which is not enough to
        // analyse without reading the silence it was made with.
        tap.push(&tone(1000.0, 0.5)[..64]);
        assert!(tap.bands(24).is_none());
    }

    /// A detector that has heard `packets` of a tone at `amplitude`, with the gap
    /// between beats cleared each time so only the loudness test is in play.
    fn detector_fed(packets: usize, amplitude: f32) -> BeatDetector {
        let mut detector = BeatDetector::default();
        for _ in 0..packets {
            detector.last_beat = None;
            detector.feed(&tone(60.0, amplitude));
        }
        detector
    }

    #[test]
    fn a_kick_after_a_quiet_passage_registers() {
        let mut detector = detector_fed(40, 0.03);
        detector.last_beat = None;
        detector.feed(&tone(60.0, 0.9));

        assert!(detector.last_beat.is_some(), "a kick should read as a beat");
        assert!(
            detector.strength > 0.5,
            "a loud kick should pulse hard, got {}",
            detector.strength
        );
    }

    #[test]
    fn a_steady_tone_stops_registering_beats() {
        // The first moments of any sound are a jump from nothing, but once the
        // average has caught up an unchanging tone is not a beat any more.
        let mut detector = detector_fed(200, 0.5);
        detector.last_beat = None;
        detector.feed(&tone(60.0, 0.5));
        assert!(detector.last_beat.is_none());
    }

    #[test]
    fn what_happens_above_the_bass_is_not_a_beat() {
        // A loud hi-hat is not a kick: the low pass is what tells them apart.
        let mut detector = detector_fed(40, 0.03);
        detector.last_beat = None;
        detector.feed(&tone(8000.0, 0.9));
        assert!(detector.last_beat.is_none());
    }

    #[test]
    fn a_second_kick_too_soon_is_ignored() {
        let tap = AudioTap::new();
        for _ in 0..40 {
            tap.push(&tone(60.0, 0.03));
        }
        tap.push(&tone(60.0, 0.9));
        let first = tap.beat.lock().unwrap().last_beat;
        assert!(first.is_some());

        tap.push(&tone(60.0, 0.9));
        assert_eq!(
            tap.beat.lock().unwrap().last_beat,
            first,
            "one kick should not be counted twice"
        );
    }

    #[test]
    fn the_pulse_rises_on_a_beat_and_fades_after_it() {
        let tap = AudioTap::new();
        assert_eq!(tap.pulse(), 0.0, "nothing has been heard yet");

        for _ in 0..40 {
            tap.push(&tone(60.0, 0.03));
        }
        tap.push(&tone(60.0, 0.9));
        let immediate = tap.pulse();
        assert!(
            immediate > 0.4,
            "the beat should land hard, got {immediate}"
        );

        std::thread::sleep(Duration::from_millis(120));
        let later = tap.pulse();
        assert!(
            later < immediate * 0.8,
            "the pulse should be fading: {immediate} then {later}"
        );
    }
}
