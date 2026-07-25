//! `~/.claude/sessions/<pid>.json` (実行中プロセスのレジストリ) の読み取り。
//!
//! 実行中セッションの jsonl を誤って削除・アーカイブしないためのガードに使う。

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

/// 実行中セッション 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningSession {
    pub pid: i64,
    pub session_id: String,
    pub cwd: Option<String>,
    pub name: Option<String>,
    /// `idle` / `busy` など
    pub status: Option<String>,
    /// `interactive` / `bg`
    pub kind: Option<String>,
}

impl RunningSession {
    /// 一覧に出す短い表示。
    pub fn label(&self) -> String {
        match (&self.kind, &self.status) {
            (Some(k), Some(s)) => format!("{k}/{s}"),
            (Some(k), None) => k.clone(),
            (None, Some(s)) => s.clone(),
            (None, None) => "running".to_string(),
        }
    }
}

/// レジストリを読み、sessionId をキーにして返す。
///
/// 同じ sessionId が複数 pid にある場合は最初に読めたものを採用する。
/// ディレクトリが無い場合は空 (エラーにしない)。
pub fn load(sessions_dir: &Path) -> HashMap<String, RunningSession> {
    let mut out = HashMap::new();
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let session_id = match value.get("sessionId").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let running = RunningSession {
            pid: value.get("pid").and_then(Value::as_i64).unwrap_or(0),
            session_id: session_id.clone(),
            cwd: str_field(&value, "cwd"),
            name: str_field(&value, "name"),
            status: str_field(&value, "status"),
            kind: str_field(&value, "kind"),
        };
        out.entry(session_id).or_insert(running);
    }

    out
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn 実行中セッションを読める() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("12063.json"),
            r#"{"pid":12063,"sessionId":"9ef3bb4b","cwd":"/Users/work/repo","kind":"bg","name":"作業中","status":"idle"}"#,
        )
        .unwrap();

        let all = load(tmp.path());
        let s = &all["9ef3bb4b"];
        assert_eq!(s.pid, 12063);
        assert_eq!(s.cwd.as_deref(), Some("/Users/work/repo"));
        assert_eq!(s.label(), "bg/idle");
    }

    #[test]
    fn ディレクトリが無ければ空() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load(&tmp.path().join("無い")).is_empty());
    }

    #[test]
    fn 壊れたファイルとsessionId無しは飛ばす() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("1.json"), "{壊れ").unwrap();
        fs::write(tmp.path().join("2.json"), r#"{"pid":2}"#).unwrap();
        fs::write(tmp.path().join("3.txt"), r#"{"sessionId":"x"}"#).unwrap();
        fs::write(tmp.path().join("4.json"), r#"{"pid":4,"sessionId":"ok","status":"busy"}"#).unwrap();

        let all = load(tmp.path());
        assert_eq!(all.len(), 1);
        assert_eq!(all["ok"].label(), "busy");
    }
}
