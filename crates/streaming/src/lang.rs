//! Spoken-language detection shared by the reply-language nudge (chat
//! system turn) and the trilingual TTS model picker (SPEC appendix A
//! #30-D). Cheap heuristics, tuned for Mandarin / Cantonese / English:
//! distinctive Cantonese characters win first, then ASCII-only text is
//! English, everything else (including ambiguous shared-character
//! Chinese) falls back to Mandarin.

/// Language of a short user/reply text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpokenLang {
    Mandarin,
    Cantonese,
    English,
}

/// Distinctive Cantonese characters — rare in written Mandarin.
const CANTONESE_HINT_CHARS: &[char] = &[
    '咁', '嘅', '唔', '係', '喺', '嗰', '啲', '乜', '嘢', '佢', '嚟', '噉', '咧', '嚿', '掂', '冇',
    '哋', '畀', '睇', '諗', '谂', '攞', '乸', '孭', '瞓',
];

/// Detect the spoken language of `text`.
///
/// Ambiguity note: a sentence written entirely in characters shared
/// between Mandarin and written Cantonese (e.g. 「你今日见到几多人」)
/// is genuinely undetectable without audio — it falls back to
/// [`SpokenLang::Mandarin`], matching the ASR default.
#[must_use]
pub fn detect(text: &str) -> SpokenLang {
    if text.chars().any(|c| CANTONESE_HINT_CHARS.contains(&c)) {
        return SpokenLang::Cantonese;
    }
    let mut letters = 0usize;
    let mut cjk = 0usize;
    for c in text.chars() {
        if c.is_ascii_alphabetic() {
            letters += 1;
        } else if ('\u{4e00}'..='\u{9fff}').contains(&c) {
            cjk += 1;
        }
    }
    if letters >= 2 && cjk == 0 {
        return SpokenLang::English;
    }
    SpokenLang::Mandarin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cantonese_distinctive_chars() {
        assert_eq!(detect("我唔知道啊"), SpokenLang::Cantonese);
        assert_eq!(detect("佢哋喺边度？"), SpokenLang::Cantonese);
        assert_eq!(detect("今晚食乜嘢？"), SpokenLang::Cantonese);
    }

    #[test]
    fn english_ascii_only() {
        assert_eq!(detect("what time is it?"), SpokenLang::English);
        assert_eq!(detect("OK 3 minutes"), SpokenLang::English);
    }

    #[test]
    fn mandarin_and_ambiguous_fallback() {
        assert_eq!(detect("现在几点了"), SpokenLang::Mandarin);
        // Shared-character Cantonese is undetectable — falls back.
        assert_eq!(detect("你今日见到几多人"), SpokenLang::Mandarin);
    }

    #[test]
    fn mixed_cjk_never_english() {
        assert_eq!(detect("这个 hello 什么意思"), SpokenLang::Mandarin);
    }
}
