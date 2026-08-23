import * as cdk from "aws-cdk-lib";
import { GwsMcpStack } from "../lib/gws-mcp-stack";

const app = new cdk.App();

const domainName: unknown = app.node.tryGetContext("domainName");
if (typeof domainName !== "string" || domainName.length === 0) {
  throw new Error(
    "Missing context 'domainName'. " +
      "Pass -c domainName=gws-mcp.example.com (or set it in cdk.json).",
  );
}

const optionalContext = (key: string): string | undefined => {
  const value: unknown = app.node.tryGetContext(key);
  return typeof value === "string" && value.length > 0 ? value : undefined;
};

new GwsMcpStack(app, "GwsMcpStack", {
  env: {
    account: process.env.CDK_DEFAULT_ACCOUNT,
    region: process.env.CDK_DEFAULT_REGION,
  },
  domainName,
  hostedZoneId: optionalContext("hostedZoneId"),
  hostedZoneName: optionalContext("hostedZoneName"),
  certificateArn: optionalContext("certificateArn"),
  oauthSecretName: optionalContext("oauthSecretName"),
  allowedClientRedirectUris: optionalContext("allowedClientRedirectUris"),
});
