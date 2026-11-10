//! Audio in and out, all in Rust (v3.7 piped everything through ffmpeg).
//!
//! Decoding gives stereo `f32` frames at the file's own rate; the ops and checks work on those,
//! and only `encode` turns them into a file, so chained steps never stack codec damage.

use std::fs::File;
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;

/// Stereo audio. Mono sources are duplicated, extra channels dropped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Audio {
    pub sr: u32,
    pub frames: Vec<[f32; 2]>,
}

impl Audio {
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn seconds(&self) -> f64 {
        self.frames.len() as f64 / self.sr as f64
    }

    pub fn interleaved(&self) -> Vec<f32> {
        self.frames.iter().flat_map(|f| *f).collect()
    }

    /// Per-frame channel mean, in `f32` like numpy's `x.mean(axis=1)` on float32 audio.
    pub fn mono(&self) -> Vec<f32> {
        self.frames.iter().map(|[l, r]| (l + r) / 2.0).collect()
    }
}

/// Export formats and their codec settings (v3.7 `CODECS`). Opus and AAC are not built in.
pub const FORMATS: [&str; 5] = ["wav", "wav24", "flac", "mp3", "ogg"];

pub fn extension(fmt: &str) -> &str {
    if fmt == "wav24" { "wav" } else { fmt }
}

pub fn decode(path: &Path) -> Result<Audio, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let bad = |e: &dyn std::fmt::Display| format!("could not decode {name}: {e}");
    let file = File::open(path).map_err(|e| bad(&e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| bad(&e))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or_else(|| bad(&"no audio track"))?;
    let track_id = track.id;
    let sr = track.codec_params.sample_rate.ok_or_else(|| bad(&"unknown sample rate"))?;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| bad(&e))?;
    let mut frames: Vec<[f32; 2]> = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(bad(&e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue, // one corrupt packet: skip it, like ffmpeg
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(bad(&e)),
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count().max(1);
        let b = buf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        if b.capacity() < decoded.capacity() * ch {
            *b = SampleBuffer::new(decoded.capacity() as u64, spec);
        }
        b.copy_interleaved_ref(decoded);
        frames.extend(b.samples().chunks_exact(ch).map(|s| [s[0], if ch > 1 { s[1] } else { s[0] }]));
    }
    Ok(Audio { sr, frames })
}

/// float -> integer PCM the way ffmpeg does it: scale, round, clip.
fn to_int(x: f32, bits: u32) -> i32 {
    let full = (1i64 << (bits - 1)) as f64;
    ((x as f64) * full).round().clamp(-full, full - 1.0) as i32
}

/// Title / comment tags (written for mp3 and ogg; wav and flac carry none).
#[derive(Debug, Clone, Default)]
pub struct Tags {
    pub title: String,
    pub comment: String,
}

pub fn encode(a: &Audio, path: &Path, fmt: &str, tags: &Tags) -> Result<(), String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let fail = |e: &dyn std::fmt::Display| format!("encode to {name} failed: {e}");
    if a.frames.iter().any(|f| !f[0].is_finite() || !f[1].is_finite()) {
        return Err(fail(&"non-finite samples"));
    }
    match fmt {
        "wav" | "wav24" => {
            let bits = if fmt == "wav" { 16 } else { 24 };
            let spec = hound::WavSpec { channels: 2, sample_rate: a.sr, bits_per_sample: bits, sample_format: hound::SampleFormat::Int };
            let mut w = hound::WavWriter::create(path, spec).map_err(|e| fail(&e))?;
            for s in a.frames.iter().flatten() {
                w.write_sample(to_int(*s, bits as u32)).map_err(|e| fail(&e))?;
            }
            w.finalize().map_err(|e| fail(&e))
        }
        "flac" => {
            use flacenc::component::BitRepr;
            use flacenc::error::Verify;
            let samples: Vec<i32> = a.frames.iter().flatten().map(|s| to_int(*s, 24)).collect();
            let config = flacenc::config::Encoder::default().into_verified().map_err(|(_, e)| fail(&e))?;
            let src = flacenc::source::MemSource::from_samples(&samples, 2, 24, a.sr as usize);
            let stream = flacenc::encode_with_fixed_block_size(&config, src, config.block_size).map_err(|e| fail(&e))?;
            let mut sink = flacenc::bitsink::ByteSink::new();
            stream.write(&mut sink).map_err(|e| fail(&e))?;
            let mut bytes = sink.as_slice().to_vec();
            // STREAMINFO tidy-up for strict readers: a fixed-blocksize stream states min = max block
            // size (flacenc puts the short last block in "min"), and frame sizes as 0 = unknown.
            if bytes.len() > 18 && &bytes[..4] == b"fLaC" {
                bytes.copy_within(10..12, 8);
                bytes[12..18].fill(0);
            }
            std::fs::write(path, bytes).map_err(|e| fail(&e))
        }
        "mp3" => {
            use mp3lame_encoder::{Builder, DualPcm, FlushNoGap, Id3Tag, Quality, VbrMode};
            let mut b = Builder::new().ok_or_else(|| fail(&"cannot start LAME"))?;
            b.set_num_channels(2).map_err(|e| fail(&e))?;
            b.set_sample_rate(a.sr).map_err(|e| fail(&e))?;
            b.set_vbr_mode(VbrMode::Mtrh).map_err(|e| fail(&e))?; // ffmpeg -q:a 2 = LAME -V2
            b.set_vbr_quality(Quality::NearBest).map_err(|e| fail(&e))?;
            b.set_id3_tag(Id3Tag {
                title: tags.title.as_bytes(),
                artist: &[],
                album: &[],
                album_art: &[],
                year: &[],
                comment: tags.comment.as_bytes(),
            })
            .map_err(|e| fail(&format!("{e:?}")))?;
            let mut enc = b.build().map_err(|e| fail(&e))?;
            let (left, right): (Vec<f32>, Vec<f32>) = a.frames.iter().map(|f| (f[0], f[1])).unzip();
            let mut out = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(left.len()));
            enc.encode_to_vec(DualPcm { left: &left, right: &right }, &mut out).map_err(|e| fail(&e))?;
            enc.flush_to_vec::<FlushNoGap>(&mut out).map_err(|e| fail(&e))?;
            std::fs::write(path, out).map_err(|e| fail(&e))
        }
        "ogg" => {
            use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};
            let file = File::create(path).map_err(|e| fail(&e))?;
            let sr = NonZeroU32::new(a.sr).ok_or_else(|| fail(&"sample rate 0"))?;
            let mut b = VorbisEncoderBuilder::new(sr, NonZeroU8::MIN.saturating_add(1), std::io::BufWriter::new(file))
                .map_err(|e| fail(&e))?;
            b.bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr { target_quality: 0.6 });
            b.comment_tags([("TITLE", tags.title.as_str()), ("COMMENT", tags.comment.as_str())]).map_err(|e| fail(&e))?;
            let mut enc = b.build().map_err(|e| fail(&e))?;
            for chunk in a.frames.chunks(4096) {
                let (l, r): (Vec<f32>, Vec<f32>) = chunk.iter().map(|f| (f[0], f[1])).unzip();
                enc.encode_audio_block([l, r]).map_err(|e| fail(&e))?;
            }
            enc.finish().map_err(|e| fail(&e))?;
            Ok(())
        }
        other => Err(format!("unknown export format '{other}'. Known: {}", FORMATS.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(sr: u32, secs: f64) -> Audio {
        let n = (sr as f64 * secs) as usize;
        let frames = (0..n)
            .map(|i| {
                let v = (0.4 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / sr as f64).sin()) as f32;
                [v, -v]
            })
            .collect();
        Audio { sr, frames }
    }

    #[test]
    fn every_format_round_trips_through_the_decoder() {
        let dir = std::env::temp_dir().join(format!("nusic-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = tone(48000, 2.0);
        for fmt in FORMATS {
            let path = dir.join(format!("t-{fmt}.{}", extension(fmt)));
            encode(&a, &path, fmt, &Tags { title: "測試".into(), comment: "c".into() }).unwrap();
            let b = decode(&path).unwrap();
            assert_eq!(b.sr, 48000, "{fmt}");
            // lossy codecs pad a little; lossless ones are exact length
            let slack = if matches!(fmt, "mp3" | "ogg") { 4096 } else { 0 };
            assert!(b.len().abs_diff(a.len()) <= slack, "{fmt}: {} vs {}", b.len(), a.len());
            let peak = b.frames.iter().map(|f| f[0].abs()).fold(0.0, f32::max);
            assert!((peak - 0.4).abs() < 0.03, "{fmt}: peak {peak}");
            if !matches!(fmt, "mp3" | "ogg") {
                let max_err = a.frames.iter().zip(&b.frames).map(|(x, y)| (x[0] - y[0]).abs().max((x[1] - y[1]).abs())).fold(0.0, f32::max);
                assert!(max_err < 1.0 / 30000.0, "{fmt}: error {max_err}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_format_is_refused() {
        let err = encode(&tone(48000, 0.1), Path::new("x.opus"), "opus", &Tags::default()).unwrap_err();
        assert!(err.contains("unknown export format 'opus'"), "{err}");
    }

    #[test]
    fn pcm_conversion_rounds_and_clips_like_ffmpeg() {
        assert_eq!(to_int(1.0, 16), 32767);
        assert_eq!(to_int(-1.0, 16), -32768);
        assert_eq!(to_int(0.5, 16), 16384);
        assert_eq!(to_int(2.0, 24), 8_388_607);
    }
}
