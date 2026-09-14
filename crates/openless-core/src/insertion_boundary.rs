//! 上屏文本与既有文档的衔接处理(纯文本规则,不涉及平台插入机制)。
//!
//! 听写结果默认整段原样插入,但光标常停在句子中间或句尾标点之后,直接拼接会出现
//! 「句号后缺空格」「句中冒出大写开头」这类违和感。这里依据听写触发时冻结的
//! 光标前文本(`DictationPolishContext::cursor_context`),对上屏文本做最小的
//! 确定性调整:只动第一个字符的大小写,以及是否在最前面补一个空格。
//!
//! 边界:光标上下文缺失(未捕获、被安全门拦截、光标在文档开头)时保持原样;
//! 中文之间、全角标点之后永不补空格(中文排版惯例);调整只发生在插入层,
//! 历史与诊断里保存的仍是润色原始输出。

use crate::prompts::CURSOR_MARKER;

/// 光标前文本的取样长度。只需覆盖「尾部空白 + 最后一个字符」,取 16 字符足够。
const TAIL_MAX_CHARS: usize = 16;

/// 从冻结的光标上下文(形如 `上文{CURSOR_MARKER}下文`)取出光标前的尾部文本。
/// 标记缺失时返回 `None`,调用方按「无衔接信息」处理。
pub fn document_tail_before_cursor(cursor_context: Option<&str>) -> Option<String> {
    let context = cursor_context?;
    let (before, _) = context.split_once(CURSOR_MARKER)?;
    let tail: Vec<char> = before.chars().rev().take(TAIL_MAX_CHARS).collect();
    Some(tail.into_iter().rev().collect())
}

/// 光标前最后一个字符的类别,决定补空格与大小写动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrevChar {
    /// ASCII 句末标点(`.` `!` `?` `…`):之后是新句子。
    SentenceEnd,
    /// ASCII 句中标点(`,` `;` `:`):同一句继续。
    ClauseEnd,
    /// ASCII 字母或数字:光标停在词中间或词后。
    WordChar,
    /// CJK 汉字或中文/全角标点:中文排版,不补空格、无大小写。
    Cjk,
    /// 引号、括号等其他字符:无从判断,保持原样。
    Other,
}

/// 依据光标前文本对将要插入的听写文本做衔接调整。
///
/// 规则只有两个动作:必要时在最前面补一个空格;必要时只改写首个 ASCII 字母的
/// 大小写。听写文本以空白、标点、emoji 开头,或以中文开头时,一律保持原样。
pub fn adjust_for_boundary(text: &str, tail_before_cursor: Option<&str>) -> String {
    let Some(tail) = tail_before_cursor else {
        return text.to_string();
    };
    let Some(head) = text.chars().next() else {
        return text.to_string();
    };
    // 显式排版(前导空白)、标点/列表符开头、中文开头:不调整。
    if head.is_whitespace() || !head.is_alphanumeric() || is_cjk(head) {
        return text.to_string();
    }
    // 跳过光标前的尾部空白找最后一个非空白字符;全空白说明光标在文档开头附近,
    // 句子状态未知,保持原样。
    let trimmed = tail.trim_end();
    let Some(prev) = trimmed.chars().last() else {
        return text.to_string();
    };
    let has_gap = trimmed.len() != tail.len();

    let prev_class = classify_prev(prev);
    let mut result = String::with_capacity(text.len() + 1);
    match prev_class {
        PrevChar::Cjk | PrevChar::Other => return text.to_string(),
        // 句末/句中标点之后紧贴(没有空隙)才补空格;已有空隙就不重复补。
        PrevChar::SentenceEnd | PrevChar::ClauseEnd if !has_gap => result.push(' '),
        _ => {}
    }
    let mut chars = text.chars();
    let head = chars.next().unwrap();
    match prev_class {
        // 句末之后应该是大写开头(原文小写说明是识别/润色漏了)。
        PrevChar::SentenceEnd if head.is_ascii_lowercase() => {
            result.extend(head.to_uppercase());
        }
        // 句中继续时不该有大写开头。
        PrevChar::ClauseEnd | PrevChar::WordChar if head.is_ascii_uppercase() => {
            result.extend(head.to_lowercase());
        }
        _ => result.push(head),
    }
    result.push_str(chars.as_str());
    result
}

fn classify_prev(c: char) -> PrevChar {
    if is_cjk(c) {
        return PrevChar::Cjk;
    }
    match c {
        '.' | '!' | '?' | '…' => PrevChar::SentenceEnd,
        ',' | ';' | ':' => PrevChar::ClauseEnd,
        c if c.is_ascii_alphanumeric() => PrevChar::WordChar,
        _ => PrevChar::Other,
    }
}

/// CJK 汉字、CJK 标点与全角形式。命中时按中文排版处理:不补空格、不改大小写。
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3000..=0x303F    // CJK 标点(。、「」〗)
            | 0x3400..=0x4DBF  // CJK 扩展 A
            | 0x4E00..=0x9FFF  // CJK 基本区
            | 0xF900..=0xFAFF  // CJK 兼容表意
            | 0xFF00..=0xFFEF  // 全角形式(！？，：；)
            | 0x20000..=0x2FA1F // CJK 扩展 B..F 及兼容补充
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(before: &str) -> String {
        crate::prompts::cursor_context_input(before, "")
    }

    #[test]
    fn tail_extraction_takes_text_before_marker_only() {
        let context = marked("已经写完的上文");
        let tail = document_tail_before_cursor(Some(&context)).unwrap();
        assert!(tail.ends_with("的上文"));
        assert!(!tail.contains(CURSOR_MARKER));
    }

    #[test]
    fn tail_extraction_fails_closed_without_marker_or_context() {
        assert_eq!(document_tail_before_cursor(None), None);
        assert_eq!(document_tail_before_cursor(Some("没有标记的文本")), None);
        assert_eq!(
            document_tail_before_cursor(Some(&marked(""))).unwrap_or_default(),
            ""
        );
    }

    #[test]
    fn missing_or_blank_tail_keeps_text_verbatim() {
        assert_eq!(adjust_for_boundary("明天十点", None), "明天十点");
        assert_eq!(adjust_for_boundary("明天十点", Some("")), "明天十点");
        assert_eq!(adjust_for_boundary("明天十点", Some("   ")), "明天十点");
    }

    #[test]
    fn cjk_head_and_cjk_tail_never_gain_space() {
        let tail = document_tail_before_cursor(Some(&marked("已经写完了。"))).unwrap();
        assert_eq!(
            adjust_for_boundary("明天十点开会", Some(&tail)),
            "明天十点开会"
        );
        // 英文字母后面接中文也不补空格(中英混排惯例)。
        let tail = document_tail_before_cursor(Some(&marked("release v2"))).unwrap();
        assert_eq!(adjust_for_boundary("发布说明", Some(&tail)), "发布说明");
    }

    #[test]
    fn ascii_sentence_end_adds_space_and_capitalizes() {
        let tail = document_tail_before_cursor(Some(&marked("I finished the report."))).unwrap();
        assert_eq!(
            adjust_for_boundary("then we left", Some(&tail)),
            " Then we left"
        );
        // 已有空隙时只补大小写不重复加空格。
        let tail = document_tail_before_cursor(Some(&marked("I finished the report. "))).unwrap();
        assert_eq!(
            adjust_for_boundary("then we left", Some(&tail)),
            "Then we left"
        );
    }

    #[test]
    fn clause_end_adds_space_and_lowercases() {
        let tail = document_tail_before_cursor(Some(&marked("hello,"))).unwrap();
        assert_eq!(adjust_for_boundary("World", Some(&tail)), " world");
        let tail = document_tail_before_cursor(Some(&marked("注意: "))).unwrap();
        assert_eq!(
            adjust_for_boundary("This is important", Some(&tail)),
            "this is important"
        );
    }

    #[test]
    fn mid_word_continuation_lowercases_without_space() {
        let tail = document_tail_before_cursor(Some(&marked("hello wor"))).unwrap();
        assert_eq!(adjust_for_boundary("World", Some(&tail)), "world");
    }

    #[test]
    fn digit_head_gets_space_after_sentence_end_but_no_case_change() {
        let tail = document_tail_before_cursor(Some(&marked("会议改到"))).unwrap();
        assert_eq!(adjust_for_boundary("3 点", Some(&tail)), "3 点");
        let tail = document_tail_before_cursor(Some(&marked("see section 2."))).unwrap();
        assert_eq!(
            adjust_for_boundary("3 covers scope", Some(&tail)),
            " 3 covers scope"
        );
    }

    #[test]
    fn cjk_punctuation_tail_keeps_latin_head_verbatim() {
        // 中文全角句号后接英文:中文排版惯例不补半角空格。
        let tail = document_tail_before_cursor(Some(&marked("写完了。"))).unwrap();
        assert_eq!(adjust_for_boundary("Done", Some(&tail)), "Done");
    }

    #[test]
    fn explicit_layout_or_symbol_head_is_untouched() {
        let tail = document_tail_before_cursor(Some(&marked("end."))).unwrap();
        assert_eq!(adjust_for_boundary("\n- 列表项", Some(&tail)), "\n- 列表项");
        assert_eq!(
            adjust_for_boundary("\"Quoted start", Some(&tail)),
            "\"Quoted start"
        );
        assert_eq!(adjust_for_boundary("", Some(&tail)), "");
    }

    #[test]
    fn quote_or_bracket_tail_leaves_text_verbatim() {
        let tail = document_tail_before_cursor(Some(&marked("he said ("))).unwrap();
        assert_eq!(
            adjust_for_boundary("Note the difference", Some(&tail)),
            "Note the difference"
        );
    }
}
