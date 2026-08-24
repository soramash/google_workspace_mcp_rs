import * as cdk from "aws-cdk-lib";
import { GwsMcpStack } from "../lib/gws-mcp-stack";

const app = new cdk.App();

const domainNameContext: unknown = app.node.tryGetContext("domainName");
if (typeof domainNameContext !== "string" || domainNameContext.length === 0) {
  throw new Error(
    "Missing context 'domainName'. " +
      "Pass -c domainName=gws-mcp.example.com (or set it in cdk.json).",
  );
}

const domainName = domainNameContext.trim();
const hostnamePattern =
  /^(?=.{1,253}$)(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)*[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/;
const ipv4AddressPattern = /^(?:\d{1,3}\.){3}\d{1,3}$/;
if (!hostnamePattern.test(domainName) || ipv4AddressPattern.test(domainName)) {
  throw new Error(
    "Invalid context 'domainName'. " +
      "Pass a hostname such as gws-mcp.example.com without a scheme, port, or path.",
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
