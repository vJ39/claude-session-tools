//! fzf 風のインクリメンタル絞り込み。
//!
//! マッチャは nucleo (helix が使っているもの) を使う。
//! 内容検索 (jsonl の全文 grep) の結果と AND で重ねられる。

use std::collections::HashSet;

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32String};

use crate::rows::SessionRow;

/// 絞り込み器。マッチャは使い回す (毎回作ると遅い)。
pub struct Filter {
    matcher: Matcher,
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// query と内容検索結果で絞り込み、行インデックスを返す。
    ///
    /// - `query` が空なら並び順 (作成日時降順) を保つ
    /// - `query` があればスコア降順、同点は元の並び順
    /// - `content_hits` が `Some` なら、その sessionId に含まれるものだけを対象にする
    pub fn apply(
        &mut self,
        rows: &[SessionRow],
        query: &str,
        content_hits: Option<&HashSet<String>>,
    ) -> Vec<usize> {
        let base: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| match content_hits {
                Some(hits) => hits.contains(&r.session_id),
                None => true,
            })
            .map(|(i, _)| i)
            .collect();

        let q = query.trim();
        if q.is_empty() {
            return base;
        }

        let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
        let mut scored: Vec<(u32, usize)> = Vec::with_capacity(base.len());

        for i in base {
            let haystack = Utf32String::from(rows[i].haystack());
            if let Some(score) = pattern.score(haystack.slice(..), &mut self.matcher) {
                scored.push((score, i));
            }
        }

        // スコア降順、同点は元の並び (= 作成日時降順) を維持
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }
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
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn scanned(id: &str, title: &str, cwd: &str, secs: u64) -> ScannedSession {
        let created = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        ScannedSession {
            target: ScanTarget {
                path: PathBuf::from(format!("/p/proj/{id}.jsonl")),
                kind: crate::scan::EntryKind::Session,
                project_dir: "proj".into(),
                file_stem: id.into(),
                size: 10,
                mtime_ns: 0,
                created,
                modified: None,
            },
            session_id: id.into(),
            cwd: Some(cwd.into()),
            title: title.into(),
            title_kind: TitleKind::Custom,
            first_prompt: None,
            line_count: 1,
            created,
        }
    }

    fn rows() -> Vec<SessionRow> {
        let tasks: HashMap<String, TaskSummary> = HashMap::new();
        let running: HashMap<String, RunningSession> = HashMap::new();
        let wl: HashMap<String, WorklogSummary> = HashMap::new();
        let tags: HashMap<String, Vec<String>> = HashMap::new();
        rows::build(
            vec![
                scanned("aaaa1111", "[#55710] WAFボット対策", "/Users/work/.ghq/chat/slack", 300),
                scanned("bbbb2222", "コスト削減PJ CrateDB移行", "/Users/work/.ghq/ss_es_teppai", 200),
                scanned("cccc3333", "termmap 開発", "/Users/work/.ghq/github.com/vJ39/termmap", 100),
            ],
            &tasks,
            &running,
            &wl,
            &tags,
        )
    }

    #[test]
    fn クエリが空なら全件を並び順のまま返す() {
        let rows = rows();
        let mut f = Filter::new();
        let got = f.apply(&rows, "", None);
        assert_eq!(got, vec![0, 1, 2]);
        // 作成日時降順のまま
        assert_eq!(rows[got[0]].session_id, "aaaa1111");
    }

    #[test]
    fn 空白だけのクエリも全件() {
        let rows = rows();
        let mut f = Filter::new();
        assert_eq!(f.apply(&rows, "   ", None).len(), 3);
    }

    #[test]
    fn タイトルで絞り込める() {
        let rows = rows();
        let mut f = Filter::new();
        let got = f.apply(&rows, "コスト", None);
        assert_eq!(got.len(), 1);
        assert_eq!(rows[got[0]].session_id, "bbbb2222");
    }

    #[test]
    fn チケット番号で絞り込める() {
        let rows = rows();
        let mut f = Filter::new();
        let got = f.apply(&rows, "55710", None);
        assert_eq!(rows[got[0]].session_id, "aaaa1111");
    }

    #[test]
    fn セッションIDで絞り込める() {
        let rows = rows();
        let mut f = Filter::new();
        let got = f.apply(&rows, "cccc3333", None);
        assert_eq!(got.len(), 1);
        assert_eq!(rows[got[0]].session_id, "cccc3333");
    }

    #[test]
    fn cwdで絞り込める() {
        let rows = rows();
        let mut f = Filter::new();
        let got = f.apply(&rows, "teppai", None);
        assert_eq!(got.len(), 1);
        assert_eq!(rows[got[0]].session_id, "bbbb2222");
    }

    #[test]
    fn 飛び飛びの部分一致でも拾う() {
        let rows = rows();
        let mut f = Filter::new();
        // "termmap" を "tmap" で引く
        let got = f.apply(&rows, "tmap", None);
        assert!(got.iter().any(|&i| rows[i].session_id == "cccc3333"));
    }

    #[test]
    fn 一致しなければ空() {
        let rows = rows();
        let mut f = Filter::new();
        assert!(f.apply(&rows, "存在しないキーワードzzz", None).is_empty());
    }

    #[test]
    fn 内容検索結果とANDで重なる() {
        let rows = rows();
        let mut f = Filter::new();
        let hits: HashSet<String> = ["bbbb2222".to_string(), "cccc3333".to_string()]
            .into_iter()
            .collect();

        let got = f.apply(&rows, "", Some(&hits));
        assert_eq!(got.len(), 2);
        assert!(!got.iter().any(|&i| rows[i].session_id == "aaaa1111"));

        // 内容検索で絞った上に fuzzy を重ねる
        let got = f.apply(&rows, "termmap", Some(&hits));
        assert_eq!(got.len(), 1);
        assert_eq!(rows[got[0]].session_id, "cccc3333");

        // 内容検索の対象外はクエリが当たっても出ない
        let got = f.apply(&rows, "WAF", Some(&hits));
        assert!(got.is_empty());
    }

    #[test]
    fn 内容検索が空集合なら何も出ない() {
        let rows = rows();
        let mut f = Filter::new();
        let hits: HashSet<String> = HashSet::new();
        assert!(f.apply(&rows, "", Some(&hits)).is_empty());
    }

    #[test]
    fn 行が空でも落ちない() {
        let mut f = Filter::new();
        assert!(f.apply(&[], "なにか", None).is_empty());
    }
}
