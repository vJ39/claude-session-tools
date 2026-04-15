#!/bin/bash
# Claude会話データをS3にバックアップ
# ネットワーク断でもエラーにならないようにチェック付き

# S3到達確認（タイムアウト5秒）
if ! AWS_PROFILE=test aws s3api head-bucket --bucket yotsuya-test --timeout 5 2>/dev/null; then
  exit 0
fi

AWS_PROFILE=test aws s3 sync ~/.claude/projects/ s3://yotsuya-test/claude-sessions/projects/ \
  --exclude "*.lock" \
  --size-only \
  --only-show-errors
