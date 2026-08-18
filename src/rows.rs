//! 一覧に出す 1 行分のデータ組み立て。
//!
//! 描画から切り離して、ここだけでテストできるようにしてある。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Local};

use crate::fork::ForkGroup;
use crate::registry::RunningSession;
use crate::scan::ScannedSession;
use crate::session::TitleKind;
use crate::tasks::TaskSummary;
use crate::ticket;
use crate::worklog::WorklogSummary;

/// 一覧の 1 行。
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub session_id: String,
    pub path: PathBuf,
    /// `projects/<encoded-cwd>` のディレクトリ名
    pub project_dir: String,
    /// jsonl 内の cwd (ディレクトリ名からの逆算は非可逆なので必ずこちらを使う)
    pub cwd: Option<String>,
    /// cwd が現存するか (機能6: 消えた作業ディレクトリを一覧で見分ける)。
    /// cwd が記録されていない場合も resume できないので false 扱い。
    pub cwd_exists: bool,
    pub title: String,
    pub title_kind: TitleKind,
    pub tickets: Vec<u64>,
    pub tasks: TaskSummary,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub running: Option<RunningSession>,
    pub tags: Vec<String>,
    pub worklog: WorklogSummary,
    pub size: u64,
    pub line_count: i64,
    pub first_prompt: Option<String>,
    /// resume による分岐 (fork) の判定結果。`rows::build` の時点では判定できないため、
    /// 全セッションの走査が終わった後に [`apply_fork_marks`] で反映する
    pub fork: Option<ForkMark>,
    /// fuzzy 検索用に連結した文字列
    haystack: String,
}

/// [`SessionRow::fork`] の中身。同じ会話から分岐したセッション群のうち、
/// 自分がどの位置にいるかを表す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkMark {
    /// このセッションがグループの起源か
    pub is_root: bool,
    /// 自分以外の同グループ session_id
    pub group_members: Vec<String>,
}

impl SessionRow {
    /// 表示用の短縮 ID (先頭 8 桁)。
    pub fn short_id(&self) -> &str {
        let n = self
            .session_id
            .char_indices()
            .nth(8)
            .map(|(i, _)| i)
            .unwrap_or(self.session_id.len());
        &self.session_id[..n]
    }

    /// 作成日時 (`YYYY-MM-DD HH:MM`)。取れなければ空。
    pub fn format_created(&self) -> String {
        format_time(self.created)
    }

    /// 最終更新 (`YYYY-MM-DD HH:MM`)。
    pub fn format_modified(&self) -> String {
        format_time(self.modified)
    }

    /// `#55710 #55711` 形式。
    pub fn format_tickets(&self) -> String {
        ticket::format(&self.tickets)
    }

    /// `pending/in_progress/done`。
    pub fn format_tasks(&self) -> String {
        self.tasks.format_counts()
    }

    /// プロジェクトの短い表示 (cwd の末尾。無ければディレクトリ名)。
    pub fn project_label(&self) -> String {
        match &self.cwd {
            Some(cwd) => std::path::Path::new(cwd)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(cwd.as_str())
                .to_string(),
            None => self.project_dir.clone(),
        }
    }

    /// 実行中かどうか。
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// cwd が消えている (もしくは記録が無い) か (機能6)。
    /// このセッションは resume できないので、一覧で見分けられるようにする。
    pub fn cwd_missing(&self) -> bool {
        !self.cwd_exists
    }

    /// fuzzy 検索の対象文字列。
    pub fn haystack(&self) -> &str {
        &self.haystack
    }

    /// タグ表示。
    pub fn format_tags(&self) -> String {
        self.tags.join(",")
    }

    /// タグを差し替える。fuzzy 検索の対象文字列も合わせて作り直す。
    pub fn set_tags(&mut self, tags: Vec<String>) {
        self.tags = tags;
        self.build_haystack();
    }

    fn build_haystack(&mut self) {
        let mut h = String::with_capacity(160);
        h.push_str(&self.title);
        h.push(' ');
        h.push_str(&self.session_id);
        if let Some(cwd) = &self.cwd {
            h.push(' ');
            h.push_str(cwd);
        } else {
            h.push(' ');
            h.push_str(&self.project_dir);
        }
        for t in &self.tickets {
            h.push_str(&format!(" #{t}"));
        }
        for t in &self.tags {
            h.push(' ');
            h.push_str(t);
        }
        self.haystack = h;
    }
}

/// SystemTime を `YYYY-MM-DD HH:MM` に整形する (ローカルタイム)。
pub fn format_time(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => {
            let dt: DateTime<Local> = t.into();
            dt.format("%Y-%m-%d %H:%M").to_string()
        }
        None => String::new(),
    }
}

/// バイト数を人が読める形にする。
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes}{}", UNITS[0])
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

/// 走査結果と周辺データを突き合わせて一覧行を作る。作成日時の降順に並べる。
pub fn build(
    sessions: Vec<ScannedSession>,
    tasks: &HashMap<String, TaskSummary>,
    running: &HashMap<String, RunningSession>,
    worklog: &HashMap<String, WorklogSummary>,
    tags: &HashMap<String, Vec<String>>,
) -> Vec<SessionRow> {
    let mut rows: Vec<SessionRow> = sessions
        .into_iter()
        .map(|s| {
            let task_summary = tasks.get(&s.session_id).cloned().unwrap_or_default();
            let wl = worklog.get(&s.session_id).cloned().unwrap_or_default();

            // タイトル → タスク subject → worklog の順にチケット番号を集める
            let mut tickets = ticket::extract(&s.title);
            for subject in &task_summary.subjects {
                for n in ticket::extract(subject) {
                    if !tickets.contains(&n) {
                        tickets.push(n);
                    }
                }
            }
            for n in &wl.tickets {
                if !tickets.contains(n) {
                    tickets.push(*n);
                }
            }

            // cwd が現存するか (機能6)。記録なし・消滅どちらも false。
            let cwd_exists = s
                .cwd
                .as_deref()
                .map(|c| std::path::Path::new(c).is_dir())
                .unwrap_or(false);

            let mut row = SessionRow {
                session_id: s.session_id.clone(),
                path: s.target.path,
                project_dir: s.target.project_dir,
                cwd: s.cwd,
                cwd_exists,
                title: s.title,
                title_kind: s.title_kind,
                tickets,
                tasks: task_summary,
                // jsonl 内 timestamp を優先した解決済みの値 (機能3)。birthtime は使わない
                created: s.created,
                modified: s.target.modified,
                running: running.get(&s.session_id).cloned(),
                tags: tags.get(&s.session_id).cloned().unwrap_or_default(),
                worklog: wl,
                size: s.target.size,
                line_count: s.line_count,
                first_prompt: s.first_prompt,
                // fork 検出には全セッションの uuid が出揃っている必要があり、ここでは
                // まだ判定できない。呼び出し側が apply_fork_marks で後から反映する
                fork: None,
                haystack: String::new(),
            };
            row.build_haystack();
            row
        })
        .collect();

    sort_by_created_desc(&mut rows);
    rows
}

/// fork グループの判定結果を各行に反映する。
///
/// `build` とは別に呼ぶ設計にしてある。fork 検出 (`crate::fork::detect_fork_groups`) は
/// 全セッションの message uuid が DB に出揃っている必要があり、`build` より後のタイミング
/// (loader 側で fork 検出を実行した後) にしか判定できないため。
pub fn apply_fork_marks(rows: &mut [SessionRow], fork_groups: &[ForkGroup]) {
    let mut marks: HashMap<&str, ForkMark> = HashMap::new();
    for g in fork_groups {
        for m in &g.members {
            let group_members = g.members.iter().filter(|x| *x != m).cloned().collect();
            marks.insert(m.as_str(), ForkMark { is_root: *m == g.root, group_members });
        }
    }
    for row in rows.iter_mut() {
        row.fork = marks.get(row.session_id.as_str()).cloned();
    }
}

/// 作成日時の降順 (不明は最後)。同着は sessionId で安定させる。
pub fn sort_by_created_desc(rows: &mut [SessionRow]) {
    rows.sort_by(|a, b| match (b.created, a.created) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.session_id.cmp(&b.session_id)),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => a.session_id.cmp(&b.session_id),
    });
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use crate::scan::ScanTarget;
    use std::time::Duration;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn scanned(id: &str, title: &str, created: Option<SystemTime>) -> ScannedSession {
        ScannedSession {
            target: ScanTarget {
                path: PathBuf::from(format!("/p/proj/{id}.jsonl")),
                kind: crate::scan::EntryKind::Session,
                project_dir: "proj".into(),
                file_stem: id.into(),
                size: 1024,
                mtime_ns: 0,
                created,
                modified: created,
            },
            session_id: id.into(),
            cwd: Some("/Users/work/.ghq/ss_es_teppai".into()),
            title: title.into(),
            title_kind: TitleKind::Custom,
            first_prompt: Some("最初の質問".into()),
            line_count: 42,
            created,
        }
    }

    /// build() に渡す 4 つの引き当て表 (tasks / running / worklog / tags)。
    type SideData = (
        HashMap<String, TaskSummary>,
        HashMap<String, RunningSession>,
        HashMap<String, WorklogSummary>,
        HashMap<String, Vec<String>>,
    );

    fn empty() -> SideData {
        Default::default()
    }

    #[test]
    fn 作成日時の降順に並ぶ() {
        let (tasks, running, wl, tags) = empty();
        let rows = build(
            vec![
                scanned("old00000-1", "古い", Some(t(1000))),
                scanned("new00000-1", "新しい", Some(t(3000))),
                scanned("mid00000-1", "中間", Some(t(2000))),
            ],
            &tasks,
            &running,
            &wl,
            &tags,
        );
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["新しい", "中間", "古い"]);
    }

    #[test]
    fn 作成日時不明は最後に来る() {
        let (tasks, running, wl, tags) = empty();
        let rows = build(
            vec![
                scanned("aaa00000-1", "日時不明", None),
                scanned("bbb00000-1", "日時あり", Some(t(1000))),
            ],
            &tasks,
            &running,
            &wl,
            &tags,
        );
        assert_eq!(rows[0].title, "日時あり");
        assert_eq!(rows[1].title, "日時不明");
    }

    #[test]
    fn 短縮IDは8桁() {
        let (tasks, running, wl, tags) = empty();
        let rows = build(
            vec![scanned("120d44c7-4cc5-451d-8576-26bf1de1f67f", "x", Some(t(1)))],
            &tasks,
            &running,
            &wl,
            &tags,
        );
        assert_eq!(rows[0].short_id(), "120d44c7");
    }

    #[test]
    fn 短いIDでも落ちない() {
        let (tasks, running, wl, tags) = empty();
        let rows = build(vec![scanned("abc", "x", Some(t(1)))], &tasks, &running, &wl, &tags);
        assert_eq!(rows[0].short_id(), "abc");
    }

    #[test]
    fn チケットはタイトルとタスクとworklogから集まる() {
        let (_, running, _, tags) = empty();
        let mut tasks = HashMap::new();
        tasks.insert(
            "s1".to_string(),
            TaskSummary {
                pending: 1,
                in_progress: 0,
                completed: 2,
                other: 0,
                subjects: vec!["[#55711] タスク側のチケット".into(), "[#3] 連番は無視".into()],
            },
        );
        let mut wl = HashMap::new();
        wl.insert(
            "s1".to_string(),
            WorklogSummary {
                elapsed_sec: 3600,
                blocks: 2,
                tickets: vec![55712],
            },
        );

        let rows = build(
            vec![scanned("s1", "[#55710] タイトル側", Some(t(1)))],
            &tasks,
            &running,
            &wl,
            &tags,
        );
        assert_eq!(rows[0].tickets, vec![55710, 55711, 55712]);
        assert_eq!(rows[0].format_tickets(), "#55710 #55711 #55712");
        assert_eq!(rows[0].format_tasks(), "1/0/2");
        assert_eq!(rows[0].worklog.format_elapsed(), "1h00m");
    }

    #[test]
    fn チケット重複は排除される() {
        let (_, running, _, tags) = empty();
        let mut tasks = HashMap::new();
        tasks.insert(
            "s1".to_string(),
            TaskSummary {
                subjects: vec!["[#55710] 同じチケット".into()],
                ..Default::default()
            },
        );
        let rows = build(
            vec![scanned("s1", "[#55710] タイトル", Some(t(1)))],
            &tasks,
            &running,
            &HashMap::new(),
            &tags,
        );
        assert_eq!(rows[0].tickets, vec![55710]);
    }

    #[test]
    fn 実行中セッションが紐づく() {
        let (tasks, _, wl, tags) = empty();
        let mut running = HashMap::new();
        running.insert(
            "s1".to_string(),
            RunningSession {
                pid: 100,
                session_id: "s1".into(),
                cwd: None,
                name: None,
                status: Some("busy".into()),
                kind: Some("interactive".into()),
            },
        );
        let rows = build(vec![scanned("s1", "x", Some(t(1)))], &tasks, &running, &wl, &tags);
        assert!(rows[0].is_running());
        assert_eq!(rows[0].running.as_ref().unwrap().label(), "interactive/busy");
    }

    #[test]
    fn タグが紐づいてhaystackにも入る() {
        let (tasks, running, wl, _) = empty();
        let mut tags = HashMap::new();
        tags.insert("s1".to_string(), vec!["コスト削減".into(), "WAF".into()]);
        let rows = build(
            vec![scanned("s1", "[#55710] タイトル", Some(t(1)))],
            &tasks,
            &running,
            &wl,
            &tags,
        );
        assert_eq!(rows[0].format_tags(), "コスト削減,WAF");
        assert!(rows[0].haystack().contains("コスト削減"));
        assert!(rows[0].haystack().contains("#55710"));
        assert!(rows[0].haystack().contains("ss_es_teppai"));
        assert!(rows[0].haystack().contains("s1"));
    }

    #[test]
    fn タグ差し替えで検索対象も作り直される() {
        let (tasks, running, wl, tags) = empty();
        let mut rows = build(vec![scanned("s1", "タイトル", Some(t(1)))], &tasks, &running, &wl, &tags);
        assert!(!rows[0].haystack().contains("あとから"));

        rows[0].set_tags(vec!["あとからタグ".into()]);
        assert!(rows[0].haystack().contains("あとからタグ"));
        assert_eq!(rows[0].format_tags(), "あとからタグ");

        // 空にすると検索対象からも消える
        rows[0].set_tags(Vec::new());
        assert!(!rows[0].haystack().contains("あとからタグ"));
    }

    #[test]
    fn プロジェクト表示はcwdの末尾() {
        let (tasks, running, wl, tags) = empty();
        let rows = build(vec![scanned("s1", "x", Some(t(1)))], &tasks, &running, &wl, &tags);
        assert_eq!(rows[0].project_label(), "ss_es_teppai");
    }

    #[test]
    fn cwdが無ければディレクトリ名を出す() {
        let (tasks, running, wl, tags) = empty();
        let mut s = scanned("s1", "x", Some(t(1)));
        s.cwd = None;
        let rows = build(vec![s], &tasks, &running, &wl, &tags);
        assert_eq!(rows[0].project_label(), "proj");
        assert!(rows[0].haystack().contains("proj"));
    }

    #[test]
    fn タスクが無ければ空表示() {
        let (tasks, running, wl, tags) = empty();
        let rows = build(vec![scanned("s1", "x", Some(t(1)))], &tasks, &running, &wl, &tags);
        assert_eq!(rows[0].format_tasks(), "");
        assert_eq!(rows[0].format_tickets(), "");
    }

    #[test]
    fn cwdが現存すればcwd_existsがtrue() {
        let (tasks, running, wl, tags) = empty();
        let tmp = tempfile::tempdir().unwrap();
        let mut s = scanned("s1", "x", Some(t(1)));
        s.cwd = Some(tmp.path().to_string_lossy().to_string());
        let rows = build(vec![s], &tasks, &running, &wl, &tags);
        assert!(rows[0].cwd_exists);
        assert!(!rows[0].cwd_missing());
    }

    #[test]
    fn cwdが消えていればcwd_missing() {
        let (tasks, running, wl, tags) = empty();
        let mut s = scanned("s1", "x", Some(t(1)));
        s.cwd = Some("/存在しないディレクトリ/abc".into());
        let rows = build(vec![s], &tasks, &running, &wl, &tags);
        assert!(!rows[0].cwd_exists);
        assert!(rows[0].cwd_missing());
    }

    #[test]
    fn cwdが無ければcwd_missing() {
        let (tasks, running, wl, tags) = empty();
        let mut s = scanned("s1", "x", Some(t(1)));
        s.cwd = None;
        let rows = build(vec![s], &tasks, &running, &wl, &tags);
        assert!(rows[0].cwd_missing());
    }

    #[test]
    fn 日時整形() {
        assert_eq!(format_time(None), "");
        let s = format_time(Some(t(1_700_000_000)));
        assert_eq!(s.len(), 16);
        assert!(s.starts_with("20"));
    }

    #[test]
    fn サイズ整形() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(2048), "2.0K");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0M");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0G");
    }

    #[test]
    fn buildの直後はforkが未設定() {
        let (tasks, running, wl, tags) = empty();
        let rows = build(vec![scanned("s1", "x", Some(t(1)))], &tasks, &running, &wl, &tags);
        assert!(rows[0].fork.is_none());
    }

    #[test]
    fn apply_fork_marksでrootと非rootが反映される() {
        let (tasks, running, wl, tags) = empty();
        let mut rows = build(
            vec![
                scanned("root1234", "本線", Some(t(100))),
                scanned("child123", "分岐", Some(t(200))),
                scanned("other000", "無関係", Some(t(300))),
            ],
            &tasks,
            &running,
            &wl,
            &tags,
        );

        let groups = vec![ForkGroup {
            root: "root1234".to_string(),
            members: vec!["root1234".to_string(), "child123".to_string()],
        }];
        apply_fork_marks(&mut rows, &groups);

        let root_row = rows.iter().find(|r| r.session_id == "root1234").unwrap();
        let root_mark = root_row.fork.as_ref().unwrap();
        assert!(root_mark.is_root);
        assert_eq!(root_mark.group_members, vec!["child123".to_string()]);

        let child_row = rows.iter().find(|r| r.session_id == "child123").unwrap();
        let child_mark = child_row.fork.as_ref().unwrap();
        assert!(!child_mark.is_root);
        assert_eq!(child_mark.group_members, vec!["root1234".to_string()]);

        let other_row = rows.iter().find(|r| r.session_id == "other000").unwrap();
        assert!(other_row.fork.is_none(), "グループ外は fork が None のまま");
    }

    #[test]
    fn apply_fork_marksは空グループなら何もしない() {
        let (tasks, running, wl, tags) = empty();
        let mut rows = build(vec![scanned("s1", "x", Some(t(1)))], &tasks, &running, &wl, &tags);
        apply_fork_marks(&mut rows, &[]);
        assert!(rows[0].fork.is_none());
    }
}
