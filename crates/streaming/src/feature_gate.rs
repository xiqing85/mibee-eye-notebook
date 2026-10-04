//! Feature-level resource gating (SPEC appendix A #40).
//!
//! Small-memory hosts must not boot every AI feature: the LLM/VLM/voice
//! stacks together can outweigh the RAM a netbook-class deployment has.
//! At boot the planner estimates each optional feature's resident cost
//! from its model files on disk and admits features greedily in a fixed
//! priority order while the running sum fits the memory budget
//! (`MemAvailable − reserve`). Non-admitted features are disabled in the
//! config copy handed to the engines, so the existing fail-open
//! machinery reports them inactive; the decision table is exposed via
//! `capabilities.resource` (and `mibee_eye_feature_admitted` gauges).
//!
//! The decision is boot-time only: no runtime eviction. A restart
//! re-evaluates against the then-current memory watermark. Model files
//! that do not exist cost nothing here — the engine's own fail-open
//! reason ("model missing") still surfaces honestly.

use serde::Serialize;

/// Feature was admitted (empty reason).
pub const REASON_ON: &str = "";
/// Feature disabled in config — the gate has nothing to say.
pub const REASON_OFF_CONFIG: &str = "off_config";
/// Feature enabled but its cost does not fit the remaining budget.
pub const REASON_OFF_BUDGET: &str = "off_budget";
/// Feature's dependency (the voice stack) was not admitted.
pub const REASON_DEPENDENCY: &str = "dependency";

/// One feature's boot-admission decision (capabilities.resource entry).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FeatureDecision {
    pub name: &'static str,
    /// Estimated resident cost (model files × factor + engine overhead).
    pub cost_mib: u64,
    pub admitted: bool,
    /// Machine code — `""` / `off_config` / `off_budget` / `dependency`.
    pub reason: &'static str,
}

/// The boot-time admission snapshot served via `capabilities.resource`.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceProfile {
    /// `"auto"` (budget admission) or `"all"` (explicit full boot).
    pub mode: String,
    pub total_mib: u64,
    /// `MemAvailable` sampled at boot (MiB).
    pub available_mib: u64,
    /// `available − reserve` in auto mode; `available` in `all` mode.
    pub budget_mib: u64,
    pub reserve_mib: u64,
    /// In priority order (`ai → audio_ai → voice → llm → tts → decision
    /// → face → ocr → meeting → vlm`).
    pub features: Vec<FeatureDecision>,
}

impl ResourceProfile {
    /// The all-features-on placeholder used by test fixtures and the
    /// legacy `run()` path before a real profile exists.
    #[must_use]
    pub fn unrestricted() -> Self {
        Self {
            mode: "all".into(),
            total_mib: 0,
            available_mib: 0,
            budget_mib: 0,
            reserve_mib: 0,
            features: Vec::new(),
        }
    }
}

/// One candidate feature for the planner (pure input — no I/O).
#[derive(Debug, Clone)]
pub struct FeatureInput {
    pub name: &'static str,
    /// Master switch state from the config.
    pub enabled: bool,
    /// Model file paths (resolved relative to the working dir). Missing
    /// files contribute zero — the engine's own fail-open covers them.
    pub model_paths: Vec<String>,
    /// Fixed per-engine resident overhead (ORT arena, buffers, threads).
    pub overhead_mib: u64,
    /// Feature that must be admitted first (`meeting`/`decision` ride
    /// the voice models).
    pub depends_on: Option<&'static str>,
}

/// Estimated resident MiB for the given model files: the on-disk bytes
/// rounded up to MiB, scaled ×1.15 (ORT sessions keep weights resident;
/// llama.cpp mmap touches roughly the file over a conversation).
#[must_use]
pub fn file_cost_mib(paths: &[String]) -> u64 {
    let bytes: u64 = paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    let mib = bytes.div_ceil(1024 * 1024);
    (mib * 115).div_ceil(100)
}

/// Greedy priority-order admission (pure; file sizes are the only I/O,
/// via [`file_cost_mib`]).
///
/// `mode = "all"` admits every enabled feature (legacy behaviour);
/// anything else is treated as `"auto"`. Features disabled in config
/// are reported `off_config` and cost nothing. A feature whose
/// dependency was not admitted is reported `dependency` (and costs
/// nothing — it will not start). Budget overflow marks `off_budget`
/// but keeps scanning: cheaper lower-priority features may still fit.
#[must_use]
pub fn plan(
    inputs: &[FeatureInput],
    mode: &str,
    total_mib: u64,
    available_mib: u64,
    reserve_mib: u64,
) -> ResourceProfile {
    let all = mode.eq_ignore_ascii_case("all");
    let budget = if all {
        available_mib
    } else {
        available_mib.saturating_sub(reserve_mib)
    };
    let mut used = 0_u64;
    let mut decisions = Vec::with_capacity(inputs.len());
    for f in inputs {
        let cost = file_cost_mib(&f.model_paths) + f.overhead_mib;
        if !f.enabled {
            decisions.push(FeatureDecision {
                name: f.name,
                cost_mib: cost,
                admitted: false,
                reason: REASON_OFF_CONFIG,
            });
            continue;
        }
        if all {
            decisions.push(FeatureDecision {
                name: f.name,
                cost_mib: cost,
                admitted: true,
                reason: REASON_ON,
            });
            continue;
        }
        if let Some(dep) = f.depends_on
            && decisions.iter().any(|d| d.name == dep && !d.admitted)
        {
            decisions.push(FeatureDecision {
                name: f.name,
                cost_mib: cost,
                admitted: false,
                reason: REASON_DEPENDENCY,
            });
            continue;
        }
        let admitted = used.saturating_add(cost) <= budget;
        if admitted {
            used += cost;
        }
        decisions.push(FeatureDecision {
            name: f.name,
            cost_mib: cost,
            admitted,
            reason: if admitted {
                REASON_ON
            } else {
                REASON_OFF_BUDGET
            },
        });
    }
    ResourceProfile {
        mode: if all { "all".into() } else { "auto".into() },
        total_mib,
        available_mib,
        budget_mib: budget,
        reserve_mib,
        features: decisions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feat(name: &'static str, enabled: bool, mib: u64) -> FeatureInput {
        FeatureInput {
            name,
            enabled,
            model_paths: vec![format!("/nonexistent/{name}.onnx")],
            overhead_mib: mib,
            depends_on: None,
        }
    }

    #[test]
    fn everything_fits_when_budget_generous() {
        let p = plan(
            &[feat("ai", true, 10), feat("llm", true, 500)],
            "auto",
            8192,
            7000,
            768,
        );
        assert!(p.features.iter().all(|d| d.admitted));
        assert_eq!(p.budget_mib, 7000 - 768);
        assert_eq!(p.mode, "auto");
    }

    #[test]
    fn overflow_cuts_the_heavy_tail_not_the_head() {
        // Budget 100: ai(10)+voice(30) fit, llm(500) cut, face(20) still fits.
        let p = plan(
            &[
                feat("ai", true, 10),
                feat("voice", true, 30),
                feat("llm", true, 500),
                feat("face", true, 20),
            ],
            "auto",
            4096,
            868,
            768,
        );
        let by = |n: &str| p.features.iter().find(|d| d.name == n).unwrap();
        assert!(by("ai").admitted);
        assert!(by("voice").admitted);
        assert!(!by("llm").admitted);
        assert_eq!(by("llm").reason, REASON_OFF_BUDGET);
        assert!(by("face").admitted, "cheaper later feature still fits");
    }

    #[test]
    fn mode_all_admits_every_enabled_feature() {
        let p = plan(&[feat("llm", true, 999_999)], "all", 2048, 1024, 768);
        assert!(p.features[0].admitted);
        assert_eq!(p.mode, "all");
        assert_eq!(p.budget_mib, 1024, "all mode keeps raw available as budget");
    }

    #[test]
    fn disabled_in_config_reports_off_config() {
        let p = plan(&[feat("vlm", false, 42)], "auto", 8192, 7000, 768);
        assert!(!p.features[0].admitted);
        assert_eq!(p.features[0].reason, REASON_OFF_CONFIG);
    }

    #[test]
    fn meeting_and_decision_follow_voice() {
        let voice = feat("voice", true, 600);
        let mut meeting = feat("meeting", true, 5);
        meeting.depends_on = Some("voice");
        // Budget 100 < voice(600) → voice cut, meeting dependency-cut.
        let p = plan(&[voice, meeting], "auto", 2048, 868, 768);
        assert!(!p.features[0].admitted);
        assert!(!p.features[1].admitted);
        assert_eq!(p.features[1].reason, REASON_DEPENDENCY);
    }

    #[test]
    fn missing_model_files_cost_overhead_only() {
        // /nonexistent paths: file bytes 0 → cost == overhead.
        assert_eq!(file_cost_mib(&["/nonexistent/x".into()]), 0);
        let p = plan(&[feat("ai", true, 40)], "auto", 2048, 800, 768);
        assert_eq!(p.features[0].cost_mib, 40);
    }

    #[test]
    fn file_cost_scales_and_rounds_up() {
        let dir = std::env::temp_dir().join("mibee-res-test");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("cost.bin");
        // 2 MiB + 1 byte → ceil = 3 MiB → ×1.15 → 3.45 → 4.
        let v = vec![0_u8; 2 * 1024 * 1024 + 1];
        std::fs::write(&f, &v).unwrap();
        assert_eq!(file_cost_mib(&[f.to_string_lossy().into_owned()]), 4);
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn zero_budget_admits_nothing_enabled() {
        let p = plan(&[feat("ai", true, 40)], "auto", 1024, 100, 768);
        assert_eq!(p.budget_mib, 0);
        assert!(!p.features[0].admitted);
        assert_eq!(p.features[0].reason, REASON_OFF_BUDGET);
    }
}
