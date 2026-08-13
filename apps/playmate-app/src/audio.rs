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

/// Opens the default audio output device and starts playback.
///
/// - `desired_rate`: requests a specific sample rate when `Some`, keeping a
///   netplay client aligned with the host. CoreAudio and shared-mode WASAPI
///   perform any required system-level conversion. `None` uses the device default.
///
/// Returns the stream handle and actual sample rate. The caller must retain the
/// handle because dropping it stops playback. Local and host modes pass the
/// actual rate back to the emulation core to keep audio and video synchronized.
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
    let mut config = supported.config();
    if let Some(rate) = desired_rate {
        config.sample_rate = rate;
    }
    let sample_rate = config.sample_rate;

    // Reuse a mono staging buffer to avoid allocations in the real-time callback.
    let mut mono: Vec<f32> = Vec::new();
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // NES audio is mono; pull once, then copy into every interleaved channel.
                let frames = data.len() / channels.max(1);
                mono.resize(frames, 0.0);
                ring.pop_into(&mut mono);
                for (frame, &sample) in data.chunks_exact_mut(channels).zip(mono.iter()) {
                    frame.fill(sample);
                }
            },
            |e| log::error!("audio stream error: {e}"),
            None,
        )
        .context("创建音频输出流失败")?;
    stream.play().context("启动音频输出流失败")?;
    log::info!("audio output ready: {sample_rate} Hz, {channels} channels");
    Ok((stream, sample_rate))
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
}
