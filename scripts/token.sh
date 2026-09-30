#!/usr/bin/env bash
# Print a Cognito access token for the deployed Trackside stack (client_credentials).
#
#   MCP_TOKEN=$(scripts/token.sh) scripts/smoke.sh          # both scopes: discovery and tools
#   CLIENT=ServiceClientId scripts/token.sh                  # discovery only, as Alexa+'s service tier
#
# Needs the AWS CLI with access to the stack; set TRACKSIDE_ROLE_ARN as for deploy/deploy.sh.
set -euo pipefail
export AWS_REGION="${AWS_REGION:-ap-southeast-2}"
STACK="${STACK:-trackside-mcp}"
CLIENT="${CLIENT:-SmokeClientId}"

if [[ -n "${TRACKSIDE_ROLE_ARN:-}" && -z "${AWS_SESSION_TOKEN:-}" ]]; then
  read -r AK SK ST < <(aws sts assume-role --role-arn "$TRACKSIDE_ROLE_ARN" \
    --role-session-name trackside-token \
    --query 'Credentials.[AccessKeyId,SecretAccessKey,SessionToken]' --output text)
  export AWS_ACCESS_KEY_ID="$AK" AWS_SECRET_ACCESS_KEY="$SK" AWS_SESSION_TOKEN="$ST"
fi

output() {
  aws cloudformation describe-stacks --stack-name "$STACK" \
    --query "Stacks[0].Outputs[?OutputKey=='$1'].OutputValue" --output text
}
POOL="$(output UserPoolId)"
DOMAIN="$(output OAuthDomain)"
ID="$(output "$CLIENT")"
SECRET="$(aws cognito-idp describe-user-pool-client --user-pool-id "$POOL" --client-id "$ID" \
  --query UserPoolClient.ClientSecret --output text)"
curl -sS --fail -u "$ID:$SECRET" -d grant_type=client_credentials "$DOMAIN/oauth2/token" |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["access_token"])'
