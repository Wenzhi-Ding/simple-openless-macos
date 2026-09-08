//! 热词同音纠错:sherpa-onnx 的 SenseVoice 不支持原生热词偏置,这里在听写
//! 结果出口处按拼音同音整段替换(与 my_typeless Python 版同一算法)。
//!
//! 词表:应用数据目录下的 `hotwords.txt`,每行一个词,# 开头为注释;
//! 文件不存在时纠错为恒等操作。贪心最长匹配;比对单位与 Python 版一致:
//! 汉字按单字拼音、连续英数段整段、空白与标点原样。

use std::collections::HashMap;
use std::path::PathBuf;

use pinyin::ToPinyin;

/// 返回热词文件路径(macOS/Windows/Linux 各自的应用数据目录)。
fn hotwords_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("OPENLESS_HOTWORDS_FILE") {
        return Some(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME").ok()?;
    #[cfg(target_os = "macos")]
    return Some(
        PathBuf::from(base)
            .join("Library/Application Support/OpenLess/hotwords.txt"),
    );
    #[cfg(target_os = "windows")]
    let base = std::env::var("APPDATA").ok()?;
    #[cfg(target_os = "windows")]
    return Some(PathBuf::from(base).join("OpenLess/hotwords.txt"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var("HOME").ok()?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Some(
        PathBuf::from(base).join(".config/openless/hotwords.txt"),
    );
}

/// 文本切词:返回 (原文片段, 比对键) 列表。汉字取单字拼音(小写),
/// 连续 ASCII 字母/数字整段小写,其余(空白、标点)原样。
fn units(text: &str) -> Vec<(String, String)> {
    let mut units = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.peek().copied() {
        if is_hanzi(ch) {
            chars.next();
            let key = pinyin_of_char(ch);
            units.push((ch.to_string(), key));
        } else if ch.is_ascii_alphanumeric() {
            let mut run = String::new();
            while let Some(c) = chars.peek().copied() {
                if c.is_ascii_alphanumeric() || (c == '-' && !run.is_empty()) {
                    run.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            let key = run.to_lowercase();
            units.push((run, key));
        } else {
            let mut run = String::new();
            while let Some(c) = chars.peek().copied() {
                if !(is_hanzi(c) || c.is_ascii_alphanumeric()) {
                    run.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            units.push((run.clone(), run));
        }
    }
    units
}

fn is_hanzi(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
}

fn pinyin_of_char(ch: char) -> String {
    let mut buffer = [0u8; 4];
    let s: &str = ch.encode_utf8(&mut buffer);
    let mut out = String::new();
    for py in s.to_pinyin() {
        if let Some(p) = py {
            out.push_str(p.plain());
        }
    }
    if out.is_empty() {
        out.push_str(s);
    }
    out.to_lowercase()
}

/// 热词比对键:pypinyin 风格的拼音序列(小写)。汉字逐字转拼音,
/// 非汉字连续段整段作为一个键元素,与 Python 版 build_index 行为一致。
fn hotword_key(word: &str) -> Vec<String> {
    let mut key = Vec::new();
    let mut latin_run = String::new();
    for ch in word.chars() {
        if is_hanzi(ch) {
            if !latin_run.is_empty() {
                key.push(latin_run.to_lowercase());
                latin_run.clear();
            }
            key.push(pinyin_of_char(ch));
        } else {
            latin_run.push(ch);
        }
    }
    if !latin_run.is_empty() {
        key.push(latin_run.to_lowercase());
    }
    key
}

fn load_index() -> HashMap<Vec<String>, String> {
    let mut index = HashMap::new();
    let Some(path) = hotwords_path() else {
        return index;
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return index;
    };
    for line in content.lines() {
        let word = line.trim();
        if word.is_empty() || word.starts_with('#') {
            continue;
        }
        let key = hotword_key(word);
        if !key.is_empty() {
            index.insert(key, word.to_string());
        }
    }
    index
}

/// 对识别文本做热词同音替换(贪心最长匹配)。词表缺失或为空时原样返回。
pub fn correct(text: &str) -> String {
    let index = load_index();
    if text.is_empty() || index.is_empty() {
        return text.to_string();
    }
    let units = units(text);
    let keys: Vec<String> = units.iter().map(|(_, k)| k.clone()).collect();
    let max_len = index.keys().map(Vec::len).max().unwrap_or(0);
    let mut out = String::new();
    let mut i = 0;
    while i < units.len() {
        let mut matched = false;
        for length in (1..=max_len.min(units.len() - i)).rev() {
            if let Some(word) = index.get(&keys[i..i + length]) {
                out.push_str(word);
                i += length;
                matched = true;
                break;
            }
        }
        if !matched {
            let (raw, _) = &units[i];
            out.push_str(raw);
            i += 1;
        }
    }
    out
}
