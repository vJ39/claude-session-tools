//! セッション jsonl から一覧表示に必要なメタ情報だけを取り出す。
//!
//! 3672 セッション (3.1GB) を横断するため、全行を `serde_json` に通すことはしない。
//! 必要なキーを含む可能性がある行だけを部分的にパースする。

use std::io::BufRead;

use serde_json::Value;

/// タイトルの出所。custom-title (`/rename`) > ai-title の優先順位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleKind {
    /// `/rename` による手動タイトル
    Custom,
    /// 自動生成タイトル
    Ai,
    /// agent-name (custom-title と同時に出るが、単独で残る場合の保険)
    AgentName,
    /// custom/ai/agent-name が無いときのフォールバック (最初の user メッセージ冒頭)
    FirstPrompt,
    /// タイトル行も user メッセージも無い
    None,
}

impl TitleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TitleKind::Custom => "custom",
            TitleKind::Ai => "ai",
            TitleKind::AgentName => "agent",
            TitleKind::FirstPrompt => "first_prompt",
            TitleKind::None => "none",
        }
    }

    /// [`TitleKind::as_str`] の逆変換 (ストアからの復元用)。
    pub fn from_tag(s: &str) -> Self {
        match s {
            "custom" => TitleKind::Custom,
            "ai" => TitleKind::Ai,
            "agent" => TitleKind::AgentName,
            "first_prompt" => TitleKind::FirstPrompt,
            _ => TitleKind::None,
        }
    }
}

/// タイトル未設定時の表示。
pub const UNTITLED: &str = "(無題)";

/// [`content_text`] が thinking ブロックに挿入するマーカー。
const THINKING_MARKER: &str = "[thinking...]";

/// custom-title/ai-title/agent-name が無いときのフォールバックに使う、
/// 最初の user メッセージ冒頭の表示文字数。
const TITLE_FALLBACK_LIMIT: usize = 40;

/// jsonl 1 ファイルから取り出したメタ情報。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonlMeta {
    /// 各行の `sessionId` (最初に見つかったもの)
    pub session_id: Option<String>,
    /// 各行の `cwd` (最初に見つかったもの)。ディレクトリ名からの逆算は非可逆なので必ずここを使う
    pub cwd: Option<String>,
    /// custom-title の最後の値
    pub custom_title: Option<String>,
    /// ai-title の最後の値
    pub ai_title: Option<String>,
    /// agent-name の最後の値
    pub agent_name: Option<String>,
    /// 最初の user メッセージの本文 (プレビュー用・切り詰め済み)
    pub first_prompt: Option<String>,
    /// 空行を除いた行数
    pub line_count: u64,
    /// JSON として読めなかった行数
    pub parse_errors: u64,
}

impl JsonlMeta {
    /// 表示に使うタイトルと、その出所を返す。
    ///
    /// 優先順位: custom-title > ai-title > agent-name > 最初の user メッセージ冒頭 > `(無題)`。
    pub fn title(&self) -> (String, TitleKind) {
        if let Some(t) = non_empty(&self.custom_title) {
            return (t, TitleKind::Custom);
        }
        if let Some(t) = non_empty(&self.ai_title) {
            return (t, TitleKind::Ai);
        }
        if let Some(t) = non_empty(&self.agent_name) {
            return (t, TitleKind::AgentName);
        }
        if let Some(p) = &self.first_prompt {
            let preview = oneline_preview(p, TITLE_FALLBACK_LIMIT);
            if !preview.is_empty() {
                return (preview, TitleKind::FirstPrompt);
            }
        }
        (UNTITLED.to_string(), TitleKind::None)
    }
}

fn non_empty(v: &Option<String>) -> Option<String> {
    v.as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// プレビュー用に保持する最初の user 発言の最大文字数。
const PROMPT_LIMIT: usize = 200;

/// jsonl を 1 行ずつ読み、メタ情報を組み立てる。
///
/// 行のフルパースは以下の場合のみ行う。
/// 1. sessionId / cwd がまだ埋まっていない
/// 2. タイトル行の可能性がある (`-title` / `agent-name` を含む)
/// 3. 最初の user 発言をまだ拾っていない
pub fn scan_jsonl<R: BufRead>(reader: R) -> JsonlMeta {
    let mut meta = JsonlMeta::default();
    let mut buf = Vec::new();
    let mut reader = reader;

    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }

        let line = match std::str::from_utf8(&buf) {
            Ok(s) => s.trim(),
            Err(_) => {
                meta.parse_errors += 1;
                continue;
            }
        };
        if line.is_empty() {
            continue;
        }
        meta.line_count += 1;

        let need_ids = meta.session_id.is_none() || meta.cwd.is_none();
        let maybe_title = line.contains("-title") || line.contains("agent-name");
        let need_prompt = meta.first_prompt.is_none() && line.contains("\"user\"");

        if !(need_ids || maybe_title || need_prompt) {
            continue;
        }

        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                meta.parse_errors += 1;
                continue;
            }
        };
        let obj = match value.as_object() {
            Some(o) => o,
            None => continue,
        };

        if meta.session_id.is_none()
            && let Some(s) = obj.get("sessionId").and_then(Value::as_str)
            && !s.is_empty()
        {
            meta.session_id = Some(s.to_string());
        }
        if meta.cwd.is_none()
            && let Some(s) = obj.get("cwd").and_then(Value::as_str)
            && !s.is_empty()
        {
            meta.cwd = Some(s.to_string());
        }

        match obj.get("type").and_then(Value::as_str) {
            // 複数回出るので、最後に出現した値で上書きする
            Some("custom-title") => {
                if let Some(s) = obj.get("customTitle").and_then(Value::as_str) {
                    meta.custom_title = Some(s.to_string());
                }
            }
            Some("ai-title") => {
                if let Some(s) = obj.get("aiTitle").and_then(Value::as_str) {
                    meta.ai_title = Some(s.to_string());
                }
            }
            Some("agent-name") => {
                if let Some(s) = obj.get("agentName").and_then(Value::as_str) {
                    meta.agent_name = Some(s.to_string());
                }
            }
            Some("user") if meta.first_prompt.is_none() => {
                let content = obj.get("message").and_then(|m| m.get("content"));
                if let Some(text) = content_text(content) {
                    let text = text.trim();
                    if !text.is_empty() {
                        meta.first_prompt = Some(truncate_chars(text, PROMPT_LIMIT));
                    }
                }
            }
            _ => {}
        }
    }

    meta
}

/// `message.content` から表示用テキストを取り出す。
/// 文字列ならそのまま、ブロック配列なら text ブロックを連結する。
pub fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let mut out = String::new();
            for b in blocks {
                if let Some(o) = b.as_object() {
                    match o.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = o.get("text").and_then(Value::as_str) {
                                out.push_str(t);
                            }
                        }
                        Some("thinking") => out.push_str("[thinking...]"),
                        _ => {}
                    }
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// 文字境界を壊さずに先頭 n 文字へ切り詰める。
pub fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

/// 表示用に 1 行へ整形する。
///
/// thinking ブロックのマーカー ([`content_text`] が挿入するもの) と改行を取り除き、
/// 前後の空白を落として n 文字に切り詰める。タイトルフォールバックと詳細ペインの
/// プレビュー表示で共通に使う。
pub fn oneline_preview(text: &str, n: usize) -> String {
    let cleaned = text.replace(THINKING_MARKER, "").replace('\n', " ");
    truncate_chars(cleaned.trim(), n)
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use std::io::Cursor;

    fn scan(text: &str) -> JsonlMeta {
        scan_jsonl(Cursor::new(text.as_bytes().to_vec()))
    }

    #[test]
    fn 基本的なメタ情報を拾う() {
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"abc-123","cwd":"/Users/work/repo","uuid":"u1","message":{"role":"user","content":"最初の質問"}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"abc-123","uuid":"u2","message":{"role":"assistant","content":[{"type":"text","text":"返答"}]}}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"自動タイトル","sessionId":"abc-123"}"#,
            "\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.session_id.as_deref(), Some("abc-123"));
        assert_eq!(m.cwd.as_deref(), Some("/Users/work/repo"));
        assert_eq!(m.line_count, 3);
        assert_eq!(m.first_prompt.as_deref(), Some("最初の質問"));
        assert_eq!(m.title(), ("自動タイトル".to_string(), TitleKind::Ai));
    }

    #[test]
    fn custom_titleがai_titleより優先される() {
        let jsonl = concat!(
            r#"{"type":"ai-title","aiTitle":"自動","sessionId":"s"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":"手動","sessionId":"s"}"#,
            "\n",
        );
        assert_eq!(scan(jsonl).title(), ("手動".to_string(), TitleKind::Custom));
    }

    #[test]
    fn 複数回出るタイトルは最後の値を採用する() {
        let jsonl = concat!(
            r#"{"type":"custom-title","customTitle":"古い","sessionId":"s"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":"新しい","sessionId":"s"}"#,
            "\n",
        );
        assert_eq!(scan(jsonl).custom_title.as_deref(), Some("新しい"));
    }

    #[test]
    fn agent_nameしか無ければそれを使う() {
        let jsonl = "{\"type\":\"agent-name\",\"agentName\":\"ソースレビュー\",\"sessionId\":\"s\"}\n";
        assert_eq!(
            scan(jsonl).title(),
            ("ソースレビュー".to_string(), TitleKind::AgentName)
        );
    }

    #[test]
    fn タイトルが無ければ最初のuserメッセージ冒頭を使う() {
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"WAFの調査をしたい"}}"#,
            "\n",
        );
        assert_eq!(
            scan(jsonl).title(),
            ("WAFの調査をしたい".to_string(), TitleKind::FirstPrompt)
        );
    }

    #[test]
    fn userメッセージ冒頭も40文字に切り詰める() {
        let long: String = "あ".repeat(60);
        let jsonl = format!(
            r#"{{"type":"user","sessionId":"s","message":{{"role":"user","content":"{long}"}}}}"#
        );
        let (title, kind) = scan(&format!("{jsonl}\n")).title();
        assert_eq!(title.chars().count(), TITLE_FALLBACK_LIMIT);
        assert_eq!(kind, TitleKind::FirstPrompt);
    }

    #[test]
    fn userメッセージ冒頭の改行はスペースに畳む() {
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"1行目\n2行目"}}"#,
            "\n",
        );
        assert_eq!(scan(jsonl).title().0, "1行目 2行目");
    }

    #[test]
    fn custom_ai_agentがあればuserメッセージより優先される() {
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"最初の質問"}}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"自動タイトル","sessionId":"s"}"#,
            "\n",
        );
        assert_eq!(scan(jsonl).title(), ("自動タイトル".to_string(), TitleKind::Ai));
    }

    #[test]
    fn thinkingマーカーのみのuserメッセージは無題にフォールバックする() {
        // title() を直接呼ぶ (scan_jsonl 経由では通常発生しない組み合わせに対する保険)
        let meta = JsonlMeta {
            first_prompt: Some(format!("  {THINKING_MARKER}  \n  ")),
            ..Default::default()
        };
        assert_eq!(meta.title(), (UNTITLED.to_string(), TitleKind::None));
    }

    #[test]
    fn userメッセージが無ければ無題() {
        let jsonl = "{\"type\":\"system\",\"sessionId\":\"s\"}\n";
        assert_eq!(scan(jsonl).title(), (UNTITLED.to_string(), TitleKind::None));
    }

    #[test]
    fn タイトルが無ければ無題() {
        let jsonl = "{\"type\":\"system\",\"sessionId\":\"s\"}\n";
        assert_eq!(scan(jsonl).title(), (UNTITLED.to_string(), TitleKind::None));
    }

    #[test]
    fn 空タイトルは無題にフォールバックする() {
        let jsonl = "{\"type\":\"custom-title\",\"customTitle\":\"   \",\"sessionId\":\"s\"}\n";
        assert_eq!(scan(jsonl).title(), (UNTITLED.to_string(), TitleKind::None));
    }

    #[test]
    fn 空行は数えない() {
        let jsonl = "\n\n{\"type\":\"system\",\"sessionId\":\"s\"}\n\n";
        let m = scan(jsonl);
        assert_eq!(m.line_count, 1);
        assert_eq!(m.parse_errors, 0);
    }

    #[test]
    fn 壊れた行があっても走査を続ける() {
        let jsonl = concat!(
            "{壊れたJSON\n",
            r#"{"type":"custom-title","customTitle":"生き残り","sessionId":"s"}"#,
            "\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.parse_errors, 1);
        assert_eq!(m.custom_title.as_deref(), Some("生き残り"));
    }

    #[test]
    fn 空ファイルでも落ちない() {
        let m = scan("");
        assert_eq!(m.line_count, 0);
        assert_eq!(m.session_id, None);
        assert_eq!(m.title(), (UNTITLED.to_string(), TitleKind::None));
    }

    #[test]
    fn cwdは最初に見つかった値を使う() {
        let jsonl = concat!(
            r#"{"type":"summary","sessionId":"s"}"#,
            "\n",
            r#"{"type":"user","cwd":"/first","sessionId":"s","message":{"role":"user","content":"a"}}"#,
            "\n",
            r#"{"type":"user","cwd":"/second","sessionId":"s","message":{"role":"user","content":"b"}}"#,
            "\n",
        );
        assert_eq!(scan(jsonl).cwd.as_deref(), Some("/first"));
    }

    #[test]
    fn 長いプロンプトは切り詰める() {
        let long: String = "あ".repeat(500);
        let jsonl = format!(
            r#"{{"type":"user","sessionId":"s","message":{{"role":"user","content":"{long}"}}}}"#
        );
        let m = scan(&format!("{jsonl}\n"));
        assert_eq!(m.first_prompt.as_ref().unwrap().chars().count(), PROMPT_LIMIT);
    }

    #[test]
    fn ブロック配列のcontentからテキストを取り出す() {
        let v: Value = serde_json::from_str(
            r#"[{"type":"thinking","thinking":"内心"},{"type":"text","text":"本文"},{"type":"tool_use","name":"Read"}]"#,
        )
        .unwrap();
        assert_eq!(content_text(Some(&v)).unwrap(), "[thinking...]本文");
    }

    #[test]
    fn 文字数で切り詰める() {
        assert_eq!(truncate_chars("あいうえお", 3), "あいう");
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("", 5), "");
    }

    #[test]
    fn タイトル種別の文字列往復() {
        for k in [
            TitleKind::Custom,
            TitleKind::Ai,
            TitleKind::AgentName,
            TitleKind::FirstPrompt,
            TitleKind::None,
        ] {
            assert_eq!(TitleKind::from_tag(k.as_str()), k);
        }
    }

    #[test]
    fn 未知のタグはnoneに落ちる() {
        assert_eq!(TitleKind::from_tag("なにか変な値"), TitleKind::None);
    }

    #[test]
    fn ワンライン整形は改行とthinkingマーカーを取り除く() {
        assert_eq!(
            oneline_preview("1行目\n2行目", 200),
            "1行目 2行目"
        );
        assert_eq!(
            oneline_preview(&format!("  {THINKING_MARKER}本文  "), 200),
            "本文"
        );
    }

    #[test]
    fn ワンライン整形はn文字に切り詰める() {
        assert_eq!(oneline_preview("あいうえおかきくけこ", 3), "あいう");
        // マーカー除去・trim 後に切り詰める (除去前の文字数で切ってはいけない)
        assert_eq!(oneline_preview(&format!("{THINKING_MARKER}abc"), 2), "ab");
    }

    #[test]
    fn ワンライン整形は空文字を返しうる() {
        assert_eq!(oneline_preview(&format!("  {THINKING_MARKER}  "), 40), "");
        assert_eq!(oneline_preview("", 40), "");
    }
}
