//! Redmine チケット番号の抽出。
//!
//! spec.md「Redmineチケット番号」節のとおり、専用フィールドは無く常に文字列埋め込み。
//! `[#NNNN]` / `#NNNN ` 形式で入る運用で、10000 以上を Redmine 番号、
//! それ未満を TaskCreate の連番とみなす (worklog 側と同じ推定ルール)。

/// Redmine 番号とみなす下限。これ未満は TaskCreate の連番 ID とみなして捨てる。
pub const REDMINE_MIN: u64 = 10_000;

/// 文字列中の `#数字` を全て拾い、Redmine 番号とみなせるものだけを
/// 出現順・重複排除して返す。
pub fn extract(text: &str) -> Vec<u64> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        // '#' の直後から続く ASCII 数字を読む
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == start {
            i += 1;
            continue;
        }
        // 桁が異常に多いものは u64 パースが失敗するので単に捨てる
        if let Ok(n) = text[start..end].parse::<u64>()
            && n >= REDMINE_MIN
            && !found.contains(&n)
        {
            found.push(n);
        }
        i = end;
    }

    found
}

/// 複数の文字列から抽出してマージする (出現順・重複排除)。
pub fn extract_all<'a, I>(texts: I) -> Vec<u64>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut found: Vec<u64> = Vec::new();
    for t in texts {
        for n in extract(t) {
            if !found.contains(&n) {
                found.push(n);
            }
        }
    }
    found
}

/// worklog.db の `ticket` 列を Redmine 番号として解釈する。
/// 連番 ID (10000 未満) や数字でない値は `None`。
pub fn parse_worklog_ticket(raw: &str) -> Option<u64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let t = t.strip_prefix('#').unwrap_or(t);
    match t.parse::<u64>() {
        Ok(n) if n >= REDMINE_MIN => Some(n),
        _ => None,
    }
}

/// 表示用に `#55710 #55711` 形式へ整形する。
pub fn format(tickets: &[u64]) -> String {
    tickets
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;

    #[test]
    fn 角括弧形式を抽出する() {
        assert_eq!(extract("[#55710] history/aucview アクセス増加"), vec![55710]);
    }

    #[test]
    fn シャープ空白形式を抽出する() {
        assert_eq!(extract("#55710 ボット対策"), vec![55710]);
    }

    #[test]
    fn 複数チケットを出現順に抽出する() {
        assert_eq!(
            extract("[#55438/#55439/#55440] solr 運用リスク3件"),
            vec![55438, 55439, 55440]
        );
    }

    #[test]
    fn 重複は排除する() {
        assert_eq!(extract("#55710 の続き (#55710)"), vec![55710]);
    }

    #[test]
    fn 連番IDは除外する() {
        // TaskCreate の連番 (10000 未満) は Redmine 番号ではない
        assert!(extract("[#1] MVPスコープの確定").is_empty());
        assert!(extract("[#9999] まだ連番").is_empty());
        assert_eq!(extract("[#10000] 境界値"), vec![10000]);
    }

    #[test]
    fn チケットが無ければ空() {
        assert!(extract("ただのタイトル").is_empty());
        assert!(extract("").is_empty());
    }

    #[test]
    fn 数字でないシャープは無視する() {
        assert!(extract("color: #ffffff / # / ##").is_empty());
    }

    #[test]
    fn 桁溢れは無視する() {
        // u64 に収まらない桁数はパース失敗として捨てる (panic しない)
        assert!(extract("#99999999999999999999999999").is_empty());
    }

    #[test]
    fn 日本語に隣接していても抽出する() {
        assert_eq!(extract("チケット#55765の対応"), vec![55765]);
    }

    #[test]
    fn 複数文字列からマージできる() {
        let texts = vec!["[#55710] タイトル", "[#1] 連番", "#55711 別チケット", "#55710 重複"];
        assert_eq!(extract_all(texts), vec![55710, 55711]);
    }

    #[test]
    fn worklogのticket列を解釈する() {
        assert_eq!(parse_worklog_ticket("55710"), Some(55710));
        assert_eq!(parse_worklog_ticket("#55710"), Some(55710));
        assert_eq!(parse_worklog_ticket(" 55710 "), Some(55710));
        assert_eq!(parse_worklog_ticket("23"), None); // 連番
        assert_eq!(parse_worklog_ticket(""), None);
        assert_eq!(parse_worklog_ticket("abc"), None);
    }

    #[test]
    fn 表示整形() {
        assert_eq!(format(&[55710, 55711]), "#55710 #55711");
        assert_eq!(format(&[]), "");
    }
}
