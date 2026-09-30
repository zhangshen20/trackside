#!/usr/bin/env bash
# Build and deploy the Trackside MCP server to AWS (Lambda arm64 + API Gateway HTTP API).
#
#   deploy/deploy.sh                 build, upload, deploy the stack
#   HR_ENV=staging SNAPSHOT_FROM=2026-09-22 SNAPSHOT_TO=2026-09-30 deploy/deploy.sh
#                                    also rebuild the data snapshot from the racing archive
#                                    (HR_ENV names the archive buckets, hr-<HR_ENV>-*)
#
# Needs: cargo, cargo-zigbuild (pip install cargo-zigbuild ziglang), the aarch64 Rust target,
# the AWS CLI, zip. When TRACKSIDE_ROLE_ARN is set, every AWS call runs as that role.
set -euo pipefail

cd "$(dirname "$0")/.."
export AWS_REGION="${AWS_REGION:-ap-southeast-2}"
export AWS_DEFAULT_REGION="$AWS_REGION"
STACK="${STACK:-trackside-mcp}"

if [[ -n "${TRACKSIDE_ROLE_ARN:-}" && -z "${AWS_SESSION_TOKEN:-}" ]]; then
  read -r AK SK ST < <(aws sts assume-role --role-arn "$TRACKSIDE_ROLE_ARN" \
    --role-session-name trackside-deploy \
    --query 'Credentials.[AccessKeyId,SecretAccessKey,SessionToken]' --output text)
  export AWS_ACCESS_KEY_ID="$AK" AWS_SECRET_ACCESS_KEY="$SK" AWS_SESSION_TOKEN="$ST"
  # The snapshot tool must not assume the role again from the role's own credentials.
  unset TRACKSIDE_ROLE_ARN
fi
ACCOUNT="$(aws sts get-caller-identity --query Account --output text)"
BUCKET="${TRACKSIDE_BUCKET:-trackside-$ACCOUNT-$AWS_REGION}"

if ! aws s3api head-bucket --bucket "$BUCKET" 2>/dev/null; then
  echo "creating s3://$BUCKET"
  aws s3api create-bucket --bucket "$BUCKET" \
    --create-bucket-configuration LocationConstraint="$AWS_REGION" >/dev/null
  aws s3api put-public-access-block --bucket "$BUCKET" --public-access-block-configuration \
    BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
  aws s3api put-bucket-tagging --bucket "$BUCKET" --tagging 'TagSet=[{Key=Project,Value=trackside}]'
fi

if [[ -n "${SNAPSHOT_FROM:-}" ]]; then
  if [[ -z "${HR_ENV:-}${TRACKSIDE_RACING_BUCKET:-}" ]]; then
    echo "set HR_ENV (e.g. HR_ENV=staging) to rebuild the snapshot from the archive" >&2
    exit 1
  fi
  cargo run --release -q -p trackside-snapshot -- \
    --from "$SNAPSHOT_FROM" --to "${SNAPSHOT_TO:-$SNAPSHOT_FROM}" \
    --out target/snapshot.json.gz --upload "s3://$BUCKET/snapshots/latest.json.gz"
fi

TARGET=aarch64-unknown-linux-gnu
cargo zigbuild --release -p trackside-mcp --target "$TARGET.2.34"
mkdir -p target/lambda
cp "target/$TARGET/release/trackside-mcp" target/lambda/bootstrap
(cd target/lambda && rm -f trackside-mcp.zip && zip -q -9 trackside-mcp.zip bootstrap)
SHA="$(sha256sum target/lambda/trackside-mcp.zip | cut -c1-16)"
CODE_KEY="lambda/trackside-mcp-$SHA.zip"
aws s3 cp --quiet target/lambda/trackside-mcp.zip "s3://$BUCKET/$CODE_KEY"

aws cloudformation deploy --stack-name "$STACK" --template-file deploy/trackside.yaml \
  --capabilities CAPABILITY_NAMED_IAM --no-fail-on-empty-changeset \
  --tags Project=trackside \
  --parameter-overrides "ArtifactBucket=$BUCKET" "CodeKey=$CODE_KEY"

# A new snapshot with unchanged code needs fresh instances to pick it up.
FN="$(aws cloudformation describe-stacks --stack-name "$STACK" \
  --query "Stacks[0].Outputs[?OutputKey=='FunctionName'].OutputValue" --output text)"
aws lambda update-function-configuration --function-name "$FN" \
  --description "Trackside MCP server (deployed $(date -u +%FT%TZ))" >/dev/null

aws cloudformation describe-stacks --stack-name "$STACK" \
  --query "Stacks[0].Outputs[?OutputKey=='McpUrl'].OutputValue" --output text
