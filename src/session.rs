//! セッション jsonl から一覧表示に必要なメタ情報だけを取り出す。
//!
//! 3672 セッション (3.1GB) を横断するため、全行を `serde_json` に通すことはしない。
//! 必要なキーを含む可能性がある行だけを部分的にパースする。
//! ただし fork 検出用の uuid/timestamp だけは全行から (フルパースせず文字列抽出で) 集める。

use std::io::BufRead;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
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
    /// 上記が全部無い・かつ全 user メッセージがスラッシュコマンド実行だった場合の
    /// 最後の手段 (`<command-name>` タグの中身)
    SlashCommand,
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
            TitleKind::SlashCommand => "slash_command",
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
            "slash_command" => TitleKind::SlashCommand,
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

/// スラッシュコマンド実行の内部表現に含まれるタグ。
/// 実データでは `<command-name>`・`<command-message>`・`<command-args>` が
/// 前後関係を問わず出現するが、判定には `<command-name>` があれば十分で、
/// かつ最後の手段のタイトルにはこのタグの中身を使う。
const COMMAND_NAME_OPEN: &str = "<command-name>";
const COMMAND_NAME_CLOSE: &str = "</command-name>";
/// `<command-name>` を伴わずに単独で出ることもあるため、これも検出対象にする。
const COMMAND_MESSAGE_TAG: &str = "<command-message>";

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
    /// 最初の、スラッシュコマンド実行ではない user メッセージの本文 (プレビュー用・切り詰め済み)
    pub first_prompt: Option<String>,
    /// 最初に見つかった `<command-name>` タグの中身。
    /// 全 user メッセージがスラッシュコマンド実行だったセッションの、最後の手段のタイトルに使う
    pub first_command_name: Option<String>,
    /// 各行の `timestamp` (最初に見つかったもの・パース済み)。
    /// ファイルの birthtime はコピー・PC移行等で書き換わるため、作成日時にはこちらを優先する
    pub timestamp: Option<SystemTime>,
    /// 空行を除いた行数
    pub line_count: u64,
    /// JSON として読めなかった行数
    pub parse_errors: u64,
    /// 各行の `uuid` と `timestamp` (fork 検出用)。
    /// `resume` で過去履歴ごとコピーされた別セッションは同じ uuid を持つメッセージが
    /// 複数セッションにまたがって出現するため、これを手がかりに fork 元を特定する
    pub message_uuids: Vec<(String, Option<SystemTime>)>,
}

impl JsonlMeta {
    /// 表示に使うタイトルと、その出所を返す。
    ///
    /// 優先順位: custom-title > ai-title > agent-name > 最初の実質的な user メッセージ冒頭 >
    /// (全 user メッセージがスラッシュコマンド実行だった場合の最後の手段) `<command-name>` の中身 > `(無題)`。
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
        if let Some(cmd) = non_empty(&self.first_command_name) {
            let preview = oneline_preview(&cmd, TITLE_FALLBACK_LIMIT);
            if !preview.is_empty() {
                return (preview, TitleKind::SlashCommand);
            }
        }
        (UNTITLED.to_string(), TitleKind::None)
    }
}

/// テキストがスラッシュコマンド実行の内部表現かどうか。
///
/// `<command-name>` と `<command-message>` はどちらが先に出現するか順序を問わない
/// (実データではタグの並びが前後することがある)ので、単純な部分文字列の有無で判定する。
fn is_slash_command_text(text: &str) -> bool {
    text.contains(COMMAND_NAME_OPEN) || text.contains(COMMAND_MESSAGE_TAG)
}

/// `<command-name>...</command-name>` の中身を取り出す。
/// タグが無い・閉じタグが無い・中身が空白のみの場合は `None`。
/// 他のタグに囲まれていても (ネストしていても) 部分文字列探索なので影響を受けない。
fn extract_command_name(text: &str) -> Option<String> {
    let start = text.find(COMMAND_NAME_OPEN)? + COMMAND_NAME_OPEN.len();
    let rest = &text[start..];
    let end = rest.find(COMMAND_NAME_CLOSE)?;
    let name = rest[..end].trim();
    if name.is_empty() { None } else { Some(name.to_string()) }
}

/// jsonl の `timestamp` (ISO8601 UTC、例 `"2026-07-25T10:00:00.000Z"`) をパースする。
/// Claude Code のセッションは全て 1970 年以降なので、負の値 (壊れたデータ) は捨てる。
fn parse_timestamp(s: &str) -> Option<SystemTime> {
    let dt: DateTime<Utc> = DateTime::parse_from_rfc3339(s).ok()?.with_timezone(&Utc);
    let secs = dt.timestamp();
    if secs < 0 {
        return None;
    }
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs as u64, dt.timestamp_subsec_nanos()))
}

/// 行から `"key":"value"` 形式の文字列値を軽量に取り出す (JSON 全体はパースしない)。
/// fork 検出用の uuid/timestamp 収集は全行に対して行うため、既存の他フィールド抽出
/// (`serde_json::from_str` によるフルパース) を素通りする行にもコストを乗せないための措置。
fn extract_str_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut needle = String::with_capacity(key.len() + 3);
    needle.push('"');
    needle.push_str(key);
    needle.push_str("\":\"");
    let start = line.find(needle.as_str())? + needle.len();
    let end = line[start..].find('"')?;
    Some(&line[start..start + end])
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

        // fork 検出用の uuid 収集は他フィールドの early-exit と無関係に全行へ行う。
        // JSON フルパースは避け、軽量な文字列抽出だけで済ませる (実測: 984MB 全体で
        // フルパースの半分程度の時間で済む)
        if let Some(uuid) = extract_str_field(line, "uuid") {
            let ts = extract_str_field(line, "timestamp").and_then(parse_timestamp);
            meta.message_uuids.push((uuid.to_string(), ts));
        }

        let need_ids = meta.session_id.is_none() || meta.cwd.is_none();
        let maybe_title = line.contains("-title") || line.contains("agent-name");
        let need_prompt = meta.first_prompt.is_none() && line.contains("\"user\"");
        let need_timestamp = meta.timestamp.is_none() && line.contains("\"timestamp\"");

        if !(need_ids || maybe_title || need_prompt || need_timestamp) {
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
        if meta.timestamp.is_none()
            && let Some(s) = obj.get("timestamp").and_then(Value::as_str)
            && let Some(ts) = parse_timestamp(s)
        {
            meta.timestamp = Some(ts);
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
                        if is_slash_command_text(text) {
                            // スラッシュコマンド実行そのものは実質的な発言とみなさない。
                            // 最後の手段用に command-name だけ (最初に見つかったもの) 覚えておき、
                            // 次の (コマンド実行ではない) user メッセージを引き続き探す
                            if meta.first_command_name.is_none() {
                                meta.first_command_name = extract_command_name(text);
                            }
                        } else {
                            meta.first_prompt = Some(truncate_chars(text, PROMPT_LIMIT));
                        }
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
        assert_eq!(
            m.message_uuids.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(),
            vec!["u1", "u2"]
        );
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
            TitleKind::SlashCommand,
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

    // ---- 機能3: timestamp ベースの作成日時 ----

    /// テストの期待値を chrono 経由で作る (手計算での epoch 秒の書き間違いを避ける)。
    fn expected_system_time(rfc3339: &str) -> SystemTime {
        let dt = DateTime::parse_from_rfc3339(rfc3339).unwrap().with_timezone(&Utc);
        SystemTime::UNIX_EPOCH + Duration::from_secs(dt.timestamp() as u64)
    }

    #[test]
    fn timestampをパースして保持する() {
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"s","timestamp":"2026-07-25T10:00:00.000Z","message":{"role":"user","content":"質問"}}"#,
            "\n",
        );
        let m = scan(jsonl);
        assert_eq!(
            m.timestamp,
            Some(expected_system_time("2026-07-25T10:00:00.000Z"))
        );
    }

    #[test]
    fn timestampはオフセット付きでも解釈できる() {
        // +09:00 の 19:00 は UTC の 10:00 と同じ瞬間
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"s","timestamp":"2026-07-25T19:00:00+09:00","message":{"role":"user","content":"質問"}}"#,
            "\n",
        );
        let m = scan(jsonl);
        assert_eq!(
            m.timestamp,
            Some(expected_system_time("2026-07-25T10:00:00.000Z"))
        );
    }

    #[test]
    fn timestampは最初に見つかった行の値を使う() {
        // cwd/session_id と同じく、先頭行に無くても最初に見つかった値を使う
        let jsonl = concat!(
            r#"{"type":"summary","sessionId":"s"}"#,
            "\n",
            r#"{"type":"user","sessionId":"s","timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"a"}}"#,
            "\n",
            r#"{"type":"user","sessionId":"s","timestamp":"2026-02-02T00:00:00.000Z","message":{"role":"user","content":"b"}}"#,
            "\n",
        );
        let m = scan(jsonl);
        assert_eq!(
            m.timestamp,
            Some(expected_system_time("2026-01-01T00:00:00.000Z"))
        );
    }

    #[test]
    fn 壊れたtimestamp文字列は無視する() {
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"s","timestamp":"そもそも日付ではない","message":{"role":"user","content":"a"}}"#,
            "\n",
        );
        assert_eq!(scan(jsonl).timestamp, None);
    }

    #[test]
    fn timestampキーが無ければNone() {
        let jsonl = "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"a\"}}\n";
        assert_eq!(scan(jsonl).timestamp, None);
    }

    #[test]
    fn 空ファイルのtimestampはNone() {
        assert_eq!(scan("").timestamp, None);
    }

    // ---- 機能5: スラッシュコマンド実行をタイトルフォールバックから除外 ----

    #[test]
    fn コマンド実行だけのメッセージはタイトルに使わず次を探す() {
        let jsonl = concat!(
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<command-name>/kibela-reflect</command-name>\\n",
            "            <command-message>kibela-reflect</command-message>\\n",
            "            <command-args>76377</command-args>",
            "\"}}\n",
            r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"WAFの調査をしたい"}}"#,
            "\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.first_prompt.as_deref(), Some("WAFの調査をしたい"));
        assert_eq!(
            m.title(),
            ("WAFの調査をしたい".to_string(), TitleKind::FirstPrompt)
        );
        // 最後の手段用の command-name も (使われないが) 拾ってはいる
        assert_eq!(m.first_command_name.as_deref(), Some("/kibela-reflect"));
    }

    #[test]
    fn 全てコマンド実行のみのセッションはcommand_nameを最後の手段にする() {
        let jsonl = concat!(
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<command-name>/kibela-reflect</command-name>\\n",
            "            <command-message>kibela-reflect</command-message>\\n",
            "            <command-args>76377</command-args>",
            "\"}}\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.first_prompt, None);
        assert_eq!(
            m.title(),
            ("/kibela-reflect".to_string(), TitleKind::SlashCommand)
        );
    }

    #[test]
    fn コマンドタグの順序が前後しても判定できる() {
        // command-message / command-args が command-name より先に出現するケース
        let jsonl = concat!(
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<command-args>76377</command-args>",
            "<command-message>kibela-reflect</command-message>",
            "<command-name>/kibela-reflect</command-name>",
            "\"}}\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.first_prompt, None);
        assert_eq!(m.first_command_name.as_deref(), Some("/kibela-reflect"));
        assert_eq!(
            m.title(),
            ("/kibela-reflect".to_string(), TitleKind::SlashCommand)
        );
    }

    #[test]
    fn command_nameが他のタグに入れ子になっていても判定できる() {
        let jsonl = concat!(
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<local-command-stdout><command-name>/foo</command-name>",
            "<command-message>foo</command-message></local-command-stdout>",
            "\"}}\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.first_prompt, None);
        assert_eq!(m.first_command_name.as_deref(), Some("/foo"));
    }

    #[test]
    fn command_messageのみでcommand_nameが無ければコマンド実行として除外するが最後の手段は取れない() {
        let jsonl = concat!(
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<command-message>kibela-reflect</command-message>",
            "\"}}\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.first_prompt, None, "コマンド実行はfirst_promptに使わない");
        assert_eq!(m.first_command_name, None, "command-nameタグが無いので取れない");
        assert_eq!(m.title(), (UNTITLED.to_string(), TitleKind::None));
    }

    #[test]
    fn command_nameタグの中身が空なら最後の手段にも使えない() {
        let jsonl = concat!(
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<command-name></command-name><command-message>x</command-message>",
            "\"}}\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.first_command_name, None);
        assert_eq!(m.title(), (UNTITLED.to_string(), TitleKind::None));
    }

    #[test]
    fn 閉じタグの無いcommand_nameは抽出できないが検出はされる() {
        let jsonl = concat!(
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<command-name>/foo",
            "\"}}\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.first_prompt, None, "壊れていてもコマンド実行として除外する");
        assert_eq!(m.first_command_name, None, "閉じタグが無いので中身は取れない");
    }

    #[test]
    fn 最初に見つかったcommand_nameを最後の手段に使う() {
        let jsonl = concat!(
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<command-name>/first</command-name>",
            "\"}}\n",
            "{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{\"role\":\"user\",\"content\":\"",
            "<command-name>/second</command-name>",
            "\"}}\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.first_command_name.as_deref(), Some("/first"));
    }

    #[test]
    fn タイトル種別にslash_commandが往復する() {
        assert_eq!(
            TitleKind::from_tag(TitleKind::SlashCommand.as_str()),
            TitleKind::SlashCommand
        );
    }

    #[test]
    fn uuidとtimestampを全行から集める() {
        // fork 検出用の収集は、既存の早期終了 (session_id/cwd/title/first_prompt が
        // 全部埋まった後) に関係なく全行に対して行われる。
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"s","cwd":"/tmp","uuid":"u1","timestamp":"2026-07-16T06:37:18.172Z","message":{"role":"user","content":"質問"}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","uuid":"u2","timestamp":"2026-07-16T06:40:00.000Z","message":{"role":"assistant","content":"回答"}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","uuid":"u3","message":{"role":"assistant","content":"timestampが無い行"}}"#,
            "\n",
        );
        let m = scan(jsonl);
        assert_eq!(m.message_uuids.len(), 3);
        assert_eq!(m.message_uuids[0].0, "u1");
        assert!(m.message_uuids[0].1.is_some());
        assert_eq!(m.message_uuids[2].0, "u3");
        assert_eq!(m.message_uuids[2].1, None, "timestampが無い行はNoneになる");
    }

    #[test]
    fn uuidが無い行はmessage_uuidsに入らない() {
        let jsonl = concat!(
            r#"{"type":"custom-title","customTitle":"タイトル","sessionId":"s"}"#,
            "\n",
        );
        let m = scan(jsonl);
        assert!(m.message_uuids.is_empty());
    }

    #[test]
    fn extract_str_fieldは対象キーの値だけを取り出す() {
        assert_eq!(
            extract_str_field(r#"{"a":"1","uuid":"abc-def","b":"2"}"#, "uuid"),
            Some("abc-def")
        );
        assert_eq!(extract_str_field(r#"{"a":"1"}"#, "uuid"), None);
        assert_eq!(extract_str_field(r#"{"uuidx":"1"}"#, "uuid"), None, "前方一致で誤検出しない");
    }
}
