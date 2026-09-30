#!/usr/bin/env bash
# Build and deploy the Trackside MCP server to AWS (Lambda arm64 + API Gateway HTTP API), with
# the web simulator of the Alexa+ experience under /sim.
#
#   deploy/deploy.sh                 build, upload, deploy the stack
#   HR_ENV=staging SNAPSHOT_FROM=2026-09-22 SNAPSHOT_TO=2026-09-30 deploy/deploy.sh
#                                    also rebuild the data snapshot from the racing archive
#                                    (HR_ENV names the archive buckets, hr-<HR_ENV>-*)
#   TRACKSIDE_DOMAIN=mcp.racingaidataset.com.au deploy/deploy.sh
#                                    also serve the API on that hostname (see "Custom domain"
#                                    below); set TRACKSIDE_HOSTED_ZONE_ID when its DNS is in
#                                    Route 53 in this account. Later deploys keep the domain.
#   TRACKSIDE_SIM_TODAY=2026-09-27 deploy/deploy.sh
#                                    the date the simulator treats as today (for a demo on a
#                                    snapshot of past racing); SIM_MODEL picks its Bedrock model
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
cargo zigbuild --release -p trackside-mcp -p trackside-sim --target "$TARGET.2.34"
# Each Lambda gets a zip holding its binary as `bootstrap`, uploaded under its content hash.
upload_lambda() {
  local name="$1" dir="target/lambda/$1"
  rm -rf "$dir" && mkdir -p "$dir"
  cp "target/$TARGET/release/$name" "$dir/bootstrap"
  (cd "$dir" && zip -q -9 "$name.zip" bootstrap)
  local key="lambda/$name-$(sha256sum "$dir/$name.zip" | cut -c1-16).zip"
  aws s3 cp --quiet "$dir/$name.zip" "s3://$BUCKET/$key"
  echo "$key"
}
CODE_KEY="$(upload_lambda trackside-mcp)"
SIM_CODE_KEY="$(upload_lambda trackside-sim)"

# Custom domain: a DNS-validated ACM certificate, requested once and reused. With a Route 53
# zone the validation record is written here; otherwise it is printed for the DNS provider,
# and the stack goes ahead without the domain until the certificate is issued.
DOMAIN_PARAMS=() DOMAIN_ON=""
if [[ -n "${TRACKSIDE_DOMAIN:-}" ]]; then
  CERT="$(aws acm list-certificates --certificate-statuses ISSUED PENDING_VALIDATION \
    --query "CertificateSummaryList[?DomainName=='$TRACKSIDE_DOMAIN'].CertificateArn | [0]" --output text)"
  if [[ -z "$CERT" || "$CERT" == None ]]; then
    CERT="$(aws acm request-certificate --domain-name "$TRACKSIDE_DOMAIN" --validation-method DNS \
      --idempotency-token trackside --tags Key=Project,Value=trackside \
      --query CertificateArn --output text)"
    echo "requested certificate $CERT"
  fi
  RECORD=""
  for _ in $(seq 10); do  # the validation record appears a few seconds after the request
    RECORD="$(aws acm describe-certificate --certificate-arn "$CERT" \
      --query 'Certificate.DomainValidationOptions[0].ResourceRecord.[Name,Value]' --output text)"
    [[ -n "$RECORD" && "$RECORD" != None ]] && break
    sleep 3
  done
  read -r VNAME VVALUE <<<"$RECORD"
  if [[ -n "${TRACKSIDE_HOSTED_ZONE_ID:-}" ]]; then
    aws route53 change-resource-record-sets --hosted-zone-id "$TRACKSIDE_HOSTED_ZONE_ID" \
      --change-batch "{\"Changes\":[{\"Action\":\"UPSERT\",\"ResourceRecordSet\":{\"Name\":\"$VNAME\",\"Type\":\"CNAME\",\"TTL\":300,\"ResourceRecords\":[{\"Value\":\"$VVALUE\"}]}}]}" >/dev/null
  fi
  CERT_STATUS=""
  for _ in $(seq 20); do
    CERT_STATUS="$(aws acm describe-certificate --certificate-arn "$CERT" --query Certificate.Status --output text)"
    [[ "$CERT_STATUS" == ISSUED ]] && break
    sleep 15
  done
  if [[ "$CERT_STATUS" == ISSUED ]]; then
    DOMAIN_ON=1
    DOMAIN_PARAMS=("DomainName=$TRACKSIDE_DOMAIN" "CertificateArn=$CERT" "HostedZoneId=${TRACKSIDE_HOSTED_ZONE_ID:-}")
  else
    echo "certificate for $TRACKSIDE_DOMAIN is $CERT_STATUS; add this DNS record, then run deploy.sh again:" >&2
    echo "  CNAME  $VNAME  ->  $VVALUE" >&2
  fi
fi

aws cloudformation deploy --stack-name "$STACK" --template-file deploy/trackside.yaml \
  --capabilities CAPABILITY_NAMED_IAM --no-fail-on-empty-changeset \
  --tags Project=trackside \
  --parameter-overrides "ArtifactBucket=$BUCKET" "CodeKey=$CODE_KEY" "SimCodeKey=$SIM_CODE_KEY" \
    ${TRACKSIDE_SIM_TODAY+"SimToday=$TRACKSIDE_SIM_TODAY"} ${SIM_MODEL:+"SimModel=$SIM_MODEL"} \
    ${DOMAIN_PARAMS[@]+"${DOMAIN_PARAMS[@]}"}

# A new snapshot with unchanged code needs fresh instances to pick it up.
FN="$(aws cloudformation describe-stacks --stack-name "$STACK" \
  --query "Stacks[0].Outputs[?OutputKey=='FunctionName'].OutputValue" --output text)"
aws lambda update-function-configuration --function-name "$FN" \
  --description "Trackside MCP server (deployed $(date -u +%FT%TZ))" >/dev/null

aws cloudformation describe-stacks --stack-name "$STACK" \
  --query "Stacks[0].Outputs[?OutputKey=='McpUrl' || OutputKey=='SimUrl'].OutputValue" --output text
if [[ -n "$DOMAIN_ON" && -z "${TRACKSIDE_HOSTED_ZONE_ID:-}" ]]; then
  HOSTNAME_TARGET="$(aws cloudformation describe-stacks --stack-name "$STACK" \
    --query "Stacks[0].Outputs[?OutputKey=='RegionalHostname'].OutputValue" --output text)"
  echo "at your DNS provider:  CNAME  $TRACKSIDE_DOMAIN  ->  $HOSTNAME_TARGET"
fi
