# claude-session-tools

Claude Code のセッションデータを管理するツール集。

## ツール一覧

### merge-session.py

セッションBの会話をセッションAに取り込むマージツール。
resumeしてcompactすれば、複数セッションの知識を1つに統合できる。

```bash
# 先頭に挿入（デフォルト）
python3 merge-session.py <session_a.jsonl> <session_b.jsonl>

# 末尾に追加
python3 merge-session.py <session_a.jsonl> <session_b.jsonl> --append

# dry-run（確認のみ）
python3 merge-session.py <session_a.jsonl> <session_b.jsonl> --dry-run
python3 merge-session.py <session_a.jsonl> <session_b.jsonl> --append --dry-run
```

マージ後の流れ:
1. `claude --resume <session_a_id>` でセッションAを再開
2. `/compact` で圧縮
3. セッションBは不要なら削除

バックアップは実行時に自動で `.bak.<timestamp>` として作成される。

### sync-to-s3.sh

Claude の会話データを S3 にバックアップするスクリプト。
cronで定期実行する想定。ネットワーク不通時はスキップする。

```bash
# 手動実行
./sync-to-s3.sh

# cron登録（毎時0分）
0 * * * * /path/to/sync-to-s3.sh
```

設定:
- `AWS_PROFILE=test`
- バケット: `yotsuya-test`
- プレフィックス: `claude-sessions/projects/`
- `--delete` なし（ローカル削除してもS3には残る）
