//! `~/.claude/tasks/<sessionId>/<taskId>.json` の読み取り (閲覧のみ)。

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use walkdir::WalkDir;

/// セッション単位のタスク集計。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSummary {
    pub pending: u32,
    pub in_progress: u32,
    pub completed: u32,
    /// その他 (未知のステータス)
    pub other: u32,
    /// チケット番号抽出に使う subject 一覧
    pub subjects: Vec<String>,
}

impl TaskSummary {
    pub fn total(&self) -> u32 {
        self.pending + self.in_progress + self.completed + self.other
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// 一覧表示用の `P/I/D` 形式。タスクが無ければ空文字。
    pub fn format_counts(&self) -> String {
        if self.is_empty() {
            String::new()
        } else {
            format!("{}/{}/{}", self.pending, self.in_progress, self.completed)
        }
    }

    /// 1 タスク分の JSON を取り込む。
    pub fn absorb(&mut self, value: &Value) {
        match value.get("status").and_then(Value::as_str) {
            Some("pending") => self.pending += 1,
            Some("in_progress") => self.in_progress += 1,
            Some("completed") => self.completed += 1,
            _ => self.other += 1,
        }
        if let Some(s) = value.get("subject").and_then(Value::as_str)
            && !s.is_empty()
        {
            self.subjects.push(s.to_string());
        }
    }
}

/// tasks ディレクトリ全体を読み、sessionId をキーにした集計を返す。
///
/// タスクファイル自身は sessionId を持たず、親ディレクトリ名でのみ紐づく。
/// ディレクトリが無い場合は空の結果を返す (エラーにしない)。
pub fn load_all(tasks_dir: &Path) -> HashMap<String, TaskSummary> {
    let mut out: HashMap<String, TaskSummary> = HashMap::new();
    if !tasks_dir.is_dir() {
        return out;
    }

    for entry in WalkDir::new(tasks_dir)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let session_id = match path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            // 壊れたタスクファイルは黙って飛ばす (一覧表示を止めない)
            Err(_) => continue,
        };
        out.entry(session_id).or_default().absorb(&value);
    }

    out
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use std::fs;

    fn write_task(dir: &Path, session: &str, id: &str, status: &str, subject: &str) {
        let d = dir.join(session);
        fs::create_dir_all(&d).unwrap();
        let body = serde_json::json!({
            "id": id,
            "subject": subject,
            "description": "",
            "activeForm": "",
            "status": status,
            "blocks": [],
            "blockedBy": []
        });
        fs::write(d.join(format!("{id}.json")), body.to_string()).unwrap();
    }

    #[test]
    fn ステータス別に集計する() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_task(root, "s1", "1", "pending", "[#55710] やること");
        write_task(root, "s1", "2", "in_progress", "[#2] 作業中");
        write_task(root, "s1", "3", "completed", "終わった");
        write_task(root, "s1", "4", "completed", "これも終わった");
        write_task(root, "s2", "1", "pending", "別セッション");

        let all = load_all(root);
        let s1 = &all["s1"];
        assert_eq!(s1.pending, 1);
        assert_eq!(s1.in_progress, 1);
        assert_eq!(s1.completed, 2);
        assert_eq!(s1.total(), 4);
        assert_eq!(s1.format_counts(), "1/1/2");
        assert_eq!(s1.subjects.len(), 4);
        assert_eq!(all["s2"].pending, 1);
    }

    #[test]
    fn ディレクトリが無ければ空() {
        let tmp = tempfile::tempdir().unwrap();
        let all = load_all(&tmp.path().join("存在しない"));
        assert!(all.is_empty());
    }

    #[test]
    fn 壊れたJSONは飛ばして続行する() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let d = root.join("s1");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("broken.json"), "{壊れてる").unwrap();
        write_task(root, "s1", "2", "pending", "生きてる");

        let all = load_all(root);
        assert_eq!(all["s1"].total(), 1);
    }

    #[test]
    fn json以外のファイルは無視する() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let d = root.join("s1");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("メモ.txt"), "無関係").unwrap();
        assert!(load_all(root).is_empty());
    }

    #[test]
    fn 未知ステータスはotherに入る() {
        let mut s = TaskSummary::default();
        s.absorb(&serde_json::json!({"status": "deleted", "subject": "消えたはず"}));
        s.absorb(&serde_json::json!({"subject": "ステータス無し"}));
        assert_eq!(s.other, 2);
        assert_eq!(s.format_counts(), "0/0/0");
    }

    #[test]
    fn タスク無しは空文字を返す() {
        assert_eq!(TaskSummary::default().format_counts(), "");
        assert!(TaskSummary::default().is_empty());
    }
}
