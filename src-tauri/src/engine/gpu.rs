//! Live VRAM readout from `nvidia-smi`, independent of the worker, so the
//! numbers stay current before the first job and right after a cancel.

use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpuInfo {
    pub name: String,
    pub total_gb: f64,
    pub used_gb: f64,
}

/// Parse `nvidia-smi --query-gpu=name,memory.total,memory.used --format=csv,noheader,nounits`.
/// Returns the card with the most VRAM (generation runs on one device).
pub fn parse_nvidia_smi(out: &str) -> Option<GpuInfo> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.rsplitn(3, ',');
            let used: f64 = parts.next()?.trim().parse().ok()?;
            let total: f64 = parts.next()?.trim().parse().ok()?;
            let name = parts.next()?.trim().to_string();
            Some(GpuInfo { name, total_gb: round2(total / 1024.0), used_gb: round2(used / 1024.0) })
        })
        .max_by(|a, b| a.total_gb.total_cmp(&b.total_gb))
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// `None` when there is no NVIDIA driver or the query fails.
pub fn query() -> Option<GpuInfo> {
    let mut cmd = Command::new("nvidia-smi");
    cmd.args(["--query-gpu=name,memory.total,memory.used", "--format=csv,noheader,nounits"]);
    super::no_window(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_nvidia_smi(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_card_in_gb() {
        let got = parse_nvidia_smi("NVIDIA GeForce RTX 3050 Laptop GPU, 4096, 1536\n").unwrap();
        assert_eq!(got, GpuInfo { name: "NVIDIA GeForce RTX 3050 Laptop GPU".into(), total_gb: 4.0, used_gb: 1.5 });
    }

    #[test]
    fn picks_biggest_card_and_keeps_commas_in_names() {
        let got = parse_nvidia_smi("Card, A, 4096, 100\nBig Card, 12288, 512\n").unwrap();
        assert_eq!(got.name, "Big Card");
        assert_eq!(got.total_gb, 12.0);
        let only = parse_nvidia_smi("Card, A, 4096, 100").unwrap();
        assert_eq!(only.name, "Card, A");
    }

    #[test]
    fn none_for_garbage() {
        assert_eq!(parse_nvidia_smi(""), None);
        assert_eq!(parse_nvidia_smi("No devices were found"), None);
    }
}
