//! Frame and audio codecs.
//!
//! Video strategy: the first frame in a connection is a **keyframe** compressed
//! in full with LZ4. Every subsequent frame is a **delta frame**, produced by
//! XORing it byte-by-byte with the previous frame before LZ4 compression.
//! NES/Famicom frames contain large static regions, so the XOR output contains
//! long runs of zeros and typically compresses below 5 KiB per frame. TCP
//! provides ordered, reliable delivery, keeping the delta chain intact.
//!
//! Audio strategy: convert f32 samples to little-endian i16 PCM, halving
//! bandwidth without a perceptible quality loss for NES/Famicom audio.

use lz4_flex::{compress_prepend_size, decompress_size_prepended};

use crate::NetError;

/// Host-side frame encoder that retains the previous raw frame for delta encoding.
pub struct FrameEncoder {
    /// Previous raw frame; empty until the first keyframe has been emitted.
    prev: Vec<u8>,
    /// Reusable XOR scratch buffer that avoids a per-frame allocation.
    scratch: Vec<u8>,
}

impl FrameEncoder {
    /// Creates an encoder; the first call to `encode` emits a keyframe.
    pub fn new() -> Self {
        Self {
            prev: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Encodes a frame and returns `(is_keyframe, compressed_data)`.
    /// A frame-length change, which should not normally occur, forces a keyframe.
    pub fn encode(&mut self, frame: &[u8]) -> (bool, Vec<u8>) {
        if self.prev.len() != frame.len() {
            self.prev = frame.to_vec();
            return (true, compress_prepend_size(frame));
        }
        // Delta: previous frame XOR current frame.
        self.scratch.clear();
        self.scratch
            .extend(self.prev.iter().zip(frame).map(|(p, f)| p ^ f));
        let compressed = compress_prepend_size(&self.scratch);
        self.prev.copy_from_slice(frame);
        (false, compressed)
    }
}

impl Default for FrameEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Client-side frame decoder that restores delta frames in place.
pub struct FrameDecoder {
    /// Current reconstructed full frame.
    current: Vec<u8>,
}

impl FrameDecoder {
    /// Creates a decoder.
    pub fn new() -> Self {
        Self {
            current: Vec::new(),
        }
    }

    /// Decodes a frame and returns the reconstructed full-frame data.
    /// Receiving a delta frame before the first keyframe is a protocol error.
    pub fn decode(&mut self, keyframe: bool, data: &[u8]) -> Result<&[u8], NetError> {
        let raw = decompress_size_prepended(data)
            .map_err(|e| NetError::Protocol(format!("帧解压失败: {e}")))?;
        if keyframe {
            self.current = raw;
        } else {
            if raw.len() != self.current.len() {
                return Err(NetError::Protocol(
                    "差分帧长度不符（缺少关键帧？）".to_string(),
                ));
            }
            for (cur, delta) in self.current.iter_mut().zip(&raw) {
                *cur ^= delta;
            }
        }
        Ok(&self.current)
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Converts f32 audio samples in `-1.0..=1.0` to little-endian i16 PCM bytes.
pub fn f32_to_i16_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Converts little-endian i16 PCM bytes to f32 audio samples.
pub fn i16_bytes_to_f32(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(2)
        .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / f32::from(i16::MAX))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// A keyframe followed by delta frames stays identical at both endpoints.
    #[test]
    fn frame_roundtrip_keyframe_then_deltas() {
        let mut encoder = FrameEncoder::new();
        let mut decoder = FrameDecoder::new();

        // Simulate three frames with a static background and a few pixel changes.
        let mut frame = vec![0x40u8; 4096];
        let (kf, data) = encoder.encode(&frame);
        assert!(kf, "the first frame must be a keyframe");
        assert_eq!(decoder.decode(kf, &data).unwrap(), &frame[..]);

        for step in 0u8..3 {
            frame[100 + usize::from(step)] = 0xFF; // Change a few pixels per frame.
            let (kf, data) = encoder.encode(&frame);
            assert!(!kf, "subsequent frames must be delta frames");
            // Delta frames for static images should be far smaller than the original.
            assert!(
                data.len() < 256,
                "delta frame should compress well; actual size was {} bytes",
                data.len()
            );
            assert_eq!(decoder.decode(kf, &data).unwrap(), &frame[..]);
        }
    }

    /// A delta frame received before a keyframe must be rejected.
    #[test]
    fn delta_without_keyframe_is_rejected() {
        let mut encoder = FrameEncoder::new();
        let frame = vec![1u8; 1024];
        let _ = encoder.encode(&frame); // Discard the keyframe.
        let (kf, data) = encoder.encode(&frame); // Delta frame.
        assert!(!kf);

        let mut decoder = FrameDecoder::new();
        assert!(decoder.decode(kf, &data).is_err());
    }

    /// The f32-to-i16 PCM round trip stays within quantization precision.
    #[test]
    fn pcm_roundtrip() {
        let samples = vec![0.0f32, 0.5, -0.5, 1.0, -1.0, 0.123, 1.5, -1.5];
        let bytes = f32_to_i16_bytes(&samples);
        assert_eq!(bytes.len(), samples.len() * 2);
        let back = i16_bytes_to_f32(&bytes);
        for (orig, restored) in samples.iter().zip(&back) {
            let clamped = orig.clamp(-1.0, 1.0);
            assert!(
                (clamped - restored).abs() < 1.0 / 32000.0,
                "sample {orig} was restored as {restored}"
            );
        }
    }
}
