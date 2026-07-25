//! 一覧から実行する操作 (resume / 削除 / アーカイブ / 要約)。
//!
//! プロセス起動を伴うものは「コマンドの組み立て」と「実行」を分け、
//! 組み立て側だけテストできるようにしてある。

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths::{Paths, claude_bin, recap_model};
use crate::rows::SessionRow;

/// 実行するコマンドの内容 (テストしやすいように構造体で持つ)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl CommandSpec {
    /// `std::process::Command` に変換する。
    pub fn to_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).current_dir(&self.cwd);
        cmd
    }

    /// 表示用の 1 行。
    pub fn display(&self) -> String {
        format!("cd {} && {} {}", self.cwd.display(), self.program, self.args.join(" "))
    }
}

/// resume 用のコマンドを組み立てる。cwd が現存しない場合はエラー。
pub fn resume_command(row: &SessionRow) -> Result<CommandSpec> {
    let cwd = match &row.cwd {
        Some(c) => PathBuf::from(c),
        None => bail!("cwd が記録されていないので resume できない"),
    };
    if !cwd.is_dir() {
        bail!("cwd が存在しない: {}", cwd.display());
    }
    Ok(CommandSpec {
        program: claude_bin(),
        args: vec!["--resume".to_string(), row.session_id.clone()],
        cwd,
    })
}

/// 要約 (recap) 用の既定プロンプト。
pub const RECAP_PROMPT: &str = "このセッションでやったことを日本語で要約して。\
出力は次の3項目だけ: 1) 何をやったか 2) 決まったこと 3) 残っている作業。\
各項目3行以内。前置き・結びの挨拶は書かない。";

/// recap 用のコマンドを組み立てる。
///
/// Anthropic API 鍵は扱わず、既存の claude CLI 認証をそのまま使う。
pub fn recap_command(row: &SessionRow, prompt: &str) -> Result<CommandSpec> {
    let cwd = match &row.cwd {
        Some(c) if Path::new(c).is_dir() => PathBuf::from(c),
        // cwd が消えていても要約自体は動かせるので、その場合は一時的に / で回す
        _ => PathBuf::from("/"),
    };
    Ok(CommandSpec {
        program: claude_bin(),
        args: vec![
            "--resume".to_string(),
            row.session_id.clone(),
            "-p".to_string(),
            prompt.to_string(),
            "--model".to_string(),
            recap_model(),
        ],
        cwd,
    })
}

/// recap を実行して標準出力を返す。
pub fn run_recap(row: &SessionRow) -> Result<String> {
    let spec = recap_command(row, RECAP_PROMPT)?;
    let output = spec
        .to_command()
        .output()
        .with_context(|| format!("{} を起動できない", spec.program))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("要約に失敗 ({}): {detail}", output.status);
    }
    Ok(stdout)
}

/// 削除・アーカイブの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveOutcome {
    pub jsonl: PathBuf,
    /// 併せて処理したタスクディレクトリ
    pub tasks_dir: Option<PathBuf>,
    /// アーカイブ時の移動先
    pub moved_to: Option<PathBuf>,
}

/// セッションを削除する (jsonl + `~/.claude/tasks/<sessionId>/`)。
///
/// 実行中チェックは呼び出し側 (TUI) で確認を挟んでから呼ぶこと。
///
/// `remove_tasks` が false のときはタスクを残す。同じ sessionId の jsonl が
/// 別プロジェクトディレクトリにも存在する場合、タスクは共有物なので消してはいけない
/// (実データに同一 ID が複数ディレクトリへ散らばっている例がある)。
pub fn delete_session(paths: &Paths, row: &SessionRow, remove_tasks: bool) -> Result<RemoveOutcome> {
    std::fs::remove_file(&row.path)
        .with_context(|| format!("削除できない: {}", row.path.display()))?;

    let tasks_dir = paths.tasks().join(&row.session_id);
    let removed_tasks = if remove_tasks && tasks_dir.is_dir() {
        std::fs::remove_dir_all(&tasks_dir)
            .with_context(|| format!("タスクを削除できない: {}", tasks_dir.display()))?;
        Some(tasks_dir)
    } else {
        None
    };

    Ok(RemoveOutcome {
        jsonl: row.path.clone(),
        tasks_dir: removed_tasks,
        moved_to: None,
    })
}

/// アーカイブ先のパスを決める。同名があれば `-2`, `-3` と連番を振る。
pub fn archive_destination(archive_root: &Path, project_dir: &str, file_name: &str) -> PathBuf {
    let dir = archive_root.join(project_dir);
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (file_name.to_string(), String::new()),
    };
    for n in 2..1000 {
        let c = dir.join(format!("{stem}-{n}{ext}"));
        if !c.exists() {
            return c;
        }
    }
    candidate
}

/// セッションをアーカイブする (projects-archive へ移動)。
///
/// タスクは残す。jsonl を戻せば元の状態に復帰できる。
pub fn archive_session(paths: &Paths, row: &SessionRow) -> Result<RemoveOutcome> {
    let file_name = crate::paths::file_name_str(&row.path);
    let dest = archive_destination(&paths.projects_archive(), &row.project_dir, file_name);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("アーカイブ先を作成できない: {}", dir.display()))?;
    }

    // 同一ボリューム内なら rename、跨ぐ場合は copy + remove
    if std::fs::rename(&row.path, &dest).is_err() {
        std::fs::copy(&row.path, &dest)
            .with_context(|| format!("アーカイブへコピーできない: {}", dest.display()))?;
        std::fs::remove_file(&row.path)
            .with_context(|| format!("移動元を削除できない: {}", row.path.display()))?;
    }

    Ok(RemoveOutcome {
        jsonl: row.path.clone(),
        tasks_dir: None,
        moved_to: Some(dest),
    })
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use crate::registry::RunningSession;
    use crate::rows;
    use crate::scan::{ScanTarget, ScannedSession};
    use crate::session::TitleKind;
    use crate::tasks::TaskSummary;
    use crate::worklog::WorklogSummary;
    use std::collections::HashMap;
    use std::fs;

    fn row_for(path: &Path, session_id: &str, cwd: Option<&str>) -> SessionRow {
        let scanned = ScannedSession {
            target: ScanTarget {
                path: path.to_path_buf(),
                kind: crate::scan::EntryKind::Session,
                project_dir: "-Users-work--ghq-repo".into(),
                file_stem: session_id.into(),
                size: 1,
                mtime_ns: 0,
                created: Some(std::time::SystemTime::UNIX_EPOCH),
                modified: None,
            },
            session_id: session_id.into(),
            cwd: cwd.map(str::to_string),
            title: "タイトル".into(),
            title_kind: TitleKind::Ai,
            first_prompt: None,
            line_count: 1,
        };
        let tasks: HashMap<String, TaskSummary> = HashMap::new();
        let running: HashMap<String, RunningSession> = HashMap::new();
        let wl: HashMap<String, WorklogSummary> = HashMap::new();
        let tags: HashMap<String, Vec<String>> = HashMap::new();
        rows::build(vec![scanned], &tasks, &running, &wl, &tags)
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn resumeコマンドを組み立てる() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().to_str().unwrap();
        let row = row_for(&tmp.path().join("s.jsonl"), "abc-123", Some(cwd));
        let spec = resume_command(&row).unwrap();
        assert_eq!(spec.args, vec!["--resume", "abc-123"]);
        assert_eq!(spec.cwd, tmp.path());
        assert!(spec.display().contains("--resume abc-123"));
    }

    #[test]
    fn cwdが無ければresumeできない() {
        let tmp = tempfile::tempdir().unwrap();
        let row = row_for(&tmp.path().join("s.jsonl"), "abc", None);
        let err = resume_command(&row).unwrap_err();
        assert!(err.to_string().contains("cwd が記録されていない"), "{err}");
    }

    #[test]
    fn cwdが消えていればresumeできない() {
        let tmp = tempfile::tempdir().unwrap();
        let row = row_for(&tmp.path().join("s.jsonl"), "abc", Some("/存在しないディレクトリ/x"));
        let err = resume_command(&row).unwrap_err();
        assert!(err.to_string().contains("cwd が存在しない"), "{err}");
    }

    #[test]
    fn recapコマンドを組み立てる() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().to_str().unwrap();
        let row = row_for(&tmp.path().join("s.jsonl"), "abc-123", Some(cwd));
        let spec = recap_command(&row, "要約して").unwrap();
        assert_eq!(
            spec.args,
            vec!["--resume", "abc-123", "-p", "要約して", "--model", &recap_model()]
        );
    }

    #[test]
    fn recapはcwdが無くても組み立てられる() {
        let tmp = tempfile::tempdir().unwrap();
        let row = row_for(&tmp.path().join("s.jsonl"), "abc", None);
        let spec = recap_command(&row, "p").unwrap();
        assert_eq!(spec.cwd, PathBuf::from("/"));
    }

    #[test]
    fn 削除はjsonlとタスクを消す() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_home = tmp.path().join("claude");
        let paths = Paths::new(&claude_home, tmp.path().join("data"));

        let proj = paths.projects().join("-Users-work--ghq-repo");
        fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("abc.jsonl");
        fs::write(&jsonl, "{}\n").unwrap();

        let task_dir = paths.tasks().join("abc");
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join("1.json"), "{}").unwrap();

        let row = row_for(&jsonl, "abc", None);
        let out = delete_session(&paths, &row, true).unwrap();

        assert!(!jsonl.exists());
        assert!(!task_dir.exists());
        assert_eq!(out.tasks_dir, Some(task_dir));
    }

    #[test]
    fn 共有タスクは残して削除できる() {
        // 同じ sessionId の jsonl が他にも残っている場合、タスクは消さない
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("claude"), tmp.path().join("data"));
        let proj = paths.projects().join("p");
        fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("abc.jsonl");
        fs::write(&jsonl, "{}\n").unwrap();

        let task_dir = paths.tasks().join("abc");
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join("1.json"), "{}").unwrap();

        let row = row_for(&jsonl, "abc", None);
        let out = delete_session(&paths, &row, false).unwrap();

        assert!(!jsonl.exists());
        assert!(task_dir.exists(), "共有タスクを消してはいけない");
        assert_eq!(out.tasks_dir, None);
    }

    #[test]
    fn タスクが無くても削除できる() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("claude"), tmp.path().join("data"));
        let proj = paths.projects().join("p");
        fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("abc.jsonl");
        fs::write(&jsonl, "{}\n").unwrap();

        let row = row_for(&jsonl, "abc", None);
        let out = delete_session(&paths, &row, true).unwrap();
        assert!(out.tasks_dir.is_none());
        assert!(!jsonl.exists());
    }

    #[test]
    fn アーカイブはディレクトリ構造を保って移動する() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("claude"), tmp.path().join("data"));
        let proj = paths.projects().join("-Users-work--ghq-repo");
        fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("abc.jsonl");
        fs::write(&jsonl, "中身\n").unwrap();

        let row = row_for(&jsonl, "abc", None);
        let out = archive_session(&paths, &row).unwrap();

        let dest = paths.projects_archive().join("-Users-work--ghq-repo").join("abc.jsonl");
        assert!(!jsonl.exists());
        assert!(dest.exists());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "中身\n");
        assert_eq!(out.moved_to, Some(dest));
    }

    #[test]
    fn アーカイブ先が埋まっていれば連番を振る() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("proj")).unwrap();
        assert_eq!(
            archive_destination(root, "proj", "a.jsonl"),
            root.join("proj").join("a.jsonl")
        );

        fs::write(root.join("proj").join("a.jsonl"), "x").unwrap();
        assert_eq!(
            archive_destination(root, "proj", "a.jsonl"),
            root.join("proj").join("a-2.jsonl")
        );

        fs::write(root.join("proj").join("a-2.jsonl"), "x").unwrap();
        assert_eq!(
            archive_destination(root, "proj", "a.jsonl"),
            root.join("proj").join("a-3.jsonl")
        );
    }

    #[test]
    fn 拡張子が無いファイル名でも連番を振れる() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("proj")).unwrap();
        fs::write(root.join("proj").join("noext"), "x").unwrap();
        assert_eq!(
            archive_destination(root, "proj", "noext"),
            root.join("proj").join("noext-2")
        );
    }
}
