//! `merge-session.py` の移植。
//!
//! セッション B の user/assistant メッセージを抽出し、UUID / parentUuid / sessionId を
//! 再採番してセッション A へ挿入する。Python 版の挙動 (CLI 引数・出力文言・
//! バックアップ命名・UUID 連鎖の作り方) をそのまま踏襲する。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use uuid::Uuid;

/// マージ動作の指定。
#[derive(Debug, Clone, Copy, Default)]
pub struct MergeOptions {
    /// 書き込まず件数とプレビューだけ出す
    pub dry_run: bool,
    /// A の末尾に追加する (既定は先頭挿入)
    pub append: bool,
}

/// マージ結果のサマリー。
#[derive(Debug, Clone)]
pub struct MergeReport {
    /// 表示用のモード名
    pub mode: &'static str,
    /// セッション A のエントリ数
    pub a_entries: usize,
    /// セッション B から抽出した会話メッセージ数
    pub b_conversation: usize,
    /// マージ後の総エントリ数
    pub merged: usize,
    /// セッション A の sessionId
    pub session_id: String,
    /// 実書き込み時のバックアップ先 (dry-run では None)
    pub backup_path: Option<PathBuf>,
}

const MODE_APPEND: &str = "末尾に追加";
const MODE_INSERT: &str = "先頭に挿入";

/// プレビューで表示する会話の件数。
const PREVIEW_LIMIT: usize = 10;
/// プレビュー 1 件あたりの最大文字数。
const PREVIEW_WIDTH: usize = 80;

/// jsonl を読み込み、空行を飛ばして 1 行 1 JSON としてパースする。
pub fn load_jsonl(path: &Path) -> Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("エラー: {} を読み込めない", path.display()))?;
    parse_jsonl(&text).with_context(|| format!("エラー: {} のパースに失敗", path.display()))
}

/// jsonl 文字列をパースする (行番号つきエラー)。
pub fn parse_jsonl(text: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("{} 行目が JSON として読めない", i + 1))?;
        out.push(value);
    }
    Ok(out)
}

/// エントリの `type` を取り出す (オブジェクトでなければ None)。
fn entry_type(v: &Value) -> Option<&str> {
    v.as_object()?.get("type")?.as_str()
}

fn is_conversation(v: &Value) -> bool {
    matches!(entry_type(v), Some("user") | Some("assistant"))
}

/// user/assistant メッセージだけ抽出する。
pub fn extract_conversation(entries: &[Value]) -> Vec<Value> {
    entries.iter().filter(|e| is_conversation(e)).cloned().collect()
}

/// セッション A の最初の user メッセージの位置。無ければ `entries.len()`。
pub fn find_first_user_index(entries: &[Value]) -> usize {
    entries
        .iter()
        .position(|e| entry_type(e) == Some("user"))
        .unwrap_or(entries.len())
}

/// 最後の user/assistant メッセージの uuid。
///
/// Python 版の `if not last_uuid` に合わせ、キー無し・null・空文字は「無し」とみなす。
pub fn find_last_message_uuid(entries: &[Value]) -> Option<String> {
    for e in entries.iter().rev() {
        if !is_conversation(e) {
            continue;
        }
        return e
            .as_object()
            .and_then(|o| o.get("uuid"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    None
}

/// UUID を採番する差し替え可能なジェネレータ (テスト用に決定的な実装を差せる)。
pub trait UuidGen {
    /// `uuid.uuid4()` 相当 (ハイフンつき)
    fn next_uuid(&mut self) -> String;
    /// `uuid.uuid4().hex[:24]` 相当
    fn next_hex24(&mut self) -> String;
}

/// 本番用のランダム v4 ジェネレータ。
#[derive(Debug, Default, Clone, Copy)]
pub struct RandomUuidGen;

impl UuidGen for RandomUuidGen {
    fn next_uuid(&mut self) -> String {
        Uuid::new_v4().hyphenated().to_string()
    }
    fn next_hex24(&mut self) -> String {
        let hex = Uuid::new_v4().simple().to_string();
        hex[..24].to_string()
    }
}

/// メッセージの uuid / parentUuid / sessionId を書き換える。
///
/// 戻り値は (書き換え後メッセージ, 最後の uuid)。`parent_uuid` は JSON の
/// null もありうるため `Value` で受ける。
pub fn rewrite_messages<G: UuidGen>(
    messages: &[Value],
    session_id: &str,
    parent_uuid: Value,
    uuid_gen: &mut G,
) -> (Vec<Value>, Value) {
    let mut rewritten = Vec::with_capacity(messages.len());
    let mut prev_uuid = parent_uuid;

    for msg in messages {
        let new_uuid = uuid_gen.next_uuid();
        // オブジェクト以外は会話として抽出されないが、念のため素通しする
        let mut obj: Map<String, Value> = match msg.as_object() {
            Some(o) => o.clone(),
            None => {
                rewritten.push(msg.clone());
                continue;
            }
        };

        obj.insert("uuid".to_string(), Value::String(new_uuid.clone()));
        obj.insert("parentUuid".to_string(), prev_uuid.clone());
        obj.insert("sessionId".to_string(), Value::String(session_id.to_string()));
        obj.insert("isSidechain".to_string(), Value::Bool(false));

        // requestId も新規生成 (assistant のみ・元から持っている場合だけ)
        if obj.get("type").and_then(Value::as_str) == Some("assistant")
            && obj.contains_key("requestId")
        {
            let hex = uuid_gen.next_hex24();
            obj.insert("requestId".to_string(), Value::String(format!("req_merged_{hex}")));
        }

        prev_uuid = Value::String(new_uuid);
        rewritten.push(Value::Object(obj));
    }

    (rewritten, prev_uuid)
}

/// マージ結果 (書き込み前の状態)。
#[derive(Debug, Clone)]
pub struct MergePlan {
    pub merged: Vec<Value>,
    pub rewritten_b: Vec<Value>,
    pub a_entries: usize,
    pub b_conversation: usize,
    pub session_id: String,
    pub mode: &'static str,
    /// プレビュー対象 (B の会話)
    pub preview: Vec<Value>,
}

/// 実ファイルに触れずにマージ後のエントリ列を組み立てる。
pub fn build_plan<G: UuidGen>(
    a_entries: Vec<Value>,
    b_entries: &[Value],
    opts: MergeOptions,
    uuid_gen: &mut G,
) -> Result<MergePlan> {
    let mut a_entries = a_entries;

    // セッション A の sessionId を取得
    let session_id = a_entries
        .iter()
        .filter_map(|e| e.as_object())
        .filter_map(|o| o.get("sessionId"))
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .map(str::to_string);

    let session_id = match session_id {
        Some(s) => s,
        None => bail!("エラー: セッションAからsessionIdが見つからない"),
    };

    let b_conversation = extract_conversation(b_entries);
    if b_conversation.is_empty() {
        bail!("エラー: セッションBに会話メッセージがない");
    }

    let a_len = a_entries.len();
    let mode = if opts.append { MODE_APPEND } else { MODE_INSERT };

    let (merged, rewritten_b) = if opts.append {
        // 末尾追加モード: A の最後のメッセージの後に B を繋ぐ
        let last_uuid = match find_last_message_uuid(&a_entries) {
            Some(u) => u,
            None => bail!("エラー: セッションAにメッセージがない"),
        };
        let (rewritten_b, _) =
            rewrite_messages(&b_conversation, &session_id, Value::String(last_uuid), uuid_gen);
        let mut merged = a_entries;
        merged.extend(rewritten_b.iter().cloned());
        (merged, rewritten_b)
    } else {
        // 先頭挿入モード
        let first_user_idx = find_first_user_index(&a_entries);
        if first_user_idx == 0 {
            bail!("エラー: セッションAにヘッダーがない");
        }
        // Python 版はここで IndexError になるケース。明示的なエラーにする
        if first_user_idx >= a_entries.len() {
            bail!("エラー: セッションAにuserメッセージがない");
        }

        let original_parent = a_entries[first_user_idx]
            .as_object()
            .and_then(|o| o.get("parentUuid"))
            .cloned()
            .unwrap_or(Value::Null);

        let (rewritten_b, last_b_uuid) =
            rewrite_messages(&b_conversation, &session_id, original_parent, uuid_gen);

        if let Some(obj) = a_entries[first_user_idx].as_object_mut() {
            obj.insert("parentUuid".to_string(), last_b_uuid);
        }

        let body = a_entries.split_off(first_user_idx);
        let mut merged = a_entries; // header
        merged.extend(rewritten_b.iter().cloned());
        merged.extend(body);
        (merged, rewritten_b)
    };

    Ok(MergePlan {
        merged,
        rewritten_b,
        a_entries: a_len,
        b_conversation: b_conversation.len(),
        session_id,
        mode,
        preview: b_conversation,
    })
}

/// プレビュー 1 行分の本文を作る (Python 版の `str(content)[:80]` 相当)。
pub fn preview_content(entry: &Value) -> String {
    let content = entry.as_object().and_then(|o| o.get("message")).and_then(|m| {
        if m.is_object() {
            m.get("content")
        } else {
            None
        }
    });

    let text = match content {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(_)) => crate::session::content_text(content).unwrap_or_default(),
        // Python の str() に寄せる
        Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(v @ Value::Object(_)) => v.to_string(),
    };

    crate::session::truncate_chars(&text, PREVIEW_WIDTH)
}

/// バックアップ先のパス名を組み立てる (`<path>.bak.<YYYYMMDDHHMMSS>`)。
pub fn backup_path_for(path: &Path, stamp: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(".bak.{stamp}"));
    PathBuf::from(s)
}

/// `shutil.copy2` 相当。内容 + パーミッション + タイムスタンプを複製する。
fn copy2(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst)
        .with_context(|| format!("バックアップを作成できない: {}", dst.display()))?;
    if let Ok(meta) = std::fs::metadata(src) {
        let atime = filetime::FileTime::from_last_access_time(&meta);
        let mtime = filetime::FileTime::from_last_modification_time(&meta);
        let _ = filetime::set_file_times(dst, atime, mtime);
    }
    Ok(())
}

/// Python の `json.dumps(..., ensure_ascii=False)` と同じ区切り (`", "` / `": "`) で書く。
struct PyFormatter;

impl serde_json::ser::Formatter for PyFormatter {
    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        if first { Ok(()) } else { writer.write_all(b", ") }
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        if first { Ok(()) } else { writer.write_all(b", ") }
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        writer.write_all(b": ")
    }
}

/// 1 エントリを Python 互換の書式で文字列化する。
pub fn dumps(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PyFormatter);
    serde::Serialize::serialize(value, &mut ser).expect("JSON シリアライズに失敗");
    String::from_utf8(buf).expect("JSON は常に UTF-8")
}

/// マージ後のエントリ列を jsonl として書き出す。
pub fn write_jsonl(path: &Path, entries: &[Value]) -> Result<()> {
    let file = File::create(path)
        .with_context(|| format!("書き込みできない: {}", path.display()))?;
    let mut w = BufWriter::new(file);
    for entry in entries {
        w.write_all(dumps(entry).as_bytes())?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    Ok(())
}

/// マージ本体。`out` に Python 版と同じ文言を書く。
pub fn merge(
    session_a_path: &Path,
    session_b_path: &Path,
    opts: MergeOptions,
    out: &mut dyn Write,
) -> Result<MergeReport> {
    let mut uuid_gen = RandomUuidGen;
    merge_with(session_a_path, session_b_path, opts, out, &mut uuid_gen, &now_stamp())
}

/// 現在時刻から `YYYYMMDDHHMMSS` を作る (ローカルタイム)。
pub fn now_stamp() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// UUID 採番とタイムスタンプを差し替えられる本体 (テスト用)。
pub fn merge_with<G: UuidGen>(
    session_a_path: &Path,
    session_b_path: &Path,
    opts: MergeOptions,
    out: &mut dyn Write,
    uuid_gen: &mut G,
    stamp: &str,
) -> Result<MergeReport> {
    let a_entries = load_jsonl(session_a_path)?;
    let b_entries = load_jsonl(session_b_path)?;

    let plan = build_plan(a_entries, &b_entries, opts, uuid_gen)?;

    let mut report = MergeReport {
        mode: plan.mode,
        a_entries: plan.a_entries,
        b_conversation: plan.b_conversation,
        merged: plan.merged.len(),
        session_id: plan.session_id.clone(),
        backup_path: None,
    };

    if opts.dry_run {
        writeln!(out, "モード: {}", plan.mode)?;
        writeln!(out, "セッションA: {}", session_a_path.display())?;
        writeln!(out, "  メッセージ数: {}", plan.a_entries)?;
        writeln!(out, "  sessionId: {}", plan.session_id)?;
        writeln!(out, "セッションB: {}", session_b_path.display())?;
        writeln!(out, "  会話メッセージ数: {}", plan.b_conversation)?;
        writeln!(out, "マージ後: {} エントリ", plan.merged.len())?;
        writeln!(out)?;
        writeln!(out, "--- セッションBの会話プレビュー ---")?;
        for msg in plan.preview.iter().take(PREVIEW_LIMIT) {
            let role = entry_type(msg).unwrap_or("?");
            writeln!(out, "  [{}] {}", role, preview_content(msg))?;
        }
        if plan.preview.len() > PREVIEW_LIMIT {
            writeln!(
                out,
                "  ... 他 {} メッセージ",
                plan.preview.len() - PREVIEW_LIMIT
            )?;
        }
    } else {
        let backup = backup_path_for(session_a_path, stamp);
        copy2(session_a_path, &backup)?;
        writeln!(out, "バックアップ: {}", backup.display())?;

        write_jsonl(session_a_path, &plan.merged)?;

        writeln!(
            out,
            "マージ完了: {} メッセージを{}",
            plan.rewritten_b.len(),
            plan.mode
        )?;
        writeln!(
            out,
            "次のステップ: claude --resume {} して /compact",
            plan.session_id
        )?;
        report.backup_path = Some(backup);
    }

    Ok(report)
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use std::fs;

    /// テスト用の決定的 UUID ジェネレータ。
    struct SeqGen {
        n: usize,
    }
    impl SeqGen {
        fn new() -> Self {
            Self { n: 0 }
        }
    }
    impl UuidGen for SeqGen {
        fn next_uuid(&mut self) -> String {
            self.n += 1;
            format!("uuid_gen-{:04}", self.n)
        }
        fn next_hex24(&mut self) -> String {
            self.n += 1;
            format!("{:024}", self.n)
        }
    }

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    /// ヘッダ 1 行 + user + assistant のセッション A。
    fn session_a() -> Vec<Value> {
        vec![
            v(r#"{"type":"summary","sessionId":"AAA","uuid":"h1"}"#),
            v(r#"{"type":"user","sessionId":"AAA","uuid":"a1","parentUuid":null,"message":{"role":"user","content":"Aの質問"}}"#),
            v(r#"{"type":"assistant","sessionId":"AAA","uuid":"a2","parentUuid":"a1","requestId":"req_orig","message":{"role":"assistant","content":[{"type":"text","text":"Aの返答"}]}}"#),
        ]
    }

    fn session_b() -> Vec<Value> {
        vec![
            v(r#"{"type":"summary","sessionId":"BBB","uuid":"bh"}"#),
            v(r#"{"type":"user","sessionId":"BBB","uuid":"b1","parentUuid":null,"message":{"role":"user","content":"Bの質問"}}"#),
            v(r#"{"type":"assistant","sessionId":"BBB","uuid":"b2","parentUuid":"b1","requestId":"req_bbb","message":{"role":"assistant","content":[{"type":"text","text":"Bの返答"}]}}"#),
            v(r#"{"type":"system","sessionId":"BBB","uuid":"b3"}"#),
        ]
    }

    fn uuid_of(e: &Value) -> Option<&str> {
        e.get("uuid").and_then(Value::as_str)
    }
    fn parent_of(e: &Value) -> &Value {
        e.get("parentUuid").unwrap_or(&Value::Null)
    }

    // ---- 抽出・探索 ----

    #[test]
    fn 会話だけ抽出する() {
        let conv = extract_conversation(&session_b());
        assert_eq!(conv.len(), 2);
        assert_eq!(entry_type(&conv[0]), Some("user"));
        assert_eq!(entry_type(&conv[1]), Some("assistant"));
    }

    #[test]
    fn 最初のuser位置を返す() {
        assert_eq!(find_first_user_index(&session_a()), 1);
        assert_eq!(find_first_user_index(&[]), 0);
        // user が無ければ len を返す (Python 版と同じ)
        let no_user = vec![v(r#"{"type":"summary"}"#)];
        assert_eq!(find_first_user_index(&no_user), 1);
    }

    #[test]
    fn 最後の会話uuidを返す() {
        assert_eq!(find_last_message_uuid(&session_a()).as_deref(), Some("a2"));
        assert_eq!(find_last_message_uuid(&[]), None);
        // 末尾に非会話行があっても会話行を遡って見つける
        let mut e = session_a();
        e.push(v(r#"{"type":"system","uuid":"sys"}"#));
        assert_eq!(find_last_message_uuid(&e).as_deref(), Some("a2"));
    }

    #[test]
    fn uuidがnullや空なら無しとみなす() {
        let e = vec![v(r#"{"type":"user","uuid":null}"#)];
        assert_eq!(find_last_message_uuid(&e), None);
        let e = vec![v(r#"{"type":"user","uuid":""}"#)];
        assert_eq!(find_last_message_uuid(&e), None);
        let e = vec![v(r#"{"type":"user"}"#)];
        assert_eq!(find_last_message_uuid(&e), None);
    }

    // ---- 再採番 ----

    #[test]
    fn UUID連鎖を張り直す() {
        let conv = extract_conversation(&session_b());
        let mut uuid_gen = SeqGen::new();
        let (rewritten, last) =
            rewrite_messages(&conv, "AAA", Value::String("root".into()), &mut uuid_gen);

        assert_eq!(rewritten.len(), 2);
        assert_eq!(parent_of(&rewritten[0]), &Value::String("root".into()));
        // 2 件目の親は 1 件目の新 uuid
        assert_eq!(parent_of(&rewritten[1]).as_str(), uuid_of(&rewritten[0]));
        assert_eq!(last.as_str(), uuid_of(&rewritten[1]));
        // sessionId は A のものへ差し替え
        for e in &rewritten {
            assert_eq!(e.get("sessionId").unwrap(), "AAA");
            assert_eq!(e.get("isSidechain").unwrap(), &Value::Bool(false));
        }
    }

    #[test]
    fn assistantのrequestIdだけ再生成する() {
        let conv = extract_conversation(&session_b());
        let mut uuid_gen = SeqGen::new();
        let (rewritten, _) = rewrite_messages(&conv, "AAA", Value::Null, &mut uuid_gen);
        assert!(rewritten[0].get("requestId").is_none()); // user は元から持たない
        let rid = rewritten[1].get("requestId").unwrap().as_str().unwrap();
        assert!(rid.starts_with("req_merged_"), "requestId={rid}");
        assert_eq!(rid.len(), "req_merged_".len() + 24);
    }

    #[test]
    fn requestIdを持たないassistantには足さない() {
        let conv = vec![v(r#"{"type":"assistant","uuid":"x","message":{"role":"assistant","content":"y"}}"#)];
        let mut uuid_gen = SeqGen::new();
        let (rewritten, _) = rewrite_messages(&conv, "AAA", Value::Null, &mut uuid_gen);
        assert!(rewritten[0].get("requestId").is_none());
    }

    #[test]
    fn 親がnullでも連鎖を張れる() {
        let conv = extract_conversation(&session_b());
        let mut uuid_gen = SeqGen::new();
        let (rewritten, _) = rewrite_messages(&conv, "AAA", Value::Null, &mut uuid_gen);
        assert_eq!(parent_of(&rewritten[0]), &Value::Null);
    }

    #[test]
    fn 元のキー順を保つ() {
        let conv = vec![v(r#"{"type":"user","uuid":"b1","parentUuid":null,"cwd":"/x","message":{"role":"user","content":"q"}}"#)];
        let mut uuid_gen = SeqGen::new();
        let (rewritten, _) = rewrite_messages(&conv, "AAA", Value::Null, &mut uuid_gen);
        let keys: Vec<&str> = rewritten[0].as_object().unwrap().keys().map(String::as_str).collect();
        // 既存キーは位置維持、新規キー (sessionId/isSidechain) は末尾に付く
        assert_eq!(keys, vec!["type", "uuid", "parentUuid", "cwd", "message", "sessionId", "isSidechain"]);
    }

    // ---- プラン構築 ----

    #[test]
    fn 先頭挿入はヘッダの直後に入る() {
        let mut uuid_gen = SeqGen::new();
        let plan = build_plan(session_a(), &session_b(), MergeOptions::default(), &mut uuid_gen).unwrap();

        assert_eq!(plan.mode, MODE_INSERT);
        assert_eq!(plan.merged.len(), 5); // A3 + B会話2
        assert_eq!(entry_type(&plan.merged[0]), Some("summary"));
        // 1,2 番目が B 由来
        assert_eq!(plan.merged[1].get("sessionId").unwrap(), "AAA");
        assert_eq!(
            plan.merged[1].get("message").unwrap().get("content").unwrap(),
            "Bの質問"
        );
        // A の最初の user の親が B の最後の uuid になっている
        assert_eq!(parent_of(&plan.merged[3]).as_str(), uuid_of(&plan.merged[2]));
        assert_eq!(
            plan.merged[3].get("message").unwrap().get("content").unwrap(),
            "Aの質問"
        );
    }

    #[test]
    fn 末尾追加はAの後ろに繋がる() {
        let mut uuid_gen = SeqGen::new();
        let opts = MergeOptions { append: true, ..Default::default() };
        let plan = build_plan(session_a(), &session_b(), opts, &mut uuid_gen).unwrap();

        assert_eq!(plan.mode, MODE_APPEND);
        assert_eq!(plan.merged.len(), 5);
        // 先頭 3 件は A のまま (parentUuid も書き換わらない)
        assert_eq!(uuid_of(&plan.merged[0]), Some("h1"));
        assert_eq!(uuid_of(&plan.merged[1]), Some("a1"));
        assert_eq!(parent_of(&plan.merged[1]), &Value::Null);
        // 4 件目 (B の 1 件目) の親は A の最後の会話 uuid
        assert_eq!(parent_of(&plan.merged[3]), &Value::String("a2".into()));
        assert_eq!(parent_of(&plan.merged[4]).as_str(), uuid_of(&plan.merged[3]));
    }

    #[test]
    fn 挿入と追加で位置が変わる() {
        let mut g1 = SeqGen::new();
        let insert = build_plan(session_a(), &session_b(), MergeOptions::default(), &mut g1).unwrap();
        let mut g2 = SeqGen::new();
        let append = build_plan(
            session_a(),
            &session_b(),
            MergeOptions { append: true, ..Default::default() },
            &mut g2,
        )
        .unwrap();

        let content = |p: &MergePlan, i: usize| {
            p.merged[i].get("message").unwrap().get("content").unwrap().to_string()
        };
        assert!(content(&insert, 1).contains("Bの質問"));
        assert!(content(&append, 1).contains("Aの質問"));
        assert!(content(&append, 3).contains("Bの質問"));
        assert_eq!(insert.merged.len(), append.merged.len());
    }

    #[test]
    fn 件数計算が正しい() {
        let mut uuid_gen = SeqGen::new();
        let plan = build_plan(session_a(), &session_b(), MergeOptions::default(), &mut uuid_gen).unwrap();
        assert_eq!(plan.a_entries, 3);
        assert_eq!(plan.b_conversation, 2);
        assert_eq!(plan.merged.len(), 5);
        assert_eq!(plan.rewritten_b.len(), 2);
    }

    // ---- 異常系 ----

    #[test]
    fn AにsessionIdが無ければエラー() {
        let a = vec![v(r#"{"type":"summary"}"#)];
        let mut uuid_gen = SeqGen::new();
        let err = build_plan(a, &session_b(), MergeOptions::default(), &mut uuid_gen).unwrap_err();
        assert_eq!(err.to_string(), "エラー: セッションAからsessionIdが見つからない");
    }

    #[test]
    fn Bに会話が無ければエラー() {
        let b = vec![v(r#"{"type":"system","sessionId":"BBB"}"#)];
        let mut uuid_gen = SeqGen::new();
        let err = build_plan(session_a(), &b, MergeOptions::default(), &mut uuid_gen).unwrap_err();
        assert_eq!(err.to_string(), "エラー: セッションBに会話メッセージがない");
    }

    #[test]
    fn Aの先頭がuserならヘッダー無しエラー() {
        let a = vec![
            v(r#"{"type":"user","sessionId":"AAA","uuid":"a1","message":{"role":"user","content":"q"}}"#),
        ];
        let mut uuid_gen = SeqGen::new();
        let err = build_plan(a, &session_b(), MergeOptions::default(), &mut uuid_gen).unwrap_err();
        assert_eq!(err.to_string(), "エラー: セッションAにヘッダーがない");
    }

    #[test]
    fn 末尾追加でAに会話が無ければエラー() {
        let a = vec![v(r#"{"type":"summary","sessionId":"AAA","uuid":"h1"}"#)];
        let mut uuid_gen = SeqGen::new();
        let opts = MergeOptions { append: true, ..Default::default() };
        let err = build_plan(a, &session_b(), opts, &mut uuid_gen).unwrap_err();
        assert_eq!(err.to_string(), "エラー: セッションAにメッセージがない");
    }

    #[test]
    fn 先頭挿入でuserが一つも無ければエラー() {
        // Python 版は IndexError で落ちる箇所。明示的なエラーにしている
        let a = vec![
            v(r#"{"type":"summary","sessionId":"AAA","uuid":"h1"}"#),
            v(r#"{"type":"assistant","sessionId":"AAA","uuid":"h2","message":{"role":"assistant","content":"x"}}"#),
        ];
        let mut uuid_gen = SeqGen::new();
        let err = build_plan(a, &session_b(), MergeOptions::default(), &mut uuid_gen).unwrap_err();
        assert_eq!(err.to_string(), "エラー: セッションAにuserメッセージがない");
    }

    // ---- jsonl 入出力 ----

    #[test]
    fn 空行を飛ばしてパースする() {
        let text = "{\"a\":1}\n\n  \n{\"b\":2}\n";
        let parsed = parse_jsonl(text).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn 壊れた行は行番号つきでエラー() {
        let err = parse_jsonl("{\"a\":1}\n{壊れ\n").unwrap_err();
        assert!(err.to_string().contains("2 行目"), "{err}");
    }

    #[test]
    fn Python互換の区切りで書き出す() {
        let value = v(r#"{"a":1,"b":["x","y"],"c":"日本語"}"#);
        // ensure_ascii=False 相当 + separators=(", ", ": ")
        assert_eq!(dumps(&value), r#"{"a": 1, "b": ["x", "y"], "c": "日本語"}"#);
    }

    #[test]
    fn バックアップ名の規則() {
        let p = backup_path_for(Path::new("/tmp/s.jsonl"), "20260725135300");
        assert_eq!(p, PathBuf::from("/tmp/s.jsonl.bak.20260725135300"));
    }

    #[test]
    fn 現在時刻スタンプは14桁の数字() {
        let s = now_stamp();
        assert_eq!(s.len(), 14);
        assert!(s.chars().all(|c| c.is_ascii_digit()), "stamp={s}");
    }

    // ---- プレビュー ----

    #[test]
    fn プレビューは文字列contentをそのまま出す() {
        let e = v(r#"{"type":"user","message":{"role":"user","content":"短い質問"}}"#);
        assert_eq!(preview_content(&e), "短い質問");
    }

    #[test]
    fn プレビューはブロック配列を連結する() {
        let e = v(r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"答え"}]}}"#);
        assert_eq!(preview_content(&e), "[thinking...]答え");
    }

    #[test]
    fn プレビューは80文字で切る() {
        let long = "あ".repeat(200);
        let e = serde_json::json!({"type":"user","message":{"role":"user","content": long}});
        assert_eq!(preview_content(&e).chars().count(), 80);
    }

    #[test]
    fn プレビューはmessageが無くても空文字() {
        assert_eq!(preview_content(&v(r#"{"type":"user"}"#)), "");
        assert_eq!(preview_content(&v(r#"{"type":"user","message":{}}"#)), "");
    }

    // ---- ファイル経由の統合 ----

    fn write_session(dir: &Path, name: &str, entries: &[Value]) -> PathBuf {
        let p = dir.join(name);
        let body: String = entries
            .iter()
            .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
            .collect();
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn dry_runはファイルを変更せずバックアップも作らない() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write_session(tmp.path(), "a.jsonl", &session_a());
        let b = write_session(tmp.path(), "b.jsonl", &session_b());
        let before = fs::read_to_string(&a).unwrap();

        let mut out = Vec::new();
        let mut uuid_gen = SeqGen::new();
        let opts = MergeOptions { dry_run: true, append: false };
        let report = merge_with(&a, &b, opts, &mut out, &mut uuid_gen, "20260725000000").unwrap();

        assert_eq!(fs::read_to_string(&a).unwrap(), before);
        assert!(report.backup_path.is_none());
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 2);

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("モード: 先頭に挿入"));
        assert!(text.contains("  メッセージ数: 3"));
        assert!(text.contains("  sessionId: AAA"));
        assert!(text.contains("  会話メッセージ数: 2"));
        assert!(text.contains("マージ後: 5 エントリ"));
        assert!(text.contains("--- セッションBの会話プレビュー ---"));
        assert!(text.contains("  [user] Bの質問"));
        assert!(text.contains("  [assistant] Bの返答"));
        assert!(!text.contains("他 "));
    }

    #[test]
    fn dry_runのモード表示がappendで変わる() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write_session(tmp.path(), "a.jsonl", &session_a());
        let b = write_session(tmp.path(), "b.jsonl", &session_b());

        let mut out = Vec::new();
        let mut uuid_gen = SeqGen::new();
        let opts = MergeOptions { dry_run: true, append: true };
        merge_with(&a, &b, opts, &mut out, &mut uuid_gen, "20260725000000").unwrap();
        assert!(String::from_utf8(out).unwrap().contains("モード: 末尾に追加"));
    }

    #[test]
    fn プレビューは10件までで残数を出す() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write_session(tmp.path(), "a.jsonl", &session_a());
        let mut b = vec![v(r#"{"type":"summary","sessionId":"BBB","uuid":"bh"}"#)];
        for i in 0..13 {
            b.push(serde_json::json!({
                "type": "user", "sessionId": "BBB", "uuid": format!("b{i}"),
                "message": {"role": "user", "content": format!("質問{i}")}
            }));
        }
        let bp = write_session(tmp.path(), "b.jsonl", &b);

        let mut out = Vec::new();
        let mut uuid_gen = SeqGen::new();
        let opts = MergeOptions { dry_run: true, append: false };
        merge_with(&a, &bp, opts, &mut out, &mut uuid_gen, "20260725000000").unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("  [user] 質問9"));
        assert!(!text.contains("質問10"));
        assert!(text.contains("... 他 3 メッセージ"));
    }

    #[test]
    fn 実行するとバックアップを作って上書きする() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write_session(tmp.path(), "a.jsonl", &session_a());
        let b = write_session(tmp.path(), "b.jsonl", &session_b());
        let before = fs::read_to_string(&a).unwrap();

        let mut out = Vec::new();
        let mut uuid_gen = SeqGen::new();
        let report = merge_with(&a, &b, MergeOptions::default(), &mut out, &mut uuid_gen, "20260725013000")
            .unwrap();

        let backup = report.backup_path.clone().unwrap();
        assert_eq!(backup.file_name().unwrap(), "a.jsonl.bak.20260725013000");
        // バックアップは元の内容そのまま
        assert_eq!(fs::read_to_string(&backup).unwrap(), before);

        // 本体はマージ後 5 行
        let merged_text = fs::read_to_string(&a).unwrap();
        let lines: Vec<&str> = merged_text.lines().collect();
        assert_eq!(lines.len(), 5);
        // 全行が JSON として読み直せる
        let reparsed = parse_jsonl(&merged_text).unwrap();
        assert_eq!(reparsed.len(), 5);
        // 全 uuid がユニークで、親子連鎖が壊れていない
        let uuids: Vec<&str> = reparsed.iter().filter_map(|e| uuid_of(e)).collect();
        let uniq: std::collections::HashSet<&&str> = uuids.iter().collect();
        assert_eq!(uuids.len(), uniq.len());

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("バックアップ: "));
        assert!(text.contains("マージ完了: 2 メッセージを先頭に挿入"));
        assert!(text.contains("次のステップ: claude --resume AAA して /compact"));
    }

    #[test]
    fn 実行後もsessionIdがAのまま揃う() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write_session(tmp.path(), "a.jsonl", &session_a());
        let b = write_session(tmp.path(), "b.jsonl", &session_b());

        let mut out = Vec::new();
        let mut uuid_gen = SeqGen::new();
        merge_with(&a, &b, MergeOptions::default(), &mut out, &mut uuid_gen, "20260725013000").unwrap();

        let entries = load_jsonl(&a).unwrap();
        for e in &entries {
            if let Some(sid) = e.get("sessionId") {
                assert_eq!(sid, "AAA");
            }
        }
    }

    #[test]
    fn 本番UUIDはv4でユニーク() {
        let mut uuid_gen = RandomUuidGen;
        let a = uuid_gen.next_uuid();
        let b = uuid_gen.next_uuid();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        let parsed = Uuid::parse_str(&a).unwrap();
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(uuid_gen.next_hex24().len(), 24);
    }

    #[test]
    fn 存在しないファイルはエラー() {
        let tmp = tempfile::tempdir().unwrap();
        let b = write_session(tmp.path(), "b.jsonl", &session_b());
        let mut out = Vec::new();
        let mut uuid_gen = SeqGen::new();
        let err = merge_with(
            &tmp.path().join("無い.jsonl"),
            &b,
            MergeOptions::default(),
            &mut out,
            &mut uuid_gen,
            "20260725000000",
        )
        .unwrap_err();
        assert!(err.to_string().contains("読み込めない"), "{err}");
    }
}
