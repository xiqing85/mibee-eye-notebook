//! Hardware capability probing and encoder-backend selection.
//!
//! This module answers two questions that drive adaptive behaviour across the
//! rest of the stack:
//!
//! 1. **What can this machine encode with?** — software (OpenH264), Intel/AMD
//!    VAAPI, or NVIDIA NVENC. The answer determines the
//!    [`EncoderBackend`] used by [`crate::capture_source`].
//! 2. **What is this CPU/GPU capable of?** — core count, SIMD features, GPU
//!    vendor. The answer calibrates quality presets and resolution ceilings
//!    surfaced to the Web UI as "recommended" profiles.
//!
//! # Platform scope
//!
//! All probing is Linux-only and best-effort: any failure (missing file,
//! absent library, permission denied) simply reports the capability as
//! unavailable rather than panicking. The module is pure Rust by default;
//! VAAPI/NVENC probing is gated behind the `vaapi` / `nvenc` cargo features so
//! a default build carries zero extra system dependencies.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

// ---------------------------------------------------------------------------
// Public capability model
// ---------------------------------------------------------------------------

/// Snapshot of everything probed about the host hardware + available encoder
/// backends. Serialised directly by the `GET /api/capabilities` endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct SystemCapabilities {
    /// CPU model name (e.g. "Intel(R) Core(TM) i5-6200U ...").
    pub cpu_model: String,
    /// Logical CPU core count.
    pub cpu_cores: usize,
    /// Physical CPU core count (best-effort; falls back to logical count).
    pub cpu_physical_cores: usize,
    /// Detected x86 SIMD instruction sets available to compiled code paths.
    pub simd: Vec<&'static str>,
    /// Approximate total system memory in MiB (0 if undeterminable).
    pub memory_mib: u64,
    /// GPU encoder backends detected on this host, in preference order
    /// (most capable first). Always contains at least [`EncoderBackend::Software`].
    pub encoder_backends: Vec<EncoderBackend>,
    /// GPUs discovered via `/dev/dri/renderD*` (VAAPI) and/or NVIDIA devices.
    /// Empty when no usable GPU is present.
    pub gpus: Vec<GpuInfo>,
    /// The backend recommended for new streams, chosen from
    /// `encoder_backends` based on availability + CPU strength.
    pub recommended_encoder: EncoderBackend,
    /// Suggested quality preset derived from CPU/GPU strength. The Web UI
    /// presents this as the default and lets the user override it.
    pub recommended_quality: QualityPreset,
}

/// A hardware encoder backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderBackend {
    /// Pure-software OpenH264. Always available; the universal fallback.
    Software,
    /// Intel Quick Sync or AMD VCN via libva (`/dev/dri/renderD*`).
    Vaapi,
    /// NVIDIA NVENC via the proprietary Codec SDK.
    Nvenc,
}

impl EncoderBackend {
    /// Human-readable label for the Web UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Software => "Software (OpenH264)",
            Self::Vaapi => "Hardware: VAAPI (Intel/AMD)",
            Self::Nvenc => "Hardware: NVENC (NVIDIA)",
        }
    }
}

/// Discovered GPU descriptor.
#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    /// Best-effort vendor classification.
    pub vendor: GpuVendor,
    /// Render node path, e.g. `/dev/dri/renderD128`.
    pub render_node: String,
    /// Verbose device description if it could be determined.
    pub description: Option<String>,
}

/// Coarse GPU vendor bucket, used to pick the encoder backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Intel,
    Amd,
    Nvidia,
    Unknown,
}

/// Encoder quality preset. Mapped to OpenH264 `complexity` / VAAPI target usage
/// by the encoding layer. Higher presets trade CPU for image quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QualityPreset {
    /// Fastest encode, lowest quality. For weak CPUs (≤4 cores, no GPU).
    UltraFast,
    /// Balanced. Default for capable CPUs.
    Medium,
    /// Best quality the software path can deliver.
    High,
    /// Hardware encoder at its quality ceiling.
    HardwareMax,
}

/// A concrete recommendation tying a backend to a quality level + resolution
/// ceiling. Used by the Web UI's encoding settings panel.
#[derive(Debug, Clone, Serialize)]
pub struct EncoderProfile {
    pub backend: EncoderBackend,
    pub quality: QualityPreset,
    /// Maximum resolution (on the long edge) this profile should drive.
    pub max_width: u32,
    pub max_height: u32,
    /// Human-readable rationale shown to the user.
    pub rationale: String,
}

// ---------------------------------------------------------------------------
// Top-level probe entry point
// ---------------------------------------------------------------------------

/// Probe the host's hardware and encoder-backends.
///
/// Best-effort: every individual probe degrades gracefully. The returned
/// [`SystemCapabilities`] always lists at least the software encoder.
pub fn probe() -> SystemCapabilities {
    let cpu = probe_cpu();
    let memory_mib = probe_memory_mib();
    let simd = probe_simd();
    let gpus = probe_gpus();

    // Build the encoder-backend preference list. Hardware backends are only
    // reported when their respective feature is enabled AND the device is
    // actually present.
    let mut backends: Vec<EncoderBackend> = Vec::new();
    let has_nvenc = cfg!(feature = "nvenc") && gpus.iter().any(|g| g.vendor == GpuVendor::Nvidia);
    let has_vaapi = cfg!(feature = "vaapi")
        && gpus
            .iter()
            .any(|g| matches!(g.vendor, GpuVendor::Intel | GpuVendor::Amd));
    if has_nvenc {
        backends.push(EncoderBackend::Nvenc);
    }
    if has_vaapi {
        backends.push(EncoderBackend::Vaapi);
    }
    backends.push(EncoderBackend::Software); // always available

    let recommended_encoder = backends[0];
    let recommended_quality = pick_quality_preset(recommended_encoder, &cpu, &simd);

    SystemCapabilities {
        cpu_model: cpu.model,
        cpu_cores: cpu.logical_cores,
        cpu_physical_cores: cpu.physical_cores,
        simd,
        memory_mib,
        encoder_backends: backends,
        gpus,
        recommended_encoder,
        recommended_quality,
    }
}

/// Derive suggested [`EncoderProfile`]s for the Web UI, given the host
/// capabilities. Returns one profile per resolution tier the host can plausibly
/// drive in real time.
pub fn recommended_profiles(caps: &SystemCapabilities) -> Vec<EncoderProfile> {
    let hw = caps.recommended_encoder != EncoderBackend::Software;
    let strong_cpu =
        caps.cpu_physical_cores >= 6 || caps.simd.iter().any(|s| *s == "avx2" || *s == "avx512f");

    let mut out = Vec::new();
    if hw {
        out.push(EncoderProfile {
            backend: caps.recommended_encoder,
            quality: QualityPreset::HardwareMax,
            max_width: 1920,
            max_height: 1080,
            rationale: "Hardware encoder available — 1080p at near-zero CPU".to_string(),
        });
    }
    let sw_quality = if strong_cpu {
        QualityPreset::High
    } else {
        QualityPreset::UltraFast
    };
    out.push(EncoderProfile {
        backend: EncoderBackend::Software,
        quality: sw_quality,
        max_width: 1280,
        max_height: 720,
        rationale: format!(
            "Software fallback{}",
            if strong_cpu {
                " — strong CPU"
            } else {
                " — weak CPU, prefer lower res"
            }
        ),
    });
    out
}

// ---------------------------------------------------------------------------
// CPU probing
// ---------------------------------------------------------------------------

struct CpuInfo {
    model: String,
    logical_cores: usize,
    physical_cores: usize,
}

fn probe_cpu() -> CpuInfo {
    let mut info = read_proc_cpuinfo().unwrap_or_else(|| CpuInfo {
        model: "unknown".to_string(),
        logical_cores: num_cpus_fallback(),
        physical_cores: num_cpus_fallback(),
    });
    if let Some(physical) = probe_physical_cores() {
        info.physical_cores = physical;
    }
    info
}

/// Parse `/proc/cpuinfo` for the model name and logical core count.
fn read_proc_cpuinfo() -> Option<CpuInfo> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut model: Option<String> = None;
    // Count logical processors by the number of `processor : N` entries.
    // Using `lines().starts_with` avoids the off-by-one that `matches("\n…")`
    // introduces when the file begins with `processor` (no leading newline).
    let cores = text
        .lines()
        .filter(|l| l.trim_start().starts_with("processor"))
        .count();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("model name") {
            if let Some(v) = v.split(':').nth(1) {
                if model.is_none() {
                    model = Some(v.trim().to_string());
                }
            }
        }
    }
    let cores = cores.max(1);
    Some(CpuInfo {
        model: model.unwrap_or_else(|| "unknown".to_string()),
        logical_cores: cores,
        physical_cores: cores,
    })
}

/// Estimate physical (not logical) cores by reading the unique
/// `(physical id, core id)` pairs from `/proc/cpuinfo`. Each unique pair is
/// one physical core; hyperthreading siblings share the same pair.
fn probe_physical_cores() -> Option<usize> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut cur_pkg: Option<String> = None;
    let mut cur_core: Option<String> = None;
    let mut flush = |pkg: &mut Option<String>, core: &mut Option<String>| {
        if let (Some(p), Some(c)) = (pkg.take(), core.take()) {
            seen.insert((p, c));
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush(&mut cur_pkg, &mut cur_core);
            continue;
        }
        if let Some(v) = strip_kv(trimmed, "physical id") {
            cur_pkg = Some(v);
        } else if let Some(v) = strip_kv(trimmed, "core id") {
            cur_core = Some(v);
        }
    }
    // Flush the final block if the file doesn't end with a blank line.
    flush(&mut cur_pkg, &mut cur_core);

    let n = seen.len();
    (n > 0).then_some(n)
}

/// Extract the value for `key` from a `/proc/cpuinfo`-style `key\t: value` line.
///
/// Handles the inconsistent whitespace the kernel emits between the key and
/// the colon (e.g. `physical id\t: 0` vs `core id\t\t: 0`).
fn strip_kv(line: &str, key: &str) -> Option<String> {
    let line = line.trim();
    let (k, v) = line.split_once(':')?;
    if k.trim() == key {
        Some(v.trim().to_string())
    } else {
        None
    }
}

/// Last-resort core count without external crates: count lines starting with
/// "processor" in `/proc/cpuinfo`, or fall back to 1.
fn num_cpus_fallback() -> usize {
    if let Ok(text) = fs::read_to_string("/proc/cpuinfo") {
        let n = text.lines().filter(|l| l.starts_with("processor")).count();
        if n > 0 {
            return n;
        }
    }
    1
}

// ---------------------------------------------------------------------------
// SIMD probing
// ---------------------------------------------------------------------------

/// Detect runtime x86 SIMD features usable by the encoding hot path. Returns
/// the most capable available set (e.g. `["avx2"]`). Empty on non-x86.
fn probe_simd() -> Vec<&'static str> {
    let mut out = Vec::new();
    // `is_x86_feature_detected!` is only available on x86; gate accordingly.
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected_impl("avx512f") {
            out.push("avx512f");
        } else if is_x86_feature_detected_impl("avx2") {
            out.push("avx2");
        } else if is_x86_feature_detected_impl("sse4.2") {
            out.push("sse4.2");
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // Non-x86 SIMD (e.g. NEON) — not auto-detected here; leave empty.
    }
    out
}

/// Thin wrapper so the call site compiles on non-x86 without cfg! noise.
#[cfg(target_arch = "x86_64")]
fn is_x86_feature_detected_impl(feature: &str) -> bool {
    match feature {
        "avx512f" => std::arch::is_x86_feature_detected!("avx512f"),
        "avx2" => std::arch::is_x86_feature_detected!("avx2"),
        "sse4.2" => std::arch::is_x86_feature_detected!("sse4.2"),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Memory probing
// ---------------------------------------------------------------------------

/// Read total system memory in MiB from `/proc/meminfo` (`MemTotal:`).
fn probe_memory_mib() -> u64 {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            // Format: "MemTotal:       16384000 kB"
            let kb: u64 = rest.trim().trim_end_matches(" kB").parse().unwrap_or(0);
            return kb / 1024;
        }
    }
    0
}

// ---------------------------------------------------------------------------
// GPU probing
// ---------------------------------------------------------------------------

/// Enumerate GPUs visible through DRI render nodes + (feature-gated) NVIDIA
/// device nodes. Pure-Rust by default: it walks sysfs to classify each
/// `/dev/dri/renderD*` device by PCI vendor — no libva link required.
fn probe_gpus() -> Vec<GpuInfo> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir("/dev/dri") {
        let mut nodes: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("renderD"))
                    .unwrap_or(false)
            })
            .collect();
        nodes.sort();
        for node in nodes {
            if let Some(info) = classify_dri_node(&node) {
                out.push(info);
            }
        }
    }
    // NVIDIA detection — feature-gated so a default build never references it.
    if cfg!(feature = "nvenc") {
        if Path::new("/dev/nvidia0").exists() {
            out.push(GpuInfo {
                vendor: GpuVendor::Nvidia,
                render_node: "/dev/nvidia0".to_string(),
                description: read_nvidia_description(),
            });
        }
    }
    out
}

/// Map a `/dev/dri/renderD*` device to its PCI vendor via sysfs. Returns
/// `None` if the sysfs walk fails (e.g. on non-PCI GPUs).
fn classify_dri_node(path: &Path) -> Option<GpuInfo> {
    let name = path.file_name()?.to_str()?.to_string();
    // Resolve the char device to its sysfs entry: walk
    // /sys/class/drm/<name>/device/vendor.
    let sys_vendor = Path::new("/sys/class/drm")
        .join(&name)
        .join("device/vendor");
    let raw = fs::read_to_string(&sys_vendor).ok()?;
    let raw = raw.trim();
    // PCI vendor IDs are stored as `0x8086` (Intel), `0x1002` (AMD), etc.
    let vendor = match raw {
        "0x8086" => GpuVendor::Intel,
        "0x1002" => GpuVendor::Amd,
        "0x10de" => GpuVendor::Nvidia,
        _ => GpuVendor::Unknown,
    };
    let description = read_device_description(&name);
    Some(GpuInfo {
        vendor,
        render_node: path.to_string_lossy().to_string(),
        description,
    })
}

/// Read the `uevent`/`device` description line for a DRM node, best-effort.
fn read_device_description(drm_name: &str) -> Option<String> {
    // Try the PCI subsystem_device which usually encodes the model.
    let subsystem = Path::new("/sys/class/drm")
        .join(drm_name)
        .join("device/subsystem_device");
    if let Ok(s) = fs::read_to_string(&subsystem) {
        let s = s.trim();
        if !s.is_empty() {
            return Some(format!("PCI subsystem {s}"));
        }
    }
    None
}

/// Best-effort NVIDIA GPU description (lspci not assumed available).
fn read_nvidia_description() -> Option<String> {
    fs::read_to_string("/proc/driver/nvidia/gpus/0/information")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("Model:").map(|s| s.trim().to_string()))
        })
}

// ---------------------------------------------------------------------------
// Selection heuristics
// ---------------------------------------------------------------------------

/// Choose a default [`QualityPreset`] given the chosen backend and CPU.
///
/// The thresholds are conservative surveillance-oriented heuristics: a 720p
/// stream needs to encode in real time with headroom for several cameras, so
/// we prefer speed on marginal hardware.
fn pick_quality_preset(
    backend: EncoderBackend,
    cpu: &CpuInfo,
    simd: &[&'static str],
) -> QualityPreset {
    match backend {
        EncoderBackend::Nvenc | EncoderBackend::Vaapi => QualityPreset::HardwareMax,
        EncoderBackend::Software => {
            let has_fast_simd = simd.iter().any(|s| *s == "avx2" || *s == "avx512f");
            match cpu.physical_cores {
                // ≤2 physical cores (e.g. old dual-cores) — keep it fastest.
                n if n <= 2 => QualityPreset::UltraFast,
                // 3–4 physical cores: capable enough for Medium, or High when
                // the CPU also has wide SIMD (modern quad-cores like i5-1135G7).
                n if n <= 4 => {
                    if has_fast_simd {
                        QualityPreset::High
                    } else {
                        QualityPreset::Medium
                    }
                }
                // 6+ physical cores — High.
                _ => QualityPreset::High,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_at_least_software_backend() {
        let caps = probe();
        assert!(
            caps.encoder_backends.contains(&EncoderBackend::Software),
            "software backend must always be available: {:?}",
            caps.encoder_backends
        );
        assert!(caps.cpu_cores >= 1);
        assert!(!caps.cpu_model.is_empty());
    }

    #[test]
    fn probe_runs_without_panic_on_linux() {
        // Smoke test: probing must never panic regardless of the host.
        let _ = probe();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpuinfo_model_nonempty_when_readable() {
        if fs::read_to_string("/proc/cpuinfo").is_err() {
            return; // not Linux-shaped host
        }
        let caps = probe();
        assert_ne!(caps.cpu_model, "unknown", "model should be parsed");
    }

    #[test]
    fn recommended_profiles_always_include_software_fallback() {
        let caps = probe();
        let profiles = recommended_profiles(&caps);
        assert!(
            profiles
                .iter()
                .any(|p| p.backend == EncoderBackend::Software),
            "a software fallback profile is always provided: {:?}",
            profiles
        );
    }

    #[test]
    fn strip_kv_handles_both_spacing_styles() {
        assert_eq!(strip_kv("physical id : 0", "physical id"), Some("0".into()));
        assert_eq!(strip_kv("physical id:0", "physical id"), Some("0".into()));
        assert_eq!(strip_kv("bogus", "physical id"), None);
    }

    #[test]
    fn encoder_backend_labels_are_distinct() {
        let labels = [
            EncoderBackend::Software.label(),
            EncoderBackend::Vaapi.label(),
            EncoderBackend::Nvenc.label(),
        ];
        let unique: BTreeSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "labels must be distinct");
    }
}
