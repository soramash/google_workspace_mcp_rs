<!-- mcp-name: io.github.soramash.google-workspace-mcp-rs -->

# Google Workspace MCP Server for Rust

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust 1.98+](https://img.shields.io/badge/Rust-1.98%2B-orange.svg)](https://www.rust-lang.org/)

Google WorkspaceをModel Context Protocol（MCP）から操作する、Pythonランタイム不要のRustネイティブサーバです。Gmail、Drive、Calendar、Docs、Sheets、Slides、Forms、Tasks、Contacts、Chat、Apps Script、Google Programmable Searchの合計121ツールを提供します。

現在はローカルのstdioとStreamable HTTPの両方に対応しています。将来の常設リモートMCP運用を想定し、OAuth 2.1、trusted gateway、Origin検証、principal単位のsession分離、resource limit、non-root Docker imageを実装しています。

## プロジェクト方針

このrepositoryでは、今後次の方針で開発・保守します。

- `src/`以下のRust実装を唯一の正式なserver実装とする
- serverのbuild・test・release・container実行にPythonを使用しない
- 新機能とbug fixはRust側へ実装する
- 121ツールのMCP contractとセキュリティ境界を維持する
- remote deploymentではHTTPSと検証済みprincipalを必須とする
- dependencyは`Cargo.toml`でversionを固定し、`Cargo.lock`を更新・reviewする

`main.py`、`pyproject.toml`、`auth/`、`core/`、各`g*` directoryなどのPython資産は、移行元を確認するためのlegacy referenceです。Rust serverからは起動・importされず、Docker build contextにも含まれません。今後の機能追加先にはしないでください。

## Native tool coverage

| Service | Tools | Service | Tools |
|---|---:|---|---:|
| Gmail | 15 | Drive | 16 |
| Calendar | 7 | Docs | 19 |
| Sheets | 14 | Slides | 7 |
| Forms | 6 | Tasks | 6 |
| Contacts | 8 | Chat | 6 |
| Apps Script | 15 | Custom Search | 2 |
| **Total** | **121** | | |

compiled registryの状態は次のコマンドで確認できます。

```bash
cargo run --locked -- --rust-migration-status
```

期待される結果は`native tools: 121/121`、`unavailable tools: 0/121`です。

## Architecture

主要な実装は以下にあります。

| Path | Responsibility |
|---|---|
| `src/main.rs` | Rust server entrypoint |
| `src/mcp.rs` | JSON-RPC/MCP lifecycle、contract読込、schema validation |
| `src/transport.rs` | stdio、stateful/stateless Streamable HTTP、attachments |
| `src/oauth.rs` | internal/external OAuth 2.1、trusted gateway、legacy auth callback |
| `src/auth.rs` | request-local principal、Google token、credential store、service account/DWD |
| `src/registry.rs` | 121ツールのservice/tier/read-only registry |
| `src/native/` | Google Workspace各serviceのnative tool implementation |
| `contracts/` | legacy、OAuth 2.1、gateway別の固定MCP tool contract |

## Requirements

- Rust 1.98以上
- Google Cloud project
- 利用するGoogle APIの有効化
- OAuth client、またはservice account / trusted gateway設定
- remote deploymentの場合はHTTPS endpointとreverse proxyまたはload balancer

## Build

```bash
cargo build --release --locked
./target/release/workspace-mcp-rs --rust-migration-status
```

binaryは`target/release/workspace-mcp-rs`に生成されます。

## ローカルMCPとして利用する

### stdio

legacy stdio modeは意図的にsingle-userです。principalは`USER_GOOGLE_EMAIL`で固定され、tool argumentやsession IDから切り替えることはできません。

```bash
export USER_GOOGLE_EMAIL="user@example.com"
export GOOGLE_OAUTH_CLIENT_ID="..."
export GOOGLE_OAUTH_CLIENT_SECRET="..."

./target/release/workspace-mcp-rs \
  --transport stdio \
  --tool-tier complete
```

初回は`start_google_auth`を呼び出してGoogle認可を完了します。credentialは既定で`~/.google_workspace_mcp/credentials`へ保存されます。保存先は`WORKSPACE_MCP_CREDENTIALS_DIR`で変更できます。

MCP client設定例:

```json
{
  "mcpServers": {
    "google-workspace": {
      "command": "/absolute/path/to/workspace-mcp-rs",
      "args": ["--transport", "stdio", "--tool-tier", "complete"],
      "env": {
        "USER_GOOGLE_EMAIL": "user@example.com",
        "GOOGLE_OAUTH_CLIENT_ID": "...",
        "GOOGLE_OAUTH_CLIENT_SECRET": "..."
      }
    }
  }
}
```

## リモートMCPとして利用する

remote MCP endpointはStreamable HTTPの`/mcp`です。開発環境ではloopback HTTPを使用できますが、internetや社内networkへ公開するときは必ずHTTPS terminationを配置してください。

### Endpoint一覧

| Path | Purpose |
|---|---|
| `/mcp` | MCP Streamable HTTP（POST/GET/DELETE） |
| `/health` | process health check |
| `/.well-known/oauth-authorization-server` | OAuth authorization server metadata |
| `/.well-known/oauth-protected-resource/mcp` | protected resource metadata |
| `/register` | Dynamic Client Registration |
| `/authorize` | OAuth authorization endpoint |
| `/oauth2callback` | Google OAuth callback |
| `/token` | OAuth token endpoint |
| `/attachments/{file_id}` | authenticated、owner-scoped attachment download |

### 推奨する最初の構成

最初のremote deploymentは次の構成を推奨します。

1. serverは1 instanceで起動する
2. Dockerまたはrelease binaryを使用する
3. nginx、Caddy、Cloud Load BalancerなどでHTTPSを終端する
4. native internal OAuth 2.1を有効にする
5. serverはprivate networkの`0.0.0.0:8000`でlistenする
6. public internetへは`443/tcp`だけを公開する
7. credential volumeとsecretをbackup対象・access制御対象にする

### Google Cloud側の設定

例としてpublic URLを`https://mcp.example.com`とする場合、Google OAuth clientのauthorized redirect URIへ次を登録します。

```text
https://mcp.example.com/oauth2callback
```

Google OAuth callbackと、MCP client自身のOAuth callbackは別物です。

- `GOOGLE_OAUTH_REDIRECT_URI`: Googleからこのserverへ戻るURL
- `WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS`: このserverからMCP clientへ戻るURLのallowlist

### Production用環境変数

`.env`の例です。実際のsecretをrepositoryへcommitしないでください。

```dotenv
MCP_ENABLE_OAUTH21=true
GOOGLE_OAUTH_CLIENT_ID=your-client-id.apps.googleusercontent.com
GOOGLE_OAUTH_CLIENT_SECRET=replace-with-secret

WORKSPACE_EXTERNAL_URL=https://mcp.example.com
GOOGLE_OAUTH_REDIRECT_URI=https://mcp.example.com/oauth2callback
WORKSPACE_MCP_HOST=0.0.0.0
WORKSPACE_MCP_PORT=8000

# 利用するMCP clientのcallbackだけを列挙する
WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS=https://client.example.com/oauth/callback,http://127.0.0.1:*/callback

# Browser由来requestを許可する場合だけ明示する
WORKSPACE_MCP_ALLOWED_ORIGINS=https://mcp.example.com,https://client.example.com

# Google client secretとは別の32 bytes以上のrandom signing materialを推奨
FASTMCP_SERVER_AUTH_GOOGLE_JWT_SIGNING_KEY=replace-with-at-least-32-random-bytes
```

signing materialは例えば次のように生成できます。

```bash
openssl rand -hex 32
```

### Docker Compose

```bash
docker compose up --build -d
# 古いCompose環境では: docker-compose up --build -d
```

`docker-compose.yml`は次のhardeningを適用します。

- Rust binaryだけを含むmulti-stage image
- non-root user（UID/GID 10001）
- read-only root filesystem
- `no-new-privileges`
- `noexec,nosuid`のbounded `/tmp`
- credential専用volume

状態確認:

```bash
docker compose ps
docker compose logs -f gws_mcp
curl --fail https://mcp.example.com/health
```

### Binaryを直接起動する

```bash
./target/release/workspace-mcp-rs \
  --transport streamable-http \
  --tool-tier complete
```

常設運用ではsystemd、launchd、supervisorなどを使用し、process restartとsecret injectionを管理してください。server logはstderrへ出力されるため、service managerまたはcontainer runtimeで収集します。

### nginx reverse proxy例

```nginx
server {
    listen 443 ssl http2;
    server_name mcp.example.com;

    ssl_certificate     /etc/letsencrypt/live/mcp.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/mcp.example.com/privkey.pem;

    client_max_body_size 4m;

    location / {
        proxy_pass http://127.0.0.1:8000;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;

        # Streamable HTTPのlong-lived GET/SSEをbufferしない
        proxy_buffering off;
        proxy_read_timeout 3600s;
    }
}
```

public URLはforwarded headerから推測させず、`WORKSPACE_EXTERNAL_URL`へ明示してください。

## Authentication mode

| Mode | Main configuration | Recommended use |
|---|---|---|
| Legacy single-user | `USER_GOOGLE_EMAIL` | local stdio only |
| Internal OAuth 2.1 | `MCP_ENABLE_OAUTH21=true` | 最初のsingle-instance remote deployment |
| External Google bearer | OAuth 2.1 + `EXTERNAL_OAUTH21_PROVIDER=true` | Google bearer tokenを外部で取得する構成 |
| Trusted gateway | `TRUST_GATEWAY_IDENTITY=true` + JWKS | identity-aware proxy配下、organization利用 |
| Service account / DWD | service account key + domain policy | 管理されたWorkspace domain |

trusted gatewayでは少なくとも次を設定します。

```dotenv
TRUST_GATEWAY_IDENTITY=true
GATEWAY_IDENTITY_JWKS_URL=https://gateway.example.com/.well-known/jwks.json
GATEWAY_IDENTITY_AUDIENCE=https://mcp.example.com
GATEWAY_IDENTITY_ISSUER=https://gateway.example.com
GATEWAY_IDENTITY_ALGORITHMS=ES256
```

JWKS URLはSSRF対策の検証対象です。HMAC tokenや未検証のidentity headerは使用できません。

service accountでrequestごとにDWD subjectが変わる構成では、verified gateway principalに加えて`DWD_ALLOWED_DOMAINS`が必要です。

## Stateful、stateless、水平スケール

### Stateful mode

既定のStreamable HTTP modeです。MCP sessionはprocess memoryに保存され、verified principalへbindされます。

- attachment downloadを利用できる
- session lifecycleをserverが保持する
- restart時にsessionは失われる
- load balancer配下ではsticky sessionが必要

### Stateless mode

```dotenv
WORKSPACE_MCP_STATELESS_MODE=true
```

OAuth 2.1でのみ使用できます。MCP requestごとに独立して処理できるためcontainer運用と水平スケールに向きますが、server-side attachment storageは利用できません。

### 現在のHA上の注意

MCP session、Dynamic Client Registration、authorization code、local reference tokenなどのruntime stateはprocess-localです。internal OAuth 2.1を複数instanceへ拡張する場合、単純なround-robinだけでは不十分です。

現時点では次のいずれかを選択してください。

- internal OAuth 2.1を1 instanceで運用する
- sticky sessionと計画的な再認証を使用する
- external Google bearer modeを使用する
- trusted gatewayでidentityを各requestに付与する

shared state backendを実装するまでは、internal OAuth stateを複数instanceで共有できるとは扱わないでください。

## Resource limits

### HTTP session

| Environment variable | Default |
|---|---:|
| `WORKSPACE_MCP_MAX_SESSIONS` | 4096 |
| `WORKSPACE_MCP_MAX_SESSIONS_PER_PRINCIPAL` | 64 |
| `WORKSPACE_MCP_SESSION_IDLE_TTL_SECONDS` | 1800 |
| `WORKSPACE_MCP_MAX_BODY_BYTES` | 4194304 |

capacity到達時はHTTP `429`、expired sessionまたは別principalのsession IDは情報漏えいを防ぐため同じ`404`を返します。

### OAuth Dynamic Client Registration

| Environment variable | Default |
|---|---:|
| `WORKSPACE_MCP_MAX_REGISTERED_CLIENTS` | 4096 |
| `WORKSPACE_MCP_REGISTERED_CLIENT_TTL_SECONDS` | 86400 |

`WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS`はinternal OAuth 2.1で必須です。loopback wildcardはport部分だけに使用でき、pathは完全一致します。

```text
http://127.0.0.1:*/callback
```

## Tool selectionと権限縮小

全121ツールが既定で有効です。本番では必要なserviceと権限だけに絞ることを推奨します。

```bash
workspace-mcp-rs --tools gmail drive calendar
workspace-mcp-rs --tool-tier core
workspace-mcp-rs --read-only
workspace-mcp-rs --permissions gmail=read drive=write tasks=manage
workspace-mcp-rs --disabled-tools send_gmail_message manage_drive_access
```

対応する環境変数:

- `WORKSPACE_MCP_TOOLS`
- `WORKSPACE_MCP_TOOL_TIER`
- `WORKSPACE_MCP_READ_ONLY`
- `WORKSPACE_MCP_PERMISSIONS`
- `WORKSPACE_MCP_DISABLED_TOOLS`

## Security checklist for remote deployment

公開前に最低限次を確認してください。

- [ ] public endpointがHTTPSのみである
- [ ] `WORKSPACE_EXTERNAL_URL`が実際のHTTPS URLと一致する
- [ ] Google OAuth redirect URIが完全一致する
- [ ] DCR redirect URI allowlistを必要なclientだけに限定した
- [ ] browser clientを使う場合、Origin allowlistを必要なoriginだけに限定した
- [ ] legacy modeをpublic endpointで使用していない
- [ ] OAuth secret、signing key、service account keyをsecret managerで管理した
- [ ] credential volumeがbackup・暗号化・access制御されている
- [ ] `/mcp`を直接internetへ平文公開していない
- [ ] tool/service/read-only permissionを最小化した
- [ ] session/client/body limitをtraffic量に合わせて確認した
- [ ] reverse proxyとapplication logを監視している
- [ ] process restart時にsession・OAuth runtime stateが失われることを運用へ織り込んだ

credential directoryはUnixで`0700`へ補正され、symlinkを拒否します。credential fileは`O_NOFOLLOW`でopenされ、current user所有、regular file、mode `0600`、hardlink数1を要求します。

request identityはverified transport authenticationからのみ取得されます。tool argumentや`Mcp-Session-Id`をidentityとして信頼しません。

詳細な脆弱性対応履歴は[`docs/security/vulnerability-remediation-plan.md`](docs/security/vulnerability-remediation-plan.md)を参照してください。

## Monitoring and operations

### Health check

```bash
curl --fail https://mcp.example.com/health
```

正常時は次の形式を返します。

```json
{
  "service": "workspace-mcp-rs",
  "status": "ok",
  "transport": "streamable-http",
  "version": "0.1.0"
}
```

`/health`はprocessがrequestを受けられることを示します。Google API、Google OAuth、JWKS providerまで含むend-to-end readinessではありません。外部dependencyのsynthetic monitoringは別途用意してください。

### Graceful shutdown

SIGINT/SIGTERMを受けるとHTTP serverはgraceful shutdownへ移行します。deployment時はload balancerからinstanceを外してから終了し、long-lived requestのdrain時間を確保してください。

### Backup対象

- credential volume
- deployment secret
- OAuth client設定
- service account keyを使う場合はその管理情報
- reverse proxy設定

MCP sessionやin-memory OAuth runtime stateはbackup対象ではなく、restart後に再確立します。

## Maintenance

### 日常のvalidation

変更後は以下をすべて実行します。

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo run --locked -- --rust-migration-status
```

期待値:

- test failureが0件
- warningが0件
- native toolsが121/121
- unavailable toolsが0/121

### Contractを変更するとき

contractは以下の3 modeで別々に固定されています。

- `contracts/tools-legacy.json`: 121 tools
- `contracts/tools-oauth21.json`: 120 tools（`start_google_auth`を除く）
- `contracts/tools-gateway.json`: 121 tools

新規tool、argument、schemaを変更するときは、Rust implementation、`src/registry.rs`、対象contractを同じchangeで更新してください。少なくとも以下を確認します。

- `tool_count`と実際の配列長が一致する
- tool nameが重複していない
- legacyとgatewayのname setが一致する
- OAuth 2.1はlegacyから`start_google_auth`だけを除いた集合である
- `tools/list`が期待するschemaを返す
- native dispatchに全toolの到達先がある

### Dependency update

1. `Cargo.toml`のversionを意図的に更新する
2. `cargo update -p <crate> --precise <version>`でlockfileを更新する
3. release noteとsecurity advisoryを確認する
4. full validationとDocker buildを実行する
5. image内に不要なruntimeやtoolが増えていないことを確認する

server dependencyへopen version rangeを追加しないでください。

### Docker validation

```bash
docker build -t workspace-mcp-rs:local .
docker run --rm workspace-mcp-rs:local --rust-migration-status
docker compose config
docker compose build
```

runtime imageには`python`、`python3`、`pip`、`uv`を導入しません。

## Release procedure

1. `Cargo.toml`と`server.json`のversionを揃える
2. full validationを実行する
3. release binaryとDocker imageをbuildする
4. containerの`/health`と`--rust-migration-status`を確認する
5. semver tagを作成する
6. OCI imageをregistryへpublishする
7. digestを記録し、deploymentでは可能ならdigest pinする
8. stagingでOAuth login、MCP initialize、`tools/list`、代表的なread/write toolを確認する
9. productionへ段階的にrolloutする

`server.json`は`ghcr.io/soramash/google_workspace_mcp_rs`をOCI package identifierとして予定しています。registryへ実際にpublishするまでは、利用可能なpublic imageとして案内しないでください。

## Known limitations and roadmap

現在の主な制約:

- 実Google accountを使うE2Eは環境ごとに必要
- internal OAuth/session runtime stateはprocess-local
- stateless modeではattachment routeを利用できない
- `/health`は外部dependencyのreadinessを検査しない
- OCI imageの自動publish pipelineは別途整備が必要

remote MCP運用に向けた優先roadmap:

1. staging環境と実Google test accountによるE2E
2. HTTPS domain、certificate、reverse proxyの自動化
3. secret manager連携とcredential backup policy
4. structured logging、metrics、alerting
5. OCI release automation、SBOM、image signing
6. shared OAuth/session stateまたはgateway方式によるHA設計
7. legacy Python referenceの段階的なarchiveまたは削除

## License

MIT License。詳細は[`LICENSE`](LICENSE)を参照してください。
