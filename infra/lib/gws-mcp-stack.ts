import * as path from "node:path";
import * as cdk from "aws-cdk-lib";
import * as acm from "aws-cdk-lib/aws-certificatemanager";
import * as ec2 from "aws-cdk-lib/aws-ec2";
import * as ecrAssets from "aws-cdk-lib/aws-ecr-assets";
import * as ecs from "aws-cdk-lib/aws-ecs";
import * as ecsPatterns from "aws-cdk-lib/aws-ecs-patterns";
import * as elbv2 from "aws-cdk-lib/aws-elasticloadbalancingv2";
import * as logs from "aws-cdk-lib/aws-logs";
import * as route53 from "aws-cdk-lib/aws-route53";
import * as secretsmanager from "aws-cdk-lib/aws-secretsmanager";
import type { Construct } from "constructs";

export interface GwsMcpStackProps extends cdk.StackProps {
  /** Public FQDN the server is reached at, e.g. "gws-mcp.example.com". */
  readonly domainName: string;
  /** Route53 hosted zone (id + name) to create the ACM cert and A-alias in. */
  readonly hostedZoneId?: string;
  readonly hostedZoneName?: string;
  /** Pre-provisioned ACM cert ARN; required when the zone is not in this account. */
  readonly certificateArn?: string;
  /** Secrets Manager secret holding clientId / clientSecret / jwtSigningKey. */
  readonly oauthSecretName?: string;
  /** CSV allowlist for MCP client OAuth callback URLs. */
  readonly allowedClientRedirectUris?: string;
}

const DEFAULT_OAUTH_SECRET_NAME = "gws-mcp/oauth";

// Callback URLs used by claude.ai custom connectors and Claude Code (loopback).
const DEFAULT_ALLOWED_CLIENT_REDIRECT_URIS = [
  "https://claude.ai/api/mcp/auth_callback",
  "https://claude.com/api/mcp/auth_callback",
  "http://localhost:*/callback",
  "http://127.0.0.1:*/callback",
].join(",");

export class GwsMcpStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: GwsMcpStackProps) {
    super(scope, id, props);

    const zone =
      props.hostedZoneId && props.hostedZoneName
        ? route53.HostedZone.fromHostedZoneAttributes(this, "Zone", {
            hostedZoneId: props.hostedZoneId,
            zoneName: props.hostedZoneName,
          })
        : undefined;

    const certificate = props.certificateArn
      ? acm.Certificate.fromCertificateArn(this, "Cert", props.certificateArn)
      : zone
        ? new acm.Certificate(this, "Cert", {
            domainName: props.domainName,
            validation: acm.CertificateValidation.fromDns(zone),
          })
        : undefined;
    if (!certificate) {
      throw new Error(
        "HTTPS is mandatory (WORKSPACE_EXTERNAL_URL must be https). " +
          "Provide either hostedZoneId+hostedZoneName or certificateArn.",
      );
    }

    // Public subnets only, no NAT: the task gets a public IP for egress to
    // Google APIs; its security group only admits traffic from the ALB.
    const vpc = new ec2.Vpc(this, "Vpc", {
      maxAzs: 2,
      natGateways: 0,
      subnetConfiguration: [
        { name: "public", subnetType: ec2.SubnetType.PUBLIC },
      ],
    });

    const image = new ecrAssets.DockerImageAsset(this, "Image", {
      directory: path.join(__dirname, "..", ".."),
      file: "Dockerfile",
      platform: ecrAssets.Platform.LINUX_ARM64,
    });

    const oauthSecret = secretsmanager.Secret.fromSecretNameV2(
      this,
      "OauthSecret",
      props.oauthSecretName ?? DEFAULT_OAUTH_SECRET_NAME,
    );

    const logGroup = new logs.LogGroup(this, "Logs", {
      retention: logs.RetentionDays.ONE_MONTH,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
    });

    const externalUrl = `https://${props.domainName}`;

    // InternalOAuth21 keeps all OAuth state (DCR clients, codes, tokens) in
    // process memory, so exactly one task may run at a time: desiredCount 1
    // and minHealthyPercent 0 so a rolling deploy never overlaps two tasks.
    const service = new ecsPatterns.ApplicationLoadBalancedFargateService(
      this,
      "Service",
      {
        vpc,
        cpu: 256,
        memoryLimitMiB: 512,
        desiredCount: 1,
        minHealthyPercent: 0,
        maxHealthyPercent: 100,
        assignPublicIp: true,
        taskSubnets: { subnetType: ec2.SubnetType.PUBLIC },
        runtimePlatform: {
          cpuArchitecture: ecs.CpuArchitecture.ARM64,
          operatingSystemFamily: ecs.OperatingSystemFamily.LINUX,
        },
        circuitBreaker: { rollback: true },
        protocol: elbv2.ApplicationProtocol.HTTPS,
        certificate,
        redirectHTTP: true,
        domainName: zone ? props.domainName : undefined,
        domainZone: zone,
        // Keep long-lived SSE GET streams on /mcp alive.
        idleTimeout: cdk.Duration.seconds(3600),
        taskImageOptions: {
          image: ecs.ContainerImage.fromDockerImageAsset(image),
          containerPort: 8000,
          environment: {
            MCP_ENABLE_OAUTH21: "true",
            WORKSPACE_MCP_STATELESS_MODE: "true",
            WORKSPACE_EXTERNAL_URL: externalUrl,
            WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS:
              props.allowedClientRedirectUris ??
              DEFAULT_ALLOWED_CLIENT_REDIRECT_URIS,
            // Default DCR client TTL is 24h, which would force daily re-auth;
            // align it with the 30-day refresh-token TTL instead.
            WORKSPACE_MCP_REGISTERED_CLIENT_TTL_SECONDS: "2592000",
          },
          secrets: {
            GOOGLE_OAUTH_CLIENT_ID: ecs.Secret.fromSecretsManager(
              oauthSecret,
              "clientId",
            ),
            GOOGLE_OAUTH_CLIENT_SECRET: ecs.Secret.fromSecretsManager(
              oauthSecret,
              "clientSecret",
            ),
            FASTMCP_SERVER_AUTH_GOOGLE_JWT_SIGNING_KEY:
              ecs.Secret.fromSecretsManager(oauthSecret, "jwtSigningKey"),
          },
          logDriver: ecs.LogDrivers.awsLogs({
            streamPrefix: "gws-mcp",
            logGroup,
          }),
        },
      },
    );

    service.targetGroup.configureHealthCheck({
      path: "/health",
      healthyHttpCodes: "200",
    });

    new cdk.CfnOutput(this, "ExternalUrl", { value: externalUrl });
    new cdk.CfnOutput(this, "McpEndpoint", { value: `${externalUrl}/mcp` });
    new cdk.CfnOutput(this, "LoadBalancerDnsName", {
      value: service.loadBalancer.loadBalancerDnsName,
      description: zone
        ? "A-alias record was created automatically."
        : `Create a CNAME: ${props.domainName} -> this value.`,
    });
  }
}
