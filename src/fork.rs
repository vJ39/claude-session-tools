//! fork (resume による分岐) 検出。
//!
//! `claude --resume <id>` で過去のセッションを resume すると、そこまでの会話履歴を
//! 丸ごと引き継いだ別の sessionId が生成される。同じ会話の分岐は各メッセージの
//! `uuid` を共有するため、複数セッションにまたがって出現する uuid を手がかりに
//! 「同じ会話から分岐したセッション群 (fork グループ)」を検出する。
//!
//! 厳密な親子ツリー (誰が誰から直接分岐したか) の推定は、`/compact` によるコンテキスト
//! 圧縮や多方向 fork が絡むと精度が出ないため行わない (docs/fork-detection-design.md 参照)。
//! ここではフラットなグループ化と、グループ内の「起源」判定だけを行う。

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use crate::store::SharedUuidRow;

/// 同じ会話から分岐したセッション群。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkGroup {
    /// 起源 (グループ内で物理ファイルの birthtime が最も古いもの) の session_id
    pub root: String,
    /// root を含む全メンバーの session_id
    pub members: Vec<String>,
}

/// 単純な Union-Find (経路圧縮のみ、size union はしない。データ量が小さいので十分)。
struct UnionFind {
    parent: HashMap<String, String>,
}

impl UnionFind {
    fn new() -> Self {
        Self { parent: HashMap::new() }
    }

    /// 代表元を求める。未登録のノードは自分自身が代表元。
    fn find(&mut self, x: &str) -> String {
        let parent = self.parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if parent == x {
            x.to_string()
        } else {
            let root = self.find(&parent);
            self.parent.insert(x.to_string(), root.clone());
            root
        }
    }

    fn union(&mut self, a: &str, b: &str) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent.insert(ra, rb);
        }
    }
}

/// fork グループを検出する。
///
/// - `shared`: 複数セッションにまたがって出現する uuid の行 ([`crate::store::Store::shared_message_uuids`])
/// - `birthtimes`: session_id ごとの物理ファイル birthtime (起源判定に使う)。
///   見つからない/取得できない session_id は最も新しい扱いにする (起源になりにくくする)
pub fn detect_fork_groups(
    shared: &[SharedUuidRow],
    birthtimes: &[(String, Option<SystemTime>)],
) -> Vec<ForkGroup> {
    let mut by_uuid: HashMap<&str, Vec<&str>> = HashMap::new();
    for row in shared {
        by_uuid.entry(row.uuid.as_str()).or_default().push(row.session_id.as_str());
    }

    let mut uf = UnionFind::new();
    let mut all_ids: HashSet<&str> = HashSet::new();
    for sids in by_uuid.values() {
        if sids.len() < 2 {
            continue;
        }
        for s in sids {
            all_ids.insert(s);
        }
        for pair in sids.windows(2) {
            uf.union(pair[0], pair[1]);
        }
    }

    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for id in &all_ids {
        let root_key = uf.find(id);
        groups.entry(root_key).or_default().push((*id).to_string());
    }

    let bt: HashMap<&str, Option<SystemTime>> =
        birthtimes.iter().map(|(s, t)| (s.as_str(), *t)).collect();

    let mut out: Vec<ForkGroup> = groups
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|mut members| {
            members.sort();
            let root = members
                .iter()
                .min_by(|a, b| compare_birthtime(&bt, a, b))
                .cloned()
                .expect("グループは空でないことを filter で保証済み");
            ForkGroup { root, members }
        })
        .collect();

    // 呼び出し側での表示順を安定させる (root の session_id 順)
    out.sort_by(|a, b| a.root.cmp(&b.root));
    out
}

/// 起源判定の比較。birthtime が古い方を優先し、無いものは最新扱いにして後回しにする。
/// 両方無ければ session_id の辞書順でタイブレークする (発生頻度は低い想定)。
fn compare_birthtime(bt: &HashMap<&str, Option<SystemTime>>, a: &str, b: &str) -> Ordering {
    let ta = bt.get(a).copied().flatten();
    let tb = bt.get(b).copied().flatten();
    match (ta, tb) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.cmp(b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use std::time::Duration;

    fn row(uuid: &str, session_id: &str) -> SharedUuidRow {
        SharedUuidRow { uuid: uuid.to_string(), session_id: session_id.to_string(), ts_ms: None }
    }

    fn t(secs: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn 単純な2方向forkでbirthtimeが古い方が起源になる() {
        let shared = vec![row("u1", "a"), row("u1", "b")];
        let birthtimes = vec![("a".to_string(), t(200)), ("b".to_string(), t(100))];
        let groups = detect_fork_groups(&shared, &birthtimes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].root, "b");
        let mut members = groups[0].members.clone();
        members.sort();
        assert_eq!(members, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn 三方向forkでも1グループにまとまる() {
        // a-b, b-c がそれぞれ別のuuidで共有されていても推移的に同じグループになる
        let shared = vec![row("u1", "a"), row("u1", "b"), row("u2", "b"), row("u2", "c")];
        let birthtimes = vec![
            ("a".to_string(), t(300)),
            ("b".to_string(), t(100)),
            ("c".to_string(), t(200)),
        ];
        let groups = detect_fork_groups(&shared, &birthtimes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].root, "b");
        let mut members = groups[0].members.clone();
        members.sort();
        assert_eq!(members, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn 独立した2組のforkは別グループになる() {
        let shared = vec![row("u1", "a"), row("u1", "b"), row("u2", "c"), row("u2", "d")];
        let birthtimes = vec![
            ("a".to_string(), t(100)),
            ("b".to_string(), t(200)),
            ("c".to_string(), t(50)),
            ("d".to_string(), t(60)),
        ];
        let groups = detect_fork_groups(&shared, &birthtimes);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].root, "a");
        assert_eq!(groups[1].root, "c");
    }

    #[test]
    fn 共有uuidが無ければ空() {
        let shared: Vec<SharedUuidRow> = Vec::new();
        let groups = detect_fork_groups(&shared, &[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn 単独セッションにしか出ないuuidは無視する() {
        // shared には元々「2セッション以上に出る uuid」だけが来る前提だが、
        // 呼び出し側の実装ミスで単独行が混ざっても壊れないことを確認する
        let shared = vec![row("u1", "a")];
        let groups = detect_fork_groups(&shared, &[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn birthtimeが取れないセッションは起源になりにくい() {
        let shared = vec![row("u1", "a"), row("u1", "b")];
        // a は birthtime 不明、b はある → b を優先して起源にする
        let birthtimes = vec![("b".to_string(), t(999))];
        let groups = detect_fork_groups(&shared, &birthtimes);
        assert_eq!(groups[0].root, "b");
    }

    #[test]
    fn birthtimeが両方無ければsession_idの辞書順でタイブレークする() {
        let shared = vec![row("u1", "zzz"), row("u1", "aaa")];
        let groups = detect_fork_groups(&shared, &[]);
        assert_eq!(groups[0].root, "aaa");
    }
}
