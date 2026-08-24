<!-- mcp-name: io.github.soramash.google-workspace-mcp-rs -->

# Google Workspace MCP Server for Rust

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust 1.98+](https://img.shields.io/badge/Rust-1.98%2B-orange.svg)](https://www.rust-lang.org/)

[日本語](README_ja.md)

A native Rust server for accessing Google Workspace through the Model Context Protocol (MCP), with no Python runtime required. It provides 121 tools across Gmail, Drive, Calendar, Docs, Sheets, Slides, Forms, Tasks, Contacts, Chat, Apps Script, and Google Programmable Search.

Both local stdio and Streamable HTTP transports are supported. For future long-running remote MCP deployments, the server implements OAuth 2.1, trusted gateway integration, Origin validation, per-principal session isolation, resource limits, and a non-root Docker image.

## Project policy

This repository will be developed and maintained according to the following policies:

- The Rust implementation under `src/` is the only authoritative server implementation.
- Python is not used to build, test, release, or run the server or its container.
- New features and bug fixes are implemented in Rust.
- The MCP contract for all 121 tools and its security boundaries are preserved.
- Remote deployments require HTTPS and a verified principal.
- Dependency versions are pinned in `Cargo.toml`; changes to `Cargo.lock` must be reviewed.

The Python assets, including `main.py`, `pyproject.toml`, `auth/`, `core/`, and the various `g*` directories, are legacy references retained for reviewing the migration source. The Rust server neither starts nor imports them, and they are excluded from the Docker build context. Do not use them as the target for future feature development.

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

Check the compiled registry with:

```bash
cargo run --locked -- --rust-migration-status
```

The expected output is `native tools: 121/121` and `unavailable tools: 0/121`.

## Architecture

The main implementation is organized as follows:

| Path | Responsibility |
|---|---|
| `src/main.rs` | Rust server entry point |
| `src/mcp.rs` | JSON-RPC/MCP lifecycle, contract loading, and schema validation |
| `src/transport.rs` | stdio, stateful/stateless Streamable HTTP, and attachments |
| `src/oauth.rs` | Internal/external OAuth 2.1, trusted gateway, and legacy authentication callback |
| `src/auth.rs` | Request-local principal, Google token, credential store, and service account/DWD |
| `src/registry.rs` | Service/tier/read-only registry for 121 tools |
| `src/native/` | Native tool implementations for each Google Workspace service |
| `contracts/` | Fixed MCP tool contracts for legacy, OAuth 2.1, and gateway modes |

## Requirements

- Rust 1.98 or later
- A Google Cloud project
- The Google APIs you plan to use must be enabled
- An OAuth client, or service account / trusted gateway configuration
- For remote deployments, an HTTPS endpoint and a reverse proxy or load balancer

## Build

```bash
cargo build --release --locked
./target/release/workspace-mcp-rs --rust-migration-status
```

The binary is generated at `target/release/workspace-mcp-rs`.

## Use as a local MCP server

### stdio

Legacy stdio mode is intentionally single-user. The principal is fixed by `USER_GOOGLE_EMAIL` and cannot be changed through tool arguments or a session ID.

```bash
export USER_GOOGLE_EMAIL="user@example.com"
export GOOGLE_OAUTH_CLIENT_ID="..."
export GOOGLE_OAUTH_CLIENT_SECRET="..."

./target/release/workspace-mcp-rs \
  --transport stdio \
  --tool-tier complete
```

On first use, call `start_google_auth` to complete Google authorization. Credentials are stored in `~/.google_workspace_mcp/credentials` by default. Set `WORKSPACE_MCP_CREDENTIALS_DIR` to use a different location.

Example MCP client configuration:

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

## Use as a remote MCP server

The remote MCP endpoint is the Streamable HTTP endpoint at `/mcp`. Loopback HTTP is acceptable in development, but HTTPS termination is mandatory when exposing the server to the internet or a corporate network.

### Endpoints

| Path | Purpose |
|---|---|
| `/mcp` | MCP Streamable HTTP (POST/GET/DELETE) |
| `/health` | Process health check |
| `/.well-known/oauth-authorization-server` | OAuth authorization server metadata |
| `/.well-known/oauth-protected-resource/mcp` | Protected resource metadata |
| `/register` | Dynamic Client Registration |
| `/authorize` | OAuth authorization endpoint |
| `/oauth2callback` | Google OAuth callback |
| `/token` | OAuth token endpoint |
| `/attachments/{file_id}` | Authenticated, owner-scoped attachment download |

### Recommended initial deployment

For the first remote deployment, use the following configuration:

1. Run a single server instance.
2. Use Docker or the release binary.
3. Terminate HTTPS with nginx, Caddy, Cloud Load Balancer, or an equivalent service.
4. Enable the native internal OAuth 2.1 implementation.
5. Have the server listen on `0.0.0.0:8000` within a private network.
6. Expose only `443/tcp` to the public internet.
7. Back up the credential volume and secrets, and apply appropriate access controls.

### Google Cloud configuration

If the public URL is `https://mcp.example.com`, for example, register the following as an authorized redirect URI for the Google OAuth client:

```text
https://mcp.example.com/oauth2callback
```

The Google OAuth callback and the MCP client's own OAuth callback are separate endpoints.

- `GOOGLE_OAUTH_REDIRECT_URI`: the URL Google uses to return to this server
- `WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS`: the allowlist of URLs this server may use to return to MCP clients

### Production environment variables

The following is an example `.env` file. Never commit real secrets to the repository.

```dotenv
MCP_ENABLE_OAUTH21=true
GOOGLE_OAUTH_CLIENT_ID=your-client-id.apps.googleusercontent.com
GOOGLE_OAUTH_CLIENT_SECRET=replace-with-secret

WORKSPACE_EXTERNAL_URL=https://mcp.example.com
GOOGLE_OAUTH_REDIRECT_URI=https://mcp.example.com/oauth2callback
WORKSPACE_MCP_HOST=0.0.0.0
WORKSPACE_MCP_PORT=8000

# Keep Dynamic Client Registration valid for 30 days in long-running deployments
WORKSPACE_MCP_REGISTERED_CLIENT_TTL_SECONDS=2592000

# List only the callbacks used by your MCP clients
WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS=https://client.example.com/oauth/callback,http://127.0.0.1:*/callback

# Set this only when requests from browser clients must be allowed
WORKSPACE_MCP_ALLOWED_ORIGINS=https://mcp.example.com,https://client.example.com

# Use at least 32 bytes of random signing material, separate from the Google client secret
FASTMCP_SERVER_AUTH_GOOGLE_JWT_SIGNING_KEY=replace-with-at-least-32-random-bytes
```

For example, generate signing material with:

```bash
openssl rand -hex 32
```

### Docker Compose

```bash
docker compose up --build -d
# For older Compose installations: docker-compose up --build -d
```

`docker-compose.yml` applies the following hardening measures:

- A multi-stage image containing only the Rust binary
- A non-root user (UID/GID 10001)
- A read-only root filesystem
- `no-new-privileges`
- A bounded `/tmp` with `noexec,nosuid`
- A dedicated credential volume

Check the deployment status with:

```bash
docker compose ps
docker compose logs -f gws_mcp
curl --fail https://mcp.example.com/health
```

### Run the binary directly

```bash
./target/release/workspace-mcp-rs \
  --transport streamable-http \
  --tool-tier complete
```

For long-running deployments, use systemd, launchd, supervisor, or a similar process manager to handle restarts and secret injection. Server logs are written to stderr and should be collected by the service manager or container runtime.

### nginx reverse proxy example

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

        # Do not buffer long-lived Streamable HTTP GET/SSE connections
        proxy_buffering off;
        proxy_read_timeout 3600s;
    }
}
```

Set the public URL explicitly with `WORKSPACE_EXTERNAL_URL`; do not infer it from forwarded headers.

## Authentication modes

| Mode | Main configuration | Recommended use |
|---|---|---|
| Legacy single-user | `USER_GOOGLE_EMAIL` | Local stdio only |
| Internal OAuth 2.1 | `MCP_ENABLE_OAUTH21=true` | Initial single-instance remote deployment |
| External Google bearer | OAuth 2.1 + `EXTERNAL_OAUTH21_PROVIDER=true` | Deployments that obtain Google bearer tokens externally |
| Trusted gateway | `TRUST_GATEWAY_IDENTITY=true` + JWKS | Organization use behind an identity-aware proxy |
| Service account / DWD | Service account key + domain policy | Managed Workspace domains |

At a minimum, configure the following for a trusted gateway:

```dotenv
TRUST_GATEWAY_IDENTITY=true
GATEWAY_IDENTITY_JWKS_URL=https://gateway.example.com/.well-known/jwks.json
GATEWAY_IDENTITY_AUDIENCE=https://mcp.example.com
GATEWAY_IDENTITY_ISSUER=https://gateway.example.com
GATEWAY_IDENTITY_ALGORITHMS=ES256
```

The JWKS URL is validated to mitigate SSRF. HMAC tokens and unverified identity headers are not accepted.

If the DWD subject changes per request when using a service account, `DWD_ALLOWED_DOMAINS` is required in addition to a verified gateway principal.

## Stateful, stateless, and horizontal scaling

### Stateful mode

This is the default Streamable HTTP mode. MCP sessions are stored in process memory and bound to a verified principal.

- Attachment downloads are available.
- The server maintains the session lifecycle.
- Sessions are lost when the server restarts.
- Sticky sessions are required behind a load balancer.

### Stateless mode

```dotenv
WORKSPACE_MCP_STATELESS_MODE=true
```

Stateless mode is available only with OAuth 2.1. Because each MCP request can be handled independently, this mode is well suited to container deployments, but server-side attachment storage is unavailable. Horizontal scaling is safe only when authentication state is externalized, such as with external Google bearer mode or a trusted gateway. Internal OAuth 2.1 still keeps Dynamic Client Registration, authorization codes, and tokens in process-local memory.

### Current high-availability considerations

Runtime state such as MCP sessions, Dynamic Client Registration, authorization codes, and local reference tokens is process-local. When scaling internal OAuth 2.1 across multiple instances, simple round-robin load balancing is not sufficient.

For now, choose one of the following approaches:

- Run internal OAuth 2.1 on a single instance.
- Use sticky sessions and plan for reauthentication.
- Use external Google bearer mode.
- Use a trusted gateway to attach identity to every request.

Until a shared state backend is implemented, do not assume internal OAuth state can be shared across multiple instances.

## Resource limits

### HTTP sessions

| Environment variable | Default |
|---|---:|
| `WORKSPACE_MCP_MAX_SESSIONS` | 4096 |
| `WORKSPACE_MCP_MAX_SESSIONS_PER_PRINCIPAL` | 64 |
| `WORKSPACE_MCP_SESSION_IDLE_TTL_SECONDS` | 1800 |
| `WORKSPACE_MCP_MAX_BODY_BYTES` | 4194304 |

The server returns HTTP `429` when capacity is reached. To prevent information disclosure, both an expired session and a session ID belonging to another principal produce the same `404` response.

### OAuth Dynamic Client Registration

| Environment variable | Default |
|---|---:|
| `WORKSPACE_MCP_MAX_REGISTERED_CLIENTS` | 4096 |
| `WORKSPACE_MCP_REGISTERED_CLIENT_TTL_SECONDS` | 86400 |

`WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS` is required for internal OAuth 2.1. A loopback wildcard may be used only for the port; the path must match exactly.

```text
http://127.0.0.1:*/callback
```

## Tool selection and least privilege

All 121 tools are enabled by default. In production, restrict the server to only the services and permissions you need.

```bash
workspace-mcp-rs --tools gmail drive calendar
workspace-mcp-rs --tool-tier core
workspace-mcp-rs --read-only
workspace-mcp-rs --permissions gmail=read drive=write tasks=manage
workspace-mcp-rs --disabled-tools send_gmail_message manage_drive_access
```

Corresponding environment variables:

- `WORKSPACE_MCP_TOOLS`
- `WORKSPACE_MCP_TOOL_TIER`
- `WORKSPACE_MCP_READ_ONLY`
- `WORKSPACE_MCP_PERMISSIONS`
- `WORKSPACE_MCP_DISABLED_TOOLS`

## Security checklist for remote deployment

Before making a deployment public, verify at least the following:

- [ ] The public endpoint accepts HTTPS only.
- [ ] `WORKSPACE_EXTERNAL_URL` matches the actual HTTPS URL.
- [ ] The Google OAuth redirect URI matches exactly.
- [ ] The DCR redirect URI allowlist contains only the required clients.
- [ ] When browser clients are used, the Origin allowlist contains only the required origins.
- [ ] Legacy mode is not used on a public endpoint.
- [ ] OAuth secrets, signing keys, and service account keys are managed by a secret manager.
- [ ] The credential volume is backed up, encrypted, and access-controlled.
- [ ] `/mcp` is not exposed directly to the internet over plaintext HTTP.
- [ ] Tool, service, and read-only permissions are minimized.
- [ ] Session, client, and body limits are appropriate for the expected traffic.
- [ ] Reverse proxy and application logs are monitored.
- [ ] Operations account for the loss of sessions and OAuth runtime state after a process restart.

On Unix, the credential directory is corrected to mode `0700`, and symlinks are rejected. Credential files are opened with `O_NOFOLLOW` and must be owned by the current user, be regular files, have mode `0600`, and have exactly 1 hard link.

Request identity is derived only from verified transport authentication. Tool arguments and `Mcp-Session-Id` are never trusted as identity.

See [`docs/security/vulnerability-remediation-plan.md`](docs/security/vulnerability-remediation-plan.md) for the detailed vulnerability remediation history.

## Monitoring and operations

### Health check

```bash
curl --fail https://mcp.example.com/health
```

A healthy server returns:

```json
{
  "service": "workspace-mcp-rs",
  "status": "ok",
  "transport": "streamable-http",
  "version": "0.1.0"
}
```

`/health` indicates that the process can accept requests. It is not an end-to-end readiness check for Google APIs, Google OAuth, or the JWKS provider. Configure separate synthetic monitoring for external dependencies.

### Graceful shutdown

On SIGINT/SIGTERM, the HTTP server begins a graceful shutdown. During deployments, remove the instance from the load balancer before stopping it, and allow enough drain time for long-lived requests.

### Backup scope

- Credential volume
- Deployment secrets
- OAuth client configuration
- Service account key management information, when a service account key is used
- Reverse proxy configuration

MCP sessions and in-memory OAuth runtime state are not backed up and must be re-established after a restart.

## Maintenance

### Routine validation

Run all of the following after making changes:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo run --locked -- --rust-migration-status
```

Expected results:

- 0 test failures
- 0 warnings
- 121/121 native tools
- 0/121 unavailable tools

### Changing contracts

Contracts are fixed separately for the following three modes:

- `contracts/tools-legacy.json`: 121 tools
- `contracts/tools-oauth21.json`: 120 tools (excluding `start_google_auth`)
- `contracts/tools-gateway.json`: 121 tools

When adding a tool or changing an argument or schema, update the Rust implementation, `src/registry.rs`, and the applicable contracts in the same change. Verify at least the following:

- `tool_count` matches the actual array length.
- Tool names are unique.
- Legacy and gateway name sets match.
- The OAuth 2.1 set contains every legacy tool except `start_google_auth`.
- `tools/list` returns the expected schema.
- Native dispatch has a destination for every tool.

### Dependency updates

1. Deliberately update the version in `Cargo.toml`.
2. Update the lockfile with `cargo update -p <crate> --precise <version>`.
3. Review the release notes and security advisories.
4. Run the full validation suite and a Docker build.
5. Confirm that the image contains no unnecessary runtimes or tools.

Do not add open version ranges to server dependencies.

### Docker validation

```bash
docker build -t workspace-mcp-rs:local .
docker run --rm workspace-mcp-rs:local --rust-migration-status
docker compose config
docker compose build
```

Do not install `python`, `python3`, `pip`, or `uv` in the runtime image.

## Release procedure

1. Keep the versions in `Cargo.toml` and `server.json` in sync.
2. Run the full validation suite.
3. Build the release binary and Docker image.
4. Check the container's `/health` endpoint and `--rust-migration-status` output.
5. Create a semver tag.
6. Publish the OCI image to the registry.
7. Record the digest and pin deployments to it whenever possible.
8. In staging, verify OAuth login, MCP initialization, `tools/list`, and representative read/write tools.
9. Roll out to production gradually.

`server.json` designates `ghcr.io/soramash/google_workspace_mcp_rs` as the planned OCI package identifier. Do not present it as an available public image until it has actually been published to the registry.

## Known limitations and roadmap

Current primary limitations:

- E2E testing with a real Google account is required in each environment.
- Internal OAuth/session runtime state is process-local.
- The attachment route is unavailable in stateless mode.
- `/health` does not check the readiness of external dependencies.
- An automated OCI image publishing pipeline still needs to be implemented.

Priorities for remote MCP operations:

1. E2E testing in staging with a real Google test account
2. Automation for the HTTPS domain, certificate, and reverse proxy
3. Secret manager integration and a credential backup policy
4. Structured logging, metrics, and alerting
5. OCI release automation, SBOM generation, and image signing
6. An HA design using shared OAuth/session state or a gateway
7. Gradual archival or removal of the legacy Python reference

## License

MIT License. See [`LICENSE`](LICENSE) for details.
