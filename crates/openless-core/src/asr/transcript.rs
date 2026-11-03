//! 分块转写结果的拼接。
//!
//! 长音频按块送识别后，每块单独返回文本。拼接时只在两侧都适合断句的位置补
//! 一个空格：中日韩文字之间不补，英文单词之间要补，标点按自身的开闭决定。
//! 空块（整块静音、模型没有输出）直接跳过。

/// 拼接多块转写文本：逐块 `trim()`，空块跳过，按相邻字符决定是否补空格。
pub fn join_transcript_chunks(chunks: &[String]) -> String {
    let mut joined = String::new();
    for chunk in chunks.iter().map(|chunk| chunk.trim()) {
        if chunk.is_empty() {
            continue;
        }
        if needs_chunk_separator(&joined, chunk) {
            joined.push(' ');
        }
        joined.push_str(chunk);
    }
    joined
}

fn needs_chunk_separator(current: &str, next: &str) -> bool {
    let Some(prev) = current.chars().last() else {
        return false;
    };
    let Some(first) = next.chars().next() else {
        return false;
    };

    if is_closing_punctuation(first) || is_opening_punctuation(prev) {
        return false;
    }
    if is_cjk(prev) && (is_cjk(first) || is_opening_punctuation(first)) {
        return false;
    }
    if is_cjk(first) && is_closing_punctuation(prev) {
        return false;
    }
    if is_cjk_punctuation(prev) && is_cjk(first) {
        return false;
    }
    true
}

fn is_opening_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '(' | '[' | '{' | '"' | '\'' | '（' | '「' | '『' | '《' | '“' | '‘'
    )
}

fn is_closing_punctuation(ch: char) -> bool {
    matches!(
        ch,
        ',' | '.'
            | '!'
            | '?'
            | ':'
            | ';'
            | ')'
            | ']'
            | '}'
            | '"'
            | '\''
            | '，'
            | '。'
            | '、'
            | '！'
            | '？'
            | '：'
            | '；'
            | '）'
            | '」'
            | '』'
            | '》'
            | '”'
            | '’'
            | '…'
    )
}

fn is_cjk_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。'
            | '、'
            | '！'
            | '？'
            | '：'
            | '；'
            | '（'
            | '）'
            | '「'
            | '」'
            | '『'
            | '』'
            | '《'
            | '》'
            | '“'
            | '”'
            | '‘'
            | '’'
            | '…'
            | '—'
    )
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0x3040..=0x30FF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_empty_chunks() {
        let chunks = vec![" hello ".to_string(), "".to_string(), "world".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "hello world");
    }

    #[test]
    fn keeps_cjk_together() {
        let chunks = vec!["你好".to_string(), "世界".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "你好世界");
    }

    #[test]
    fn separates_mixed_script_boundaries() {
        let chunks = vec!["中文".to_string(), "English".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "中文 English");

        let chunks = vec!["OpenLess".to_string(), "中文".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "OpenLess 中文");
    }

    #[test]
    fn handles_punctuation_boundaries() {
        let chunks = vec!["hello,".to_string(), "world".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "hello, world");

        let chunks = vec!["hello".to_string(), ",".to_string(), "world".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "hello, world");

        let chunks = vec!["foo.".to_string(), "bar".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "foo. bar");

        let chunks = vec!["(".to_string(), "hello".to_string(), ")".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "(hello)");
    }

    #[test]
    fn handles_cjk_punctuation_boundaries() {
        let chunks = vec!["你好".to_string(), "，世界".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "你好，世界");

        let chunks = vec!["中文".to_string(), "。".to_string(), "下一句".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "中文。下一句");

        let chunks = vec!["他说".to_string(), "：".to_string(), "你好".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "他说：你好");

        let chunks = vec!["中文。".to_string(), "OpenAI".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "中文。 OpenAI");

        let chunks = vec!["「".to_string(), "中文".to_string(), "」".to_string()];
        assert_eq!(join_transcript_chunks(&chunks), "「中文」");
    }
}
