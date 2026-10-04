#!/usr/bin/env python3
"""Keep docs/aws.md and deploy/trackside.yaml in step.

Reads every `Type: AWS::<Service>::<Thing>` line in the CloudFormation template (a plain text
parse; no YAML module needed) and dedupes them by service family (`AWS::<Service>`). Then:

1. every family the template creates must be named somewhere in docs/aws.md, under any of
   its everyday names (ApiGatewayV2 as "API Gateway", Events as "EventBridge", Logs as
   "CloudWatch Logs", CertificateManager as "ACM" or "Certificate Manager", and so on);
2. every service named in the first column of the doc's service table must be one the
   template creates, unless it is on the exception list below of services the stack uses
   without a template resource (an SDK call from the code, a CLI call from deploy.sh, or
   CloudFormation itself). An excepted row must say so in the words the table requires.

Exits 1 and names each problem; exits 0 when both directions hold. Standard library only.

    python3 scripts/check-aws-doc.py                          # deploy/trackside.yaml, docs/aws.md
    python3 scripts/check-aws-doc.py path/to/template.yaml path/to/aws.md
"""

import re
import sys
from pathlib import Path

# Family -> patterns (case-insensitive) that count as naming it in prose. Order matters for the
# doc-to-template direction: the first family whose pattern matches a table cell wins, so the
# longer names ("CloudWatch Logs", "EventBridge Scheduler") come before their prefixes.
ALIASES = [
    ("Logs", [r"\bCloudWatch Logs\b", r"\bLogs::"]),
    ("CloudWatch", [r"\bCloudWatch\b(?! Logs)"]),
    ("Scheduler", [r"\bEventBridge Scheduler\b", r"\bScheduler::"]),
    ("Events", [r"\bEventBridge\b(?! Scheduler)", r"\bEvents::"]),
    ("ApiGatewayV2", [r"\bAPI Gateway\b", r"\bApiGatewayV2\b"]),
    ("ApiGateway", [r"\bAPI Gateway\b", r"\bApiGateway\b"]),
    ("CertificateManager", [r"\bACM\b", r"\bCertificate Manager\b", r"\bCertificateManager\b"]),
    ("Route53", [r"\bRoute ?53\b"]),
    ("Lambda", [r"\bLambda\b"]),
    ("Cognito", [r"\bCognito\b"]),
    ("DynamoDB", [r"\bDynamoDB\b"]),
    ("S3", [r"\bS3\b"]),
    ("IAM", [r"\bIAM\b"]),
    ("SNS", [r"\bSNS\b"]),
    ("SQS", [r"\bSQS\b"]),
    ("KMS", [r"\bKMS\b"]),
    ("SecretsManager", [r"\bSecrets Manager\b", r"\bSecretsManager\b"]),
    ("SSM", [r"\bSystems Manager\b", r"\bSSM\b", r"\bParameter Store\b"]),
    ("StepFunctions", [r"\bStep Functions\b", r"\bStepFunctions\b"]),
    ("Bedrock", [r"\bBedrock\b"]),
    ("Polly", [r"\bPolly\b"]),
    ("CloudFormation", [r"\bCloudFormation\b"]),
    ("CloudFront", [r"\bCloudFront\b"]),
    ("ECR", [r"\bECR\b"]),
    ("ECS", [r"\bECS\b"]),
    ("EC2", [r"\bEC2\b", r"\bVPC\b"]),
]

# Services the stack uses with no `Type:` line in the template. The doc's row for each must
# contain the marker text, so the reader is told where the thing comes from.
NO_TEMPLATE_RESOURCE = {
    "Bedrock": ("called from the code with the AWS SDK", "SDK call, no resource"),
    "Polly": ("called from the code with the AWS SDK", "SDK call, no resource"),
    "S3": ("the bucket is created by deploy/deploy.sh with the CLI", "No template resource"),
    "CertificateManager": (
        "the certificate is requested by deploy/deploy.sh with the CLI",
        "No template resource",
    ),
    "CloudFormation": ("the stack itself", "The stack itself"),
}

TYPE_LINE = re.compile(r"^\s*-?\s*Type:\s*['\"]?AWS::([A-Za-z0-9]+)::([A-Za-z0-9]+)")
TABLE_ROW = re.compile(r"^\s*\|(.+)\|\s*$")


def template_families(text):
    """{family: sorted list of 'AWS::Family::Thing' types} for every Type line."""
    families = {}
    for line in text.splitlines():
        m = TYPE_LINE.match(line)
        if m:
            families.setdefault(m.group(1), set()).add(f"AWS::{m.group(1)}::{m.group(2)}")
    return {family: sorted(types) for family, types in families.items()}


def mentions(text, family):
    patterns = dict(ALIASES).get(family, [rf"\b{re.escape(family)}\b"])
    return any(re.search(p, text, re.I) for p in patterns)


def family_of(cell):
    """The service family a table cell names, by the first alias that matches, or None."""
    plain = re.sub(r"[`*_]", "", cell).strip()
    for family, patterns in ALIASES:
        if any(re.search(p, plain, re.I) for p in patterns):
            return family
    return None


def service_table_rows(text):
    """(line number, first cell, whole row) for each body row of the first table whose header
    starts with a 'Service' column."""
    rows = []
    in_table = False
    for lineno, line in enumerate(text.splitlines(), 1):
        m = TABLE_ROW.match(line)
        if not m:
            if in_table:
                break
            continue
        cells = [c.strip() for c in m.group(1).split("|")]
        if not in_table:
            if cells and re.fullmatch(r"\**service\**", cells[0], re.I):
                in_table = True
            continue
        if re.fullmatch(r":?-{3,}:?", cells[0]):
            continue
        rows.append((lineno, cells[0], line))
    return rows


def check(template_text, doc_text):
    errors = []
    families = template_families(template_text)
    if not families:
        errors.append("no 'Type: AWS::...' lines found in the template")

    # 1. Template -> doc.
    for family in sorted(families):
        if not mentions(doc_text, family):
            errors.append(
                f"the template creates {', '.join(families[family])} "
                f"but docs/aws.md never names {family}"
            )

    # 2. Doc's service table -> template.
    rows = service_table_rows(doc_text)
    if not rows:
        errors.append("docs/aws.md has no table whose first column is 'Service'")
    seen = set()
    for lineno, cell, row in rows:
        family = family_of(cell)
        if family is None:
            errors.append(f"docs/aws.md:{lineno}: unknown service in the table: {cell!r}")
            continue
        if family in seen:
            errors.append(f"docs/aws.md:{lineno}: {family} appears twice in the table")
        seen.add(family)
        if family in families:
            continue
        if family in NO_TEMPLATE_RESOURCE:
            why, marker = NO_TEMPLATE_RESOURCE[family]
            if marker.lower() not in row.lower():
                errors.append(
                    f"docs/aws.md:{lineno}: {family} is {why}; its row must say {marker!r}"
                )
            continue
        errors.append(
            f"docs/aws.md:{lineno}: the table names {family} but the template creates no "
            f"AWS::{family} resource (add it to NO_TEMPLATE_RESOURCE if the code or "
            f"deploy.sh uses it)"
        )
    return families, rows, errors


def main(argv):
    root = Path(__file__).resolve().parent.parent
    template = Path(argv[1]) if len(argv) > 1 else root / "deploy" / "trackside.yaml"
    doc = Path(argv[2]) if len(argv) > 2 else root / "docs" / "aws.md"
    try:
        template_text = template.read_text(encoding="utf-8")
        doc_text = doc.read_text(encoding="utf-8")
    except OSError as e:
        print(f"check-aws-doc: cannot read: {e}", file=sys.stderr)
        return 1
    families, rows, errors = check(template_text, doc_text)
    for err in errors:
        print(f"check-aws-doc: {err}", file=sys.stderr)
    if errors:
        print(f"check-aws-doc: {len(errors)} problem(s)", file=sys.stderr)
        return 1
    print(
        f"check-aws-doc: {len(families)} service families in {template.name} "
        f"({', '.join(sorted(families))}), {len(rows)} rows in {doc.name}: OK"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
