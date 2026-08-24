# workspace-mcp-rs

Rust 製 Google Workspace MCP サーバー。stdio と Streamable HTTP の両トランスポート対応、121 ツール / 12 サービス。リポジトリには上流の Python 実装も同居しているが、**ビルド対象は `src/` の Rust 実装のみ**（`.dockerignore` 参照）。

## 構成

- `src/` — Rust 実装（MCP プロトコルは `src/mcp.rs` に自前実装、SDK 不使用）
  - `src/transport.rs` — stdio / Streamable HTTP、セッション管理
  - `src/oauth.rs` — 認証 4 モード（Legacy / InternalOAuth21 / ExternalGoogle / TrustedGateway）
  - `src/auth.rs` — 認証情報ストア、トークンリフレッシュ、service account/DWD
  - `src/native/` — Google 各サービスのツール実装
- `infra/` — AWS CDK (TypeScript)。ECS/Fargate へのリモート MCP サーバーデプロイ
- `docs/DESIGN_DECISIONS.md` — ホスティング・認証方式の判断根拠と**運用上の不変条件**

## ビルド・検証

```sh
cargo build --release --locked   # Rust 1.98 固定 (rust-toolchain.toml)
cd infra && pnpm install && pnpm run build && npx cdk synth -c domainName=<domain> -c certificateArn=<arn>
```

## AI エージェント向けルール

- **インフラ（`infra/`）や認証まわりを変更する前に、必ず `docs/DESIGN_DECISIONS.md` の「不変条件」セクションを読むこと。** 特に: Fargate タスクは単一固定（OAuth 状態がプロセス内メモリのため）、EFS 追加禁止、`WORKSPACE_MCP_REGISTERED_CLIENT_TTL_SECONDS=2592000` 維持。
- `cdk deploy` / `cdk destroy` などリモートインフラを変更するコマンドはユーザーの明示的な承認なしに実行しない（`synth` / `diff` は常に可）。
- シークレット（Google OAuth クライアント ID/シークレット、署名鍵）はコミットしない。Secrets Manager `gws-mcp/oauth` で管理。
- 依存クレートは `=` で完全固定されている。バージョン変更は明示的な依頼があるときのみ。
