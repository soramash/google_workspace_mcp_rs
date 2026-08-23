# infra — AWS deployment (CDK)

Deploys `workspace-mcp-rs` as a remote MCP server (Streamable HTTP) on ECS Fargate:

```
Internet ──443──> ALB (ACM cert, HTTP→HTTPS redirect, idle timeout 3600s)
                    │  target group :8000, health check /health
                    ▼
            Fargate task x1 (arm64 Graviton, 0.25 vCPU / 512 MB)
            public subnet + public IP (no NAT), SG admits only the ALB
            secrets from Secrets Manager (gws-mcp/oauth)
```

Auth mode: **OAuth 2.1 internal AS** (`MCP_ENABLE_OAUTH21=true`) with
`WORKSPACE_MCP_STATELESS_MODE=true`. This is the mode Claude Code and
claude.ai custom connectors support out of the box (RFC 8414 metadata +
DCR + PKCE).

## Operational constraints (read before touching)

- **Exactly one task.** All OAuth state (DCR clients, auth codes, access /
  refresh tokens, Google refresh tokens) lives in process memory. The service
  is pinned to `desiredCount: 1` with `minHealthyPercent: 0` so deploys never
  run two tasks concurrently. Do not add autoscaling or raise desiredCount.
- **Every deploy / crash invalidates all issued tokens.** Each user gets one
  browser re-auth prompt on next use. Accepted trade-off for internal use.
  EFS would not help: this mode never writes credentials to disk.
- **User restriction is enforced by Google, not this server.** The OAuth
  consent screen must be set to **Internal** so only workspace-domain
  accounts can authorize. The server has no email/domain allowlist.
- `/attachments` (Gmail/Drive attachment downloads) is disabled by stateless
  mode.

## Prerequisites

1. **Google Cloud** (existing internal project is fine):
   - Enable the APIs for the tools you use (Gmail, Calendar, Drive, Docs,
     Sheets, Slides, Forms, Tasks, People, Chat, Apps Script, Custom Search).
   - OAuth consent screen: **User type = Internal**.
   - OAuth client (type *Web application*) with authorized redirect URI
     `https://<domainName>/oauth2callback`.
2. **Secrets Manager** — create the secret before the first deploy
   (values shown are placeholders):

   ```sh
   aws secretsmanager create-secret \
     --name gws-mcp/oauth \
     --secret-string "{\"clientId\":\"<client-id>\",\"clientSecret\":\"<client-secret>\",\"jwtSigningKey\":\"$(openssl rand -hex 32)\"}"
   ```

3. Docker running locally (the image is built from the repo-root `Dockerfile`
   for `linux/arm64` and pushed by `cdk deploy`).
4. `cdk bootstrap` once per account/region.

## Deploy

```sh
cd infra
pnpm install
pnpm run build            # typecheck

# Zone hosted in this account (cert + A-alias created automatically):
npx cdk diff \
  -c domainName=gws-mcp.example.com \
  -c hostedZoneId=Z0000000000000 \
  -c hostedZoneName=example.com
npx cdk deploy -c domainName=... -c hostedZoneId=... -c hostedZoneName=...

# Zone elsewhere: bring a pre-validated ACM cert, then create the CNAME
# from the LoadBalancerDnsName output manually:
npx cdk deploy \
  -c domainName=gws-mcp.example.com \
  -c certificateArn=arn:aws:acm:<region>:<account>:certificate/<id>
```

Context keys can also be set in `cdk.json` under `context` to avoid
repeating `-c` flags. Optional keys: `oauthSecretName` (default
`gws-mcp/oauth`), `allowedClientRedirectUris` (CSV; defaults cover
claude.ai / claude.com callbacks and loopback for Claude Code).

## Verify

```sh
curl -sf https://<domainName>/health
curl -sf https://<domainName>/.well-known/oauth-authorization-server   # has registration_endpoint
```

- MCP Inspector: `npx @modelcontextprotocol/inspector` → Streamable HTTP →
  `https://<domainName>/mcp` → complete the OAuth flow.
- Claude Code:
  `claude mcp add --transport http google-workspace https://<domainName>/mcp`
  then `/mcp` → authenticate.
- claude.ai: Settings → Connectors → add custom connector with the `/mcp`
  URL. If its callback URL is rejected, the task log shows the redirect-URI
  allowlist rejection — add the exact URL via `allowedClientRedirectUris`.
- Negative test: a Google account outside the workspace domain must be
  blocked by Google (internal consent screen).

## Cost (ap-northeast-1, rough)

Fargate arm64 0.25 vCPU/512 MB ~$9 + ALB ~$20 + public IPv4 ~$11 +
logs/misc ~$2 ≈ **$40–45/month**. No NAT gateway by design.

## Future HA path

Horizontal scaling requires externalizing the in-memory OAuth store
(upstream change), or switching machine-to-machine callers to the
`EXTERNAL_OAUTH21_PROVIDER` mode where clients bring their own Google token.
