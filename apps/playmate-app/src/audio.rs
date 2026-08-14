//! Audio output: a cpal stream and a ring buffer shared with the emulation thread.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Ring-buffer capacity, approximately 250 ms at 48 kHz.
const RING_CAP: usize = 12_000;

/// Sample buffer shared by the emulation producer and audio callback consumer.
///
/// The consumer fills underruns with silence, while the producer drops the
/// oldest samples on overflow. This keeps the real-time audio thread
/// nonblocking; the worst result is a brief artifact.
pub struct AudioRing {
    /// Internal queue; both sides perform short batched operations under the lock.
    queue: Mutex<VecDeque<f32>>,
}

impl AudioRing {
    /// Creates an empty buffer.
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(RING_CAP)),
        }
    }

    /// Pushes one frame of samples, dropping the oldest samples if capacity is exceeded.
    pub fn push(&self, samples: &[f32]) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        queue.extend(samples.iter().copied());
        let excess = queue.len().saturating_sub(RING_CAP);
        if excess > 0 {
            queue.drain(..excess);
        }
    }

    /// Fills `out` with queued samples and uses silence for any underrun.
    /// `try_lock` ensures the real-time audio callback never blocks; lock
    /// contention produces one silent block instead.
    pub fn pop_into(&self, out: &mut [f32]) {
        let Ok(mut queue) = self.queue.try_lock() else {
            out.fill(0.0);
            return;
        };
        let available = queue.len().min(out.len());
        for slot in out.iter_mut().take(available) {
            *slot = queue.pop_front().unwrap_or(0.0);
        }
        out[available..].fill(0.0);
    }
}

/// Converts ring samples from a source rate to the device rate by linear
/// interpolation. Used when the device rejects the requested rate: shared-mode
/// WASAPI only accepts the device mix rate, unlike CoreAudio which converts.
struct LinearResampler {
    /// Source samples advanced per output sample (`src_rate / dst_rate`).
    step: f64,
    /// Fractional position between `prev` and `cur`, kept in `[0, 1)`.
    phase: f64,
    /// The two source samples straddling the current position.
    prev: f32,
    cur: f32,
    /// Batch buffer refilled from the ring once per callback.
    staging: Vec<f32>,
}

impl LinearResampler {
    fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            step: f64::from(src_rate) / f64::from(dst_rate),
            phase: 0.0,
            prev: 0.0,
            cur: 0.0,
            staging: Vec::new(),
        }
    }

    /// Fills `out` with device-rate samples, consuming source samples from `ring`.
    fn pull(&mut self, ring: &AudioRing, out: &mut [f32]) {
        // Exact number of source samples crossed while producing `out.len()` outputs.
        let need = (self.phase + out.len() as f64 * self.step) as usize;
        self.staging.resize(need, 0.0);
        ring.pop_into(&mut self.staging);
        let mut idx = 0;
        for slot in out.iter_mut() {
            *slot = self.prev + (self.cur - self.prev) * self.phase as f32;
            self.phase += self.step;
            while self.phase >= 1.0 {
                self.phase -= 1.0;
                self.prev = self.cur;
                // Float rounding can leave the batch one sample short; hold the
                // last sample for that sub-sample gap instead of over-popping.
                self.cur = self.staging.get(idx).copied().unwrap_or(self.cur);
                idx += 1;
            }
        }
    }
}

/// Opens the default audio output device and starts playback.
///
/// - `desired_rate`: when `Some`, requests the given rate to keep a netplay
///   client aligned with the host. If the device rejects it (shared-mode WASAPI
///   only accepts the device mix rate), the stream opens at the device default
///   rate and ring samples are resampled in the callback.
///
/// Returns the stream handle and the rate at which ring samples are consumed
/// (`desired_rate` when given, the device rate otherwise). The caller must
/// retain the handle because dropping it stops playback. Local and host modes
/// pass the returned rate to the emulation core to keep audio and video in sync.
pub fn start(ring: Arc<AudioRing>, desired_rate: Option<u32>) -> Result<(cpal::Stream, u32)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("找不到默认音频输出设备")?;
    let supported = device
        .default_output_config()
        .context("查询音频设备默认配置失败")?;
    // Keep the output path simple by supporting the default f32 format on macOS and Windows.
    anyhow::ensure!(
        supported.sample_format() == SampleFormat::F32,
        "暂不支持的音频采样格式: {:?}",
        supported.sample_format()
    );

    let channels = supported.channels() as usize;
    let default_config = supported.config();
    let device_rate = default_config.sample_rate;

    // Try the requested rate first so a matching device plays without conversion.
    // Depending on the backend, an unsupported rate can fail at build or at
    // play; either failure falls back to the device default rate below.
    if let Some(rate) = desired_rate
        && rate != device_rate
    {
        let mut config = default_config;
        config.sample_rate = rate;
        match build_stream(&device, config, channels, Arc::clone(&ring), None)
            .and_then(|stream| stream.play().map(|_| stream))
        {
            Ok(stream) => {
                log::info!("audio output ready: {rate} Hz, {channels} channels");
                return Ok((stream, rate));
            }
            Err(e) => log::warn!(
                "audio device rejected {rate} Hz, falling back to {device_rate} Hz with resampling: {e}"
            ),
        }
    }

    // Device default rate, resampling when the ring is fed at a different rate.
    let ring_rate = desired_rate.unwrap_or(device_rate);
    let resampler =
        (ring_rate != device_rate).then(|| LinearResampler::new(ring_rate, device_rate));
    let stream = build_stream(&device, default_config, channels, ring, resampler)
        .context("创建音频输出流失败")?;
    stream.play().context("启动音频输出流失败")?;
    log::info!("audio output ready: {device_rate} Hz, {channels} channels (source {ring_rate} Hz)");
    Ok((stream, ring_rate))
}

/// Builds an f32 output stream that fans mono ring samples out to every
/// interleaved channel, resampling when the ring rate differs from the device rate.
fn build_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    ring: Arc<AudioRing>,
    mut resampler: Option<LinearResampler>,
) -> Result<cpal::Stream, cpal::Error> {
    // Reuse a mono staging buffer to avoid allocations in the real-time callback.
    let mut mono: Vec<f32> = Vec::new();
    device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // NES audio is mono; pull once, then copy into every interleaved channel.
            let frames = data.len() / channels.max(1);
            mono.resize(frames, 0.0);
            match resampler.as_mut() {
                Some(r) => r.pull(&ring, &mut mono),
                None => ring.pop_into(&mut mono),
            }
            for (frame, &sample) in data.chunks_exact_mut(channels).zip(mono.iter()) {
                frame.fill(sample);
            }
        },
        |e| log::error!("audio stream error: {e}"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ring buffer fills underruns with silence and drops the oldest overflow.
    #[test]
    fn ring_underrun_and_overflow() {
        let ring = AudioRing::new();

        // Underrun: requesting four samples with only two queued yields two silent samples.
        ring.push(&[0.1, 0.2]);
        let mut out = [9.0f32; 4];
        ring.pop_into(&mut out);
        assert_eq!(out, [0.1, 0.2, 0.0, 0.0]);

        // Overflow: the oldest samples are dropped and the queue remains within capacity.
        let big = vec![0.5f32; RING_CAP + 100];
        ring.push(&big);
        let mut out = [0.0f32; 8];
        ring.pop_into(&mut out);
        assert_eq!(out, [0.5; 8]);
    }

    /// 2:1 downsampling settles into emitting every other source sample.
    #[test]
    fn resampler_downsamples_by_two() {
        let ring = AudioRing::new();
        let mut rs = LinearResampler::new(96_000, 48_000);
        ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);

        let mut out = [0.0f32; 3];
        rs.pull(&ring, &mut out);
        // The first output reflects the silent startup state (prev = cur = 0).
        assert_eq!(out, [0.0, 1.0, 3.0]);

        let mut out = [0.0f32; 2];
        rs.pull(&ring, &mut out);
        assert_eq!(out, [5.0, 7.0]);
    }

    /// 1:2 upsampling interpolates midpoints between source samples.
    #[test]
    fn resampler_upsamples_by_two() {
        let ring = AudioRing::new();
        let mut rs = LinearResampler::new(24_000, 48_000);
        ring.push(&[10.0, 20.0, 30.0]);

        let mut out = [0.0f32; 6];
        rs.pull(&ring, &mut out);
        // Two outputs of startup latency, then interpolated values follow.
        assert_eq!(out, [0.0, 0.0, 0.0, 5.0, 10.0, 15.0]);
    }
}
