//! YAMNet class-name resolution and Chinese display labels.

use tracing::warn;

use super::labels_data::YAMNET_CLASSES;

/// A watched class resolved to its YAMNet output column.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchedClass {
    /// Canonical YAMNet display name (the config key).
    pub name: String,
    /// Chinese display label (the English name itself when no mapping).
    pub label_zh: String,
    /// Index into the 521-wide YAMNet score vector.
    pub index: usize,
}

/// Default watched classes (security-relevant AudioSet classes).
pub const DEFAULT_CLASSES: &[&str] = &[
    "Dog",
    "Bark",
    "Yip",
    "Howl",
    "Bow-wow",
    "Baby cry, infant cry",
    "Screaming",
    "Shout",
    "Glass",
    "Shatter",
    "Breaking",
    "Smoke detector, smoke alarm",
    "Fire alarm",
    "Siren",
    "Knock",
];

/// Chinese labels for classes users commonly watch. Unmapped classes fall
/// back to the English display name.
const ZH_LABELS: &[(&str, &str)] = &[
    ("Speech", "人声"),
    ("Conversation", "谈话声"),
    ("Child speech, kid speaking", "儿童说话"),
    ("Shout", "喊叫"),
    ("Screaming", "尖叫"),
    ("Crying, sobbing", "哭声"),
    ("Baby cry, infant cry", "婴儿啼哭"),
    ("Baby laughter", "婴儿笑声"),
    ("Dog", "狗叫"),
    ("Bark", "犬吠"),
    ("Yip", "犬吠"),
    ("Howl", "嚎叫"),
    ("Bow-wow", "狗叫"),
    ("Cat", "猫叫"),
    ("Purr", "猫呼噜"),
    ("Moo", "牛叫"),
    ("Oink", "猪叫"),
    ("Roar", "吼叫"),
    ("Chicken, rooster", "鸡鸣"),
    ("Bird", "鸟叫"),
    ("Chirp, tweet", "鸟鸣"),
    ("Crow", "鸦叫"),
    ("Knock", "敲门"),
    ("Door", "门响"),
    ("Doorbell", "门铃"),
    ("Glass", "玻璃声"),
    ("Shatter", "破碎声"),
    ("Breaking", "破碎"),
    ("Alarm", "警报"),
    ("Siren", "警笛"),
    ("Smoke detector, smoke alarm", "烟雾报警器"),
    ("Fire alarm", "火警"),
    ("Whistle", "口哨"),
    ("Vehicle", "车辆"),
    ("Vehicle horn, car horn, honking", "汽车鸣笛"),
    ("Car", "汽车"),
    ("Truck", "卡车"),
    ("Motorcycle", "摩托车"),
    ("Music", "音乐"),
    ("Crowd", "人群"),
    ("Applause", "掌声"),
    ("Laughter", "笑声"),
    ("Cough", "咳嗽"),
    ("Sneeze", "打喷嚏"),
    ("Run", "奔跑声"),
    ("Walk", "脚步声"),
];

/// Workspace-relative model path for self-test runs (the binary may start
/// from any cwd; tests/CLI default to the repo layout).
#[must_use]
pub fn self_model_path(which: &str) -> String {
    match which {
        "yamnet" => "models/audio/yamnet.onnx".to_string(),
        _ => "models/audio/silero_vad.onnx".to_string(),
    }
}

/// Look up a YAMNet output column by display name.
#[must_use]
pub fn class_index(name: &str) -> Option<usize> {
    YAMNET_CLASSES
        .iter()
        .position(|c| c.eq_ignore_ascii_case(name))
}

/// Chinese display label for a class name (the name itself when unmapped).
#[must_use]
pub fn zh_label(name: &str) -> String {
    ZH_LABELS
        .iter()
        .find(|(en, _)| en.eq_ignore_ascii_case(name))
        .map(|(_, zh)| (*zh).to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Resolve configured class names to watched classes; unknown names are
/// logged and skipped (typo-tolerant startup, never a hard failure).
#[must_use]
pub fn resolve(configured: &[String]) -> Vec<WatchedClass> {
    let mut out = Vec::with_capacity(configured.len());
    for name in configured {
        match class_index(name) {
            Some(index) => out.push(WatchedClass {
                name: YAMNET_CLASSES[index].to_string(),
                label_zh: zh_label(name),
                index,
            }),
            None => warn!(class = %name, "audio_ai: unknown YAMNet class, ignored"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_size_matches_model_output() {
        assert_eq!(YAMNET_CLASSES.len(), 521);
    }

    #[test]
    fn resolves_default_classes() {
        let specs = resolve(
            &DEFAULT_CLASSES
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>(),
        );
        assert_eq!(specs.len(), DEFAULT_CLASSES.len(), "all defaults resolve");
        for spec in &specs {
            assert!(spec.index < 521);
            assert!(!spec.label_zh.is_empty());
        }
    }

    #[test]
    fn unknown_class_is_skipped() {
        let specs = resolve(&["Definitely Not A Class".to_string(), "Dog".to_string()]);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "Dog");
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(class_index("dog").is_some());
        assert_eq!(class_index("dog"), class_index("Dog"));
    }

    #[test]
    fn zh_labels_cover_security_defaults() {
        for name in DEFAULT_CLASSES {
            let zh = zh_label(name);
            assert_ne!(zh, "", "no zh label for {name}");
        }
    }

    #[test]
    fn unmapped_class_falls_back_to_english() {
        assert_eq!(zh_label("Anechoic chamber"), "Anechoic chamber");
    }
}
