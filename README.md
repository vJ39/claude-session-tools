# claude-session-tools

Claude Code のセッションデータを一覧・検索・操作する CLI (`cst`, Rust製)。

## ビルド

```bash
cargo build --release
# バイナリ: target/release/cst
```

## TUI セッションブラウザ

サブコマンド無しで実行すると起動する。

```bash
cst
cst --include-subagents   # サブエージェント記録も起動時から表示
```

- 一覧: session id / タイトル(無ければ最初のuserメッセージ冒頭 / それも無ければ`(無題)`) / Redmineチケット番号 / タスク数(pending・in_progress・done) / 作成日時、作成日時降順
- 既定ではサブエージェント記録(`<sessionId>/subagents/agent-*.jsonl`)を一覧から除外する

操作キー:

```
Enter : resume起動 (cwdへcdしてclaude --resume)
^g    : 内容検索(全文grep)
^t    : タグ付け
^r    : 要約
^s    : サブエージェント表示切替
^d    : 削除
^a    : アーカイブ
^u    : クリア
Esc   : 戻る/終了
```

## merge サブコマンド

セッションBの会話をセッションAに取り込む。resumeして`/compact`すれば複数セッションの知識を1つに統合できる。

```bash
# 先頭に挿入（デフォルト）
cst merge <session_a.jsonl> <session_b.jsonl>

# 末尾に追加
cst merge <session_a.jsonl> <session_b.jsonl> --append

# dry-run（確認のみ）
cst merge <session_a.jsonl> <session_b.jsonl> --dry-run
cst merge <session_a.jsonl> <session_b.jsonl> --append --dry-run
```

マージ後の流れ:
1. `claude --resume <session_a_id>` でセッションAを再開
2. `/compact` で圧縮
3. セッションBは不要なら削除

バックアップは実行時に自動で `.bak.<timestamp>` として作成される。

## sync-s3 サブコマンド

Claude の会話データを S3 にバックアップする。cronで定期実行する想定。ネットワーク不通時はスキップする。

```bash
# 手動実行
cst sync-s3

# 転送対象を確認するだけ
cst sync-s3 --dry-run

# cron登録（毎時0分）
0 * * * * /path/to/cst sync-s3
```

既定値:
- AWSプロファイル: `test` (明示指定 `--profile` > 環境変数 `AWS_PROFILE` > 既定値の順で解決)
- バケット: `yotsuya-test`
- プレフィックス: `claude-sessions/projects/`
- 同期元: `~/.claude/projects/`
- 除外パターン: `*.lock`
- `--size-only` 相当の判定のみで転送、削除同期はしない（ローカル削除してもS3には残る）
