//! Session-scoped hotword homophone correction.
use pinyin::ToPinyin;
use std::collections::HashMap;
fn is_hanzi(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
}
fn pinyin_of(ch: char) -> String {
    ch.to_pinyin()
        .map(|p| p.plain().to_lowercase())
        .unwrap_or_else(|| ch.to_string())
}
fn units(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut it = text.chars().peekable();
    while let Some(ch) = it.peek().copied() {
        if is_hanzi(ch) {
            it.next();
            out.push((ch.to_string(), pinyin_of(ch)));
        } else if ch.is_ascii_alphanumeric() {
            let mut run = String::new();
            while let Some(c) = it.peek().copied() {
                if c.is_ascii_alphanumeric() || (c == '-' && !run.is_empty()) {
                    run.push(c);
                    it.next();
                } else {
                    break;
                }
            }
            out.push((run.clone(), run.to_lowercase()));
        } else {
            let mut run = String::new();
            while let Some(c) = it.peek().copied() {
                if !is_hanzi(c) && !c.is_ascii_alphanumeric() {
                    run.push(c);
                    it.next();
                } else {
                    break;
                }
            }
            out.push((run.clone(), run));
        }
    }
    out
}
fn key(word: &str) -> Vec<String> {
    units(word).into_iter().map(|(_, key)| key).collect()
}
/// Correct using entries frozen into this DictationContext; no filesystem or environment access.
pub fn correct(text: &str, hotwords: &[String]) -> String {
    if text.is_empty() || hotwords.is_empty() {
        return text.to_string();
    }
    let index: HashMap<_, _> = hotwords
        .iter()
        .filter_map(|word| {
            let k = key(word);
            (!k.is_empty()).then(|| (k, word.as_str()))
        })
        .collect();
    let parts = units(text);
    let keys: Vec<_> = parts.iter().map(|(_, k)| k.clone()).collect();
    let max = index.keys().map(Vec::len).max().unwrap_or(0);
    let mut out = String::new();
    let mut i = 0;
    while i < parts.len() {
        let mut found = false;
        for n in (1..=max.min(parts.len() - i)).rev() {
            if let Some(word) = index.get(&keys[i..i + n]) {
                out.push_str(word);
                i += n;
                found = true;
                break;
            }
        }
        if !found {
            out.push_str(&parts[i].0);
            i += 1
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_homophone() {
        assert_eq!(correct("张是", &["张世".into()]), "张世")
    }
    #[test]
    fn english_case() {
        assert_eq!(correct("openless", &["OpenLess".into()]), "OpenLess")
    }
    #[test]
    fn longest_match() {
        assert_eq!(
            correct("北精大穴", &["北京".into(), "北京大学".into()]),
            "北京大学"
        )
    }
    #[test]
    fn empty_words() {
        assert_eq!(correct("原文", &[]), "原文")
    }
}
