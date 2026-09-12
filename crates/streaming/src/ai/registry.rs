//! Model registry + runtime uploaded overlay (SPEC v1 §4.6), mirroring
//! `mibee-eye-raspi-rs`.
//!
//! Notebook variant: this device's decoder implements the NanoDet family
//! only (the YOLOX port is future work), so the builtin list carries the
//! two NanoDet exports. Uploaded models must also be NanoDet graphs —
//! the upload gate is a full session load, which rejects anything the
//! decoder cannot serve.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Canonical model directory (relative to the working directory, matching
/// the default `model_path` convention; uploads + manifest land here).
pub const MODELS_DIR: &str = "models";

/// Upload size cap (SPEC §4.6 `max_bytes`).
pub const UPLOAD_MAX_BYTES: usize = 32 * 1024 * 1024;

/// Why a model activation failed before the running detector was touched.
#[derive(Debug, Clone)]
pub enum ActivateError {
    /// Model file missing on this device (HTTP 409, SPEC §4.6).
    Unavailable(String),
    /// Detector construction failed (HTTP 500); the old model keeps running.
    LoadFailed(String),
}

/// One entry of the model registry.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// Registry id — the `"model"` value of SPEC §4.6.
    pub id: String,
    /// Decoder family (this build: `nanodet` only).
    pub family: String,
    /// Square model input size in pixels (informational).
    pub input: u32,
    /// Path of the ONNX file (relative to the working dir or absolute).
    pub path: String,
    /// `builtin` | `uploaded`.
    pub source: String,
}

/// The built-in entries (families this build implements).
pub fn builtin() -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            id: "nanodet-plus-m-320".into(),
            family: "nanodet".into(),
            input: 320,
            path: "models/nanodet-m.onnx".into(),
            source: "builtin".into(),
        },
        ModelSpec {
            id: "nanodet-plus-m-416".into(),
            family: "nanodet".into(),
            input: 416,
            path: "models/nanodet-m-416.onnx".into(),
            source: "builtin".into(),
        },
    ]
}

/// Validate a model id's syntax (`^[a-z0-9][a-z0-9-]{0,63}$`).
#[must_use]
pub fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The model a configuration resolves to.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveModel {
    pub id: String,
    pub path: String,
    /// `builtin` | `uploaded` | `custom`.
    pub source: &'static str,
    pub family: String,
    /// 0 = unknown until the ONNX session is loaded (custom paths only).
    pub input: u32,
}

/// Whether the model file exists (drives `available` and the 409 gate).
#[must_use]
pub fn is_available(path: &str) -> bool {
    Path::new(path).exists()
}

/// Manifest record for an uploaded model (persisted as uploaded.json).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UploadEntry {
    id: String,
    family: String,
    input: u32,
    file: String,
}

fn manifest_entry(m: &ModelSpec) -> UploadEntry {
    UploadEntry {
        id: m.id.clone(),
        family: m.family.clone(),
        input: m.input,
        file: Path::new(&m.path)
            .file_name()
            .map_or_else(|| m.id.clone(), |f| f.to_string_lossy().into_owned()),
    }
}

/// Runtime registry: builtin entries plus the uploaded overlay persisted
/// in `<dir>/uploaded.json`.
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<ModelSpec>,
    dir: Option<PathBuf>,
}

impl Registry {
    /// Registry without persistence (tests).
    #[must_use]
    pub fn builtin_only() -> Self {
        Self {
            entries: builtin(),
            dir: None,
        }
    }

    /// Load builtin entries + the uploaded manifest from `dir`; manifest
    /// entries whose files vanished are pruned.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let mut uploaded: Vec<UploadEntry> = std::fs::read(dir.join("uploaded.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let before = uploaded.len();
        uploaded.retain(|e| dir.join(&e.file).exists());
        let mut entries = builtin();
        entries.extend(uploaded.iter().map(|e| ModelSpec {
            id: e.id.clone(),
            family: e.family.clone(),
            input: e.input,
            path: dir.join(&e.file).to_string_lossy().into_owned(),
            source: "uploaded".into(),
        }));
        let reg = Self {
            entries,
            dir: Some(dir.to_path_buf()),
        };
        if uploaded.len() != before {
            let _ = reg.save_manifest(&uploaded);
        }
        reg
    }

    /// All entries (builtin first, then uploaded).
    #[must_use]
    pub fn list(&self) -> &[ModelSpec] {
        &self.entries
    }

    /// Look up an entry by id.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<ModelSpec> {
        self.entries.iter().find(|m| m.id == id).cloned()
    }

    /// The models directory uploads land in (None = no persistence).
    #[must_use]
    pub fn models_dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Resolve `(model id, model path)` against the registry: a non-default
    /// `model_path` is a custom override; otherwise the id must name an
    /// entry.
    pub fn resolve_active(&self, model: &str, model_path: &str) -> Result<ActiveModel, String> {
        if model_path != crate::ai::default_model_path() {
            return Ok(ActiveModel {
                id: "custom".to_string(),
                path: model_path.to_string(),
                source: "custom",
                family: "nanodet".into(),
                input: 0,
            });
        }
        self.find(model).map_or_else(
            || {
                Err(format!(
                    "unknown ai.model '{model}' (known: {})",
                    self.entries
                        .iter()
                        .map(|m| m.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            },
            |spec| {
                Ok(ActiveModel {
                    id: spec.id.clone(),
                    path: spec.path.clone(),
                    source: if spec.source == "uploaded" {
                        "uploaded"
                    } else {
                        "builtin"
                    },
                    family: spec.family.clone(),
                    input: spec.input,
                })
            },
        )
    }

    /// Add an uploaded entry (after validation) and persist the manifest.
    pub fn insert_uploaded(&mut self, spec: ModelSpec) -> std::io::Result<()> {
        let uploaded: Vec<UploadEntry> = self
            .entries
            .iter()
            .filter(|m| m.source == "uploaded")
            .map(manifest_entry)
            .chain(std::iter::once(manifest_entry(&spec)))
            .collect();
        self.save_manifest(&uploaded)?;
        self.entries.push(spec);
        Ok(())
    }

    /// Remove an uploaded entry, persisting the manifest. `None` when the
    /// id is unknown or not uploaded.
    pub fn remove_uploaded(&mut self, id: &str) -> Option<ModelSpec> {
        let idx = self
            .entries
            .iter()
            .position(|m| m.id == id && m.source == "uploaded")?;
        let spec = self.entries.remove(idx);
        let uploaded: Vec<UploadEntry> = self
            .entries
            .iter()
            .filter(|m| m.source == "uploaded")
            .map(manifest_entry)
            .collect();
        if let Some(dir) = self.dir.as_ref() {
            let _ = std::fs::write(
                dir.join("uploaded.json"),
                serde_json::to_vec_pretty(&uploaded).unwrap_or_default(),
            );
        }
        Some(spec)
    }

    fn save_manifest(&self, uploaded: &[UploadEntry]) -> std::io::Result<()> {
        let dir = self
            .dir
            .as_ref()
            .ok_or_else(|| std::io::Error::other("registry has no models dir"))?;
        std::fs::create_dir_all(dir)?;
        std::fs::write(
            dir.join("uploaded.json"),
            serde_json::to_vec_pretty(uploaded).unwrap_or_default(),
        )
    }
}

/// Loads a detector for a model file path, returning the detector and its
/// square input size. Production builds construct an ONNX Runtime session
/// (the load doubles as upload validation); tests inject fakes.
pub type DetectorFactory =
    Arc<dyn Fn(&str) -> Result<(Arc<dyn crate::ai::AiDetector>, u32), ActivateError> + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_ids_and_syntax() {
        assert_eq!(builtin().len(), 2);
        assert!(valid_model_id("nanodet-plus-m-320"));
        assert!(!valid_model_id("Bad_ID"));
        assert!(!valid_model_id(""));
    }

    #[test]
    fn resolve_default_and_custom() {
        let reg = Registry::builtin_only();
        let m = reg
            .resolve_active("nanodet-plus-m-320", &crate::ai::default_model_path())
            .expect("default resolves");
        assert_eq!((m.id.as_str(), m.source), ("nanodet-plus-m-320", "builtin"));
        let m = reg
            .resolve_active("nanodet-plus-m-320", "/opt/custom.onnx")
            .expect("custom path wins");
        assert_eq!(m.source, "custom");
        assert!(
            reg.resolve_active("yolo-9000", &crate::ai::default_model_path())
                .is_err()
        );
    }

    #[test]
    fn uploaded_manifest_roundtrip() {
        let dir = std::env::temp_dir().join(format!("nb-reg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("custom.onnx");
        std::fs::write(&file, b"onnx").unwrap();

        let mut reg = Registry::load(&dir);
        reg.insert_uploaded(ModelSpec {
            id: "custom".into(),
            family: "nanodet".into(),
            input: 320,
            path: file.to_string_lossy().into_owned(),
            source: "uploaded".into(),
        })
        .expect("persist");
        let reloaded = Registry::load(&dir);
        assert_eq!(reloaded.find("custom").expect("survives").input, 320);

        std::fs::remove_file(&file).unwrap();
        assert!(Registry::load(&dir).find("custom").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
