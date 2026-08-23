# 設計判断記録: workspace-mcp-rs のリモート MCP サーバー化

- 作成日: 2026-08-24
- ステータス: 承認済み（インフラコード実装済み・未デプロイ）
- コード行参照は移行元リポジトリ commit `c8ec8e3` 時点のもの。移行後にファイルを動かした場合は読み替えること。

このドキュメントは、Rust 製 Google Workspace MCP サーバー（`workspace-mcp-rs`）をリモート MCP サーバーとして公開するにあたっての検討経緯・決定事項・運用上の不変条件をまとめたもの。**AI エージェントが本リポジトリで作業する際は、まず「不変条件」セクションを読むこと。**

---

## 1. 目的

社内チーム（soracom.jp の Google Workspace ドメイン）が、Claude Code / claude.ai カスタムコネクタから Streamable HTTP でアクセスできるリモート MCP サーバーとして本サーバーをホストする。

## 2. 決定サマリ

| 項目 | 決定 |
|---|---|
| ホスティング | **AWS ECS/Fargate**（Graviton/arm64、単一タスク）。Cloudflare は却下 |
| IaC | AWS CDK (TypeScript)、`infra/` ディレクトリ |
| イメージ | リポジトリルートの既存 `Dockerfile` を無改修で使用（`src/` の変更ゼロ） |
| 認証 | `MCP_ENABLE_OAUTH21=true`（InternalOAuth21: サーバー自身が OAuth 2.1 AS） |
| 状態 | `WORKSPACE_MCP_STATELESS_MODE=true`（添付ファイル機能 `/attachments` は不要と判断済み） |
| ユーザー制限 | Google OAuth 同意画面を **Internal** に設定（サーバー側に制限機能なし） |
| シークレット | Secrets Manager `gws-mcp/oauth`（clientId / clientSecret / jwtSigningKey） |
| コスト目安 | 約 $40–45/月（ap-northeast-1、NAT なし構成） |

## 3. 不変条件（AI エージェント向け・変更禁止事項）

以下はコード調査で裏取りした制約であり、**該当コードが変わらない限り破ってはならない**:

1. **Fargate タスクは必ず 1 つ**（`desiredCount: 1`, `minHealthyPercent: 0`, `maxHealthyPercent: 100`）。
   理由: InternalOAuth21 モードの OAuth 状態（DCR クライアント・認可コード・アクセストークン・リフレッシュトークン・Google リフレッシュトークン）は `src/oauth.rs` の `RuntimeStore`（行 279–297 の `Mutex<HashMap>`）に**プロセス内メモリのみで保持**される。デプロイ中に 2 タスクが並走すると OAuth ハンドシェイクが分断されて失敗する。**オートスケーリングを追加しないこと。desiredCount を上げないこと。**
2. **EFS を追加しないこと。** InternalOAuth21 モードはディスクに認証情報を書かない（ディスク書き込みは Legacy モードの callback 分岐のみ、`src/oauth.rs:1227–1250`）。EFS を付けても再起動時の再認証は防げない。
3. **`WORKSPACE_MCP_REGISTERED_CLIENT_TTL_SECONDS=2592000`（30 日）を維持すること。** デフォルトは 86400 秒（24 時間）で、そのままだとユーザーが毎日再認証させられる。
4. **`WORKSPACE_EXTERNAL_URL` は HTTPS 必須。** サーバーは `X-Forwarded-*` ヘッダを意図的に信用しない設計のため、ALB 背後では必ずこの env で公開 URL を明示する。
5. **ドメイン制限をサーバー側に実装しようとしないこと（現状は不可能）。** OAuth 2.1 系モードにメール/ドメイン allowlist は存在しない（存在するのは service account DWD 用の `DWD_ALLOWED_DOMAINS` のみ）。soracom.jp 限定は **Google の同意画面 Internal 設定**で担保する。
6. デプロイ・再起動のたびに発行済みトークンは全て無効化され、各ユーザーは次回利用時に 1 回ブラウザ再認証する。これは既知の許容済みトレードオフであり、バグではない。

## 4. 検討経緯

### 4.1 Cloudflare vs AWS（2026-08-23 検討）

当初は Cloudflare 上でのホストも検討したが、コード調査の結果 AWS を選択した。

**Cloudflare Workers — 却下（技術的に不可）**
本実装は以下に全面依存しており、wasm32/workerd ターゲットではビルド・実行できない:
- `tokio`（`rt-multi-thread`, `net`, `signal`）+ `TcpListener` + `axum::serve`
- `std::fs` によるローカルファイル操作、`libc::geteuid` / `O_NOFOLLOW`、`fs2` の flock
- `spawn_blocking` 内での同期 DNS 解決（`safe_fetch.rs` の SSRF ガード）

Workers 化には `transport.rs`・`auth.rs` のストレージ層・`safe_fetch.rs` の約 2,200 行の書き換え + 状態管理の KV/Durable Objects 化が必要で、実質的な別実装の保守になるため却下。

**Cloudflare Containers — 却下（消去法）**
既存イメージはほぼ無改修で動くが、サービスとして新しく、永続化・スケール制御・運用の選択肢が AWS Fargate に劣る。積極的な採用理由がなかった。

**AWS Lambda — 却下**
`lambda_http` アダプタの追加実装が必要で、「アプリコード無改修」の方針に反する。stateless + 外部認証前提なら技術的には可能。

**AWS ECS/Fargate — 採用**
既存の `Dockerfile`（arm64 対応・non-root・`/health` ヘルスチェック付き）がそのまま使える。TLS は rustls / rust_crypto（OpenSSL 非依存）のため arm64 ビルドも問題なし。アプリコードの変更ゼロ。

### 4.2 認証モードの選定

コードには 4 つの認証モードがある（`src/oauth.rs` の `OAuthRuntime::from_env`, 行 348–511）:

| モード | トリガー | 評価 |
|---|---|---|
| `Legacy` | デフォルト（`USER_GOOGLE_EMAIL` 固定） | 単一ユーザー・stdio 向け。リモート複数ユーザーには不適 |
| `InternalOAuth21` | `MCP_ENABLE_OAUTH21=true` | **採用**。RFC 8414 メタデータ + DCR + PKCE を実装済みで、Claude Code / claude.ai カスタムコネクタが標準対応する唯一のモード。制約は単一インスタンス（上記不変条件 1） |
| `ExternalGoogle` | + `EXTERNAL_OAUTH21_PROVIDER=true` | 却下。クライアントが Google 発行のトークンを持参する必要があるが、Google の AS は DCR 非対応で Claude クライアントは自力でトークンを調達できない。スクリプト等の機械的クライアント向けの将来オプション |
| `TrustedGateway` | `TRUST_GATEWAY_IDENTITY=true` | 却下。ALB OIDC と組む案は 2 点で不成立: (1) ALB の `x-amzn-oidc-data` の鍵配布エンドポイントは JWKS 形式ではなく bare PEM を返すため `GATEWAY_IDENTITY_JWKS_URL` の検証実装（`fetch_jwks`, 行 1482–1528）と噛み合わない。(2) そもそも ALB OIDC はブラウザリダイレクト + セッション Cookie 前提で、MCP クライアントの素の HTTP POST では認証フローを辿れない |

stateless モードとの組み合わせ: `stateless && !oauth21` のみ拒否（行 371–375）。InternalOAuth21 + stateless は有効で、MCP セッションマップと `/attachments` が無効化されるだけ。添付ファイル機能は不要と判断済みのため有効化した。

### 4.3 AWS 構成の判断

```
Internet ──443──> ALB (public, ACM, HTTP→HTTPS リダイレクト, idle timeout 3600s)
                    │ target group :8000, health check /health
                    ▼
            Fargate タスク x1 (arm64, 0.25 vCPU / 512 MB)
            public subnet + public IP (NAT なし), SG は ALB からの 8000 のみ許可
```

- **NAT なし・public subnet 構成**: NAT Gateway（~$45/月）を回避するため、タスクに public IP を付与。トレードオフとして ENI に public IP が付くが、SG は ALB SG からの 8000 番のみ許可しており、Google API への egress はどのみち必要。
- **ALB idle timeout 3600s**: `/mcp` の GET は SSE ストリーム（keepalive）のため、デフォルト 60s では切断される。
- **イメージビルド**: CDK の `DockerImageAsset`（`Platform.LINUX_ARM64`）でデプロイ時にビルド・プッシュ。`.dockerignore` は allowlist 方式（`Cargo.toml`/`Cargo.lock`/`src`/`contracts` のみ）のため、`infra/` はビルドコンテキストにもアセットハッシュにも影響しない。
- **IAM**: タスクロールに AWS 権限は一切不要（サーバーは Google API のみ呼ぶ）。実行ロールに Secrets Manager 読み取りが自動付与されるのみ。

## 5. 環境変数（タスク定義）

変数名は `src/oauth.rs` の `from_env` で実際に読まれる名前を検証済み:

| 変数 | 値 | 備考 |
|---|---|---|
| `MCP_ENABLE_OAUTH21` | `true` | InternalOAuth21 選択 |
| `WORKSPACE_MCP_STATELESS_MODE` | `true` | セッションマップと /attachments 無効化 |
| `WORKSPACE_EXTERNAL_URL` | `https://<domain>` | HTTPS 必須（不変条件 4） |
| `WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS` | `https://claude.ai/api/mcp/auth_callback,https://claude.com/api/mcp/auth_callback,http://localhost:*/callback,http://127.0.0.1:*/callback` | **必須**（未設定だと起動エラー）。claude.ai のコールバック URL が想定と違ったらタスクログの allowlist 拒否ログを見て追記 |
| `WORKSPACE_MCP_REGISTERED_CLIENT_TTL_SECONDS` | `2592000` | 不変条件 3 |
| `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET` | Secrets Manager 経由 | 未設定だと起動エラー |
| `FASTMCP_SERVER_AUTH_GOOGLE_JWT_SIGNING_KEY` | Secrets Manager 経由 | ≥32 bytes（`openssl rand -hex 32`）。未設定だと client secret にフォールバックするため必ず設定 |

`WORKSPACE_MCP_HOST` / `WORKSPACE_MCP_PORT` / `WORKSPACE_MCP_CREDENTIALS_DIR` は Dockerfile で設定済み（credentials dir はこのモードでは未使用）。`GOOGLE_OAUTH_REDIRECT_URI` は `WORKSPACE_EXTERNAL_URL` + `/oauth2callback` に自動導出されるため設定不要。

## 6. Google Cloud 側の設定

1. 利用ツールに対応する API を有効化（Gmail, Calendar, Drive, Docs, Sheets, Slides, Forms, Tasks, People, Chat, Apps Script, Custom Search のうち有効化するティア分）
2. OAuth 同意画面: **User type = Internal**（不変条件 5 の実効手段。Google の審査不要）
3. OAuth クライアント（Web application）。Authorized redirect URI: `https://<domain>/oauth2callback`（`WORKSPACE_EXTERNAL_URL` と正確に一致させる）
4. シークレット登録: `aws secretsmanager create-secret --name gws-mcp/oauth --secret-string '{"clientId":"...","clientSecret":"...","jwtSigningKey":"..."}'`（実値は CLI でのみ扱い、リポジトリにコミットしない）

## 7. 現在の状態と残作業

**完了済み（2026-08-23）:**
- `infra/` に CDK アプリ実装（`bin/gws-mcp.ts`, `lib/gws-mcp-stack.ts`, `README.md` ほか）
- `tsc --noEmit` 通過、ダミー context での `cdk synth` 成功
- 合成テンプレートで desiredCount 1 / minHealthyPercent 0 / ARM64 / `/health` ヘルスチェック / タスク SG が ALB 限定 / NAT なし を確認済み

**未着手:**
1. GCP 設定（セクション 6）
2. Secrets Manager シークレット作成
3. ドメイン名の決定と `cdk bootstrap` / `cdk diff` / `cdk deploy`（デプロイはユーザー承認必須）
4. IS&CorpIT へのセキュリティレビュー依頼（従業員の Gmail/Drive データを AWS 上で扱うため、公開前に必須）
5. E2E 検証（下記）

**検証手順:**
1. `curl -sf https://<domain>/health` → service/version JSON
2. `curl -sf https://<domain>/.well-known/oauth-authorization-server` → `registration_endpoint` を含むメタデータ
3. MCP Inspector（`npx @modelcontextprotocol/inspector`）で `/mcp` に接続し OAuth フロー完走
4. Claude Code: `claude mcp add --transport http google-workspace https://<domain>/mcp` → `/mcp` で認証 → ツール実行
5. claude.ai カスタムコネクタ追加（コールバック URL 拒否時はログを見て env に追記）
6. ネガティブテスト: soracom.jp 以外の Google アカウントが Google 側でブロックされること

## 8. 将来の拡張パス

- **水平スケール / HA**: 上流でトークンストアの外部化（Redis 等）が入るのを待つのが本筋。自前で入れる場合は `RuntimeStore`（`src/oauth.rs`）の永続化バックエンド追加が必要
- **機械的クライアント（CI・スクリプト）**: `EXTERNAL_OAUTH21_PROVIDER=true` の ExternalGoogle モードを別エンドポイントで併設する選択肢あり（クライアントが Google トークンを持参）
- **添付ファイル機能が必要になったら**: stateless を外して単一の状態保持インスタンスにするか、`src/native/common/attachments.rs` を S3 バックエンドに書き換える

## 9. 新リポジトリ移行時のチェックリスト

- [ ] `infra/` 一式と本ドキュメント（`docs/DESIGN_DECISIONS.md`）を移行
- [ ] 新リポジトリの `CLAUDE.md` に「インフラ変更前に `docs/DESIGN_DECISIONS.md` の不変条件を読むこと」と記載する
- [ ] 本ドキュメント内のコード行参照（`src/oauth.rs:279` 等）が移行後もズレていないか確認（大きくリファクタしたら本ドキュメントを更新）
- [ ] `Dockerfile` / `.dockerignore` のパス前提（ビルドコンテキスト = リポジトリルート、`infra/` は allowlist 外）が保たれているか確認
- [ ] Secrets Manager のシークレット名（`gws-mcp/oauth`）や context 値（`domainName` 等）を変える場合は `infra/README.md` と CDK context を同時に更新
