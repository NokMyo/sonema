//! Allocation-free DSP used by both realtime playback and offline export.

use std::f32::consts::PI;

use sonema_core::{ChannelStrip, CompressorSettings, EqBandSettings, EqKind, db_to_gain};

#[derive(Debug, Clone, Copy)]
struct Coefficients {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Coefficients {
    const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn normalize(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        let reciprocal = if a0.abs() < 1.0e-12 { 1.0 } else { a0.recip() };
        Self {
            b0: b0 * reciprocal,
            b1: b1 * reciprocal,
            b2: b2 * reciprocal,
            a1: a1 * reciprocal,
            a2: a2 * reciprocal,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct FilterState {
    z1: f32,
    z2: f32,
}

#[derive(Debug, Clone)]
pub struct StereoBiquad {
    coefficients: Coefficients,
    state: [FilterState; 2],
}

impl Default for StereoBiquad {
    fn default() -> Self {
        Self {
            coefficients: Coefficients::IDENTITY,
            state: [FilterState::default(); 2],
        }
    }
}

impl StereoBiquad {
    pub fn identity(&mut self) {
        self.coefficients = Coefficients::IDENTITY;
    }

    pub fn reset(&mut self) {
        self.state = [FilterState::default(); 2];
    }

    pub fn set_high_pass(&mut self, sample_rate: f32, frequency: f32, q: f32) {
        let (sin, cos, alpha) = common(sample_rate, frequency, q);
        let b0 = (1.0 + cos) * 0.5;
        let b1 = -(1.0 + cos);
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha;
        let _ = sin;
        self.coefficients = Coefficients::normalize(b0, b1, b2, a0, a1, a2);
    }

    pub fn set_eq(&mut self, sample_rate: f32, settings: &EqBandSettings) {
        if !settings.enabled || settings.gain_db.abs() < 0.001 {
            self.identity();
            return;
        }
        let frequency = settings.frequency_hz.clamp(10.0, sample_rate * 0.49);
        let q = settings.q.clamp(0.1, 18.0);
        let omega = 2.0 * PI * frequency / sample_rate;
        let sin = omega.sin();
        let cos = omega.cos();
        let a = 10.0_f32.powf(settings.gain_db.clamp(-24.0, 24.0) / 40.0);

        self.coefficients = match settings.kind {
            EqKind::Peak => {
                let alpha = sin / (2.0 * q);
                Coefficients::normalize(
                    1.0 + alpha * a,
                    -2.0 * cos,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cos,
                    1.0 - alpha / a,
                )
            }
            EqKind::LowShelf => shelf_coefficients(false, sin, cos, a),
            EqKind::HighShelf => shelf_coefficients(true, sin, cos, a),
        };
    }

    #[inline]
    pub fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        [
            self.process_channel(0, frame[0]),
            self.process_channel(1, frame[1]),
        ]
    }

    #[inline]
    fn process_channel(&mut self, channel: usize, input: f32) -> f32 {
        let c = self.coefficients;
        let state = &mut self.state[channel];
        let output = c.b0.mul_add(input, state.z1);
        state.z1 = c.b1.mul_add(input, state.z2 - c.a1 * output);
        state.z2 = c.b2.mul_add(input, -c.a2 * output);
        if output.is_finite() { output } else { 0.0 }
    }
}

#[derive(Debug, Clone)]
pub struct StereoCompressor {
    enabled: bool,
    threshold_db: f32,
    ratio: f32,
    attack_coefficient: f32,
    release_coefficient: f32,
    makeup_gain: f32,
    envelope: f32,
    reduction_db: f32,
}

impl StereoCompressor {
    pub fn new(sample_rate: f32, settings: &CompressorSettings) -> Self {
        let mut value = Self {
            enabled: false,
            threshold_db: -18.0,
            ratio: 3.0,
            attack_coefficient: 0.0,
            release_coefficient: 0.0,
            makeup_gain: 1.0,
            envelope: 0.0,
            reduction_db: 0.0,
        };
        value.configure(sample_rate, settings);
        value
    }

    pub fn configure(&mut self, sample_rate: f32, settings: &CompressorSettings) {
        self.enabled = settings.enabled;
        self.threshold_db = settings.threshold_db.clamp(-72.0, 0.0);
        self.ratio = settings.ratio.clamp(1.0, 30.0);
        self.attack_coefficient =
            time_coefficient(sample_rate, settings.attack_ms.clamp(0.05, 500.0));
        self.release_coefficient =
            time_coefficient(sample_rate, settings.release_ms.clamp(2.0, 5_000.0));
        self.makeup_gain = db_to_gain(settings.makeup_db.clamp(-12.0, 24.0));
    }

    #[inline]
    pub fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        if !self.enabled {
            self.reduction_db = 0.0;
            return frame;
        }
        let level = frame[0].abs().max(frame[1].abs());
        let coefficient = if level > self.envelope {
            self.attack_coefficient
        } else {
            self.release_coefficient
        };
        self.envelope = coefficient.mul_add(self.envelope, (1.0 - coefficient) * level);
        let input_db = 20.0 * self.envelope.max(1.0e-9).log10();
        let over_db = (input_db - self.threshold_db).max(0.0);
        self.reduction_db = -(over_db - over_db / self.ratio);
        let gain = db_to_gain(self.reduction_db) * self.makeup_gain;
        [frame[0] * gain, frame[1] * gain]
    }

    pub fn reduction_db(&self) -> f32 {
        self.reduction_db
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.reduction_db = 0.0;
    }
}

#[derive(Debug, Clone)]
pub struct ChannelStripProcessor {
    high_pass: StereoBiquad,
    low_eq: StereoBiquad,
    mid_eq: StereoBiquad,
    high_eq: StereoBiquad,
    compressor: StereoCompressor,
}

impl ChannelStripProcessor {
    pub fn new(sample_rate: f32, settings: &ChannelStrip) -> Self {
        let mut value = Self {
            high_pass: StereoBiquad::default(),
            low_eq: StereoBiquad::default(),
            mid_eq: StereoBiquad::default(),
            high_eq: StereoBiquad::default(),
            compressor: StereoCompressor::new(sample_rate, &settings.compressor),
        };
        value.configure(sample_rate, settings);
        value
    }

    pub fn configure(&mut self, sample_rate: f32, settings: &ChannelStrip) {
        if settings.high_pass.enabled {
            self.high_pass.set_high_pass(
                sample_rate,
                settings.high_pass.frequency_hz.clamp(10.0, 2_000.0),
                0.707,
            );
        } else {
            self.high_pass.identity();
        }
        self.low_eq.set_eq(sample_rate, &settings.low_eq);
        self.mid_eq.set_eq(sample_rate, &settings.mid_eq);
        self.high_eq.set_eq(sample_rate, &settings.high_eq);
        self.compressor.configure(sample_rate, &settings.compressor);
    }

    #[inline]
    pub fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        let frame = self.high_pass.process(frame);
        let frame = self.low_eq.process(frame);
        let frame = self.mid_eq.process(frame);
        let frame = self.high_eq.process(frame);
        self.compressor.process(frame)
    }

    pub fn reduction_db(&self) -> f32 {
        self.compressor.reduction_db()
    }

    pub fn reset(&mut self) {
        self.high_pass.reset();
        self.low_eq.reset();
        self.mid_eq.reset();
        self.high_eq.reset();
        self.compressor.reset();
    }
}

#[derive(Debug, Clone)]
pub struct SafetyLimiter {
    ceiling: f32,
    gain: f32,
    release_coefficient: f32,
}

impl SafetyLimiter {
    pub fn new(sample_rate: f32, ceiling_db: f32) -> Self {
        Self {
            ceiling: db_to_gain(ceiling_db.clamp(-12.0, 0.0)),
            gain: 1.0,
            release_coefficient: time_coefficient(sample_rate, 80.0),
        }
    }

    #[inline]
    pub fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        let peak = frame[0].abs().max(frame[1].abs());
        let target = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        if target < self.gain {
            self.gain = target;
        } else {
            self.gain = self
                .release_coefficient
                .mul_add(self.gain, 1.0 - self.release_coefficient);
        }
        [frame[0] * self.gain, frame[1] * self.gain]
    }

    pub fn reset(&mut self) {
        self.gain = 1.0;
    }
}

fn common(sample_rate: f32, frequency: f32, q: f32) -> (f32, f32, f32) {
    let omega = 2.0 * PI * frequency.clamp(10.0, sample_rate * 0.49) / sample_rate.max(1.0);
    let sin = omega.sin();
    let cos = omega.cos();
    let alpha = sin / (2.0 * q.clamp(0.1, 18.0));
    (sin, cos, alpha)
}

fn shelf_coefficients(high: bool, sin: f32, cos: f32, a: f32) -> Coefficients {
    // RBJ shelf with slope S=1.
    let alpha = sin / 2.0 * 2.0_f32.sqrt();
    let beta = 2.0 * a.sqrt() * alpha;
    let ap1 = a + 1.0;
    let am1 = a - 1.0;
    if high {
        Coefficients::normalize(
            a * (ap1 + am1 * cos + beta),
            -2.0 * a * (am1 + ap1 * cos),
            a * (ap1 + am1 * cos - beta),
            ap1 - am1 * cos + beta,
            2.0 * (am1 - ap1 * cos),
            ap1 - am1 * cos - beta,
        )
    } else {
        Coefficients::normalize(
            a * (ap1 - am1 * cos + beta),
            2.0 * a * (am1 - ap1 * cos),
            a * (ap1 - am1 * cos - beta),
            ap1 + am1 * cos + beta,
            -2.0 * (am1 + ap1 * cos),
            ap1 + am1 * cos - beta,
        )
    }
}

fn time_coefficient(sample_rate: f32, milliseconds: f32) -> f32 {
    (-1.0 / (sample_rate.max(1.0) * milliseconds.max(0.001) * 0.001)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_strip_is_transparent() {
        let mut strip = ChannelStripProcessor::new(48_000.0, &ChannelStrip::default());
        let output = strip.process([0.25, -0.5]);
        assert!((output[0] - 0.25).abs() < 1.0e-6);
        assert!((output[1] + 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn compressor_reduces_sustained_peak() {
        let settings = CompressorSettings {
            enabled: true,
            threshold_db: -20.0,
            ratio: 10.0,
            attack_ms: 0.1,
            release_ms: 100.0,
            makeup_db: 0.0,
        };
        let mut compressor = StereoCompressor::new(48_000.0, &settings);
        let mut output = [0.0; 2];
        for _ in 0..4_800 {
            output = compressor.process([1.0, 1.0]);
        }
        assert!(output[0] < 0.2);
        assert!(compressor.reduction_db() < -10.0);
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let mut limiter = SafetyLimiter::new(48_000.0, -1.0);
        let output = limiter.process([2.0, -1.5]);
        let ceiling = db_to_gain(-1.0);
        assert!(output[0].abs() <= ceiling + 1.0e-6);
        assert!(output[1].abs() <= ceiling + 1.0e-6);
    }
}
