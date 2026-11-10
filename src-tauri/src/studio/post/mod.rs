//! Post-processing, loudness and the automatic quality check (milestone A3), ported from
//! the music-studio skill v3.7 `postprocess.py`. Pure Rust: no ffmpeg needed.

pub mod analyze;
pub mod audio;
pub mod ops;
