#!/usr/bin/env python3
"""AWS odds and ends for the demo video, all through TRACKSIDE_ROLE_ARN when it is set.

    awsx.py run CMD...                       run a command with the role's temporary credentials
    awsx.py fetch-snapshot OUT.json.gz       download the live data snapshot from Trackside's bucket
    awsx.py memory forget                    empty the demo listener's memory
    awsx.py memory last-checked YYYY-MM-DD   backdate the day the demo listener last heard their stable

The demo listener is the key the MCP server uses when it runs without auth ("local"), in the
table TRACKSIDE_MEMORY_TABLE (default trackside-listeners). Override with VIDEO_LISTENER.
"""
import os
import sys

import boto3

REGION = os.environ.get("AWS_REGION", "ap-southeast-2")


def session():
    role = os.environ.get("TRACKSIDE_ROLE_ARN")
    if not role:
        return boto3.Session(region_name=REGION)
    c = boto3.client("sts").assume_role(RoleArn=role, RoleSessionName="trackside-video")["Credentials"]
    return boto3.Session(
        aws_access_key_id=c["AccessKeyId"],
        aws_secret_access_key=c["SecretAccessKey"],
        aws_session_token=c["SessionToken"],
        region_name=REGION,
    )


def run(cmd):
    env = dict(os.environ)
    creds = session().get_credentials()
    if creds:
        frozen = creds.get_frozen_credentials()
        env.update(AWS_ACCESS_KEY_ID=frozen.access_key, AWS_SECRET_ACCESS_KEY=frozen.secret_key)
        if frozen.token:
            env["AWS_SESSION_TOKEN"] = frozen.token
    env.pop("AWS_PROFILE", None)
    os.execvpe(cmd[0], cmd, env)


def fetch_snapshot(out):
    s = session()
    account = s.client("sts").get_caller_identity()["Account"]
    bucket = os.environ.get("TRACKSIDE_BUCKET", f"trackside-{account}-{REGION}")
    s.client("s3").download_file(bucket, "snapshots/latest.json.gz", out)
    print(f"snapshot from s3://{bucket}/snapshots/latest.json.gz -> {out}")


def memory(args):
    table = session().resource("dynamodb").Table(os.environ.get("TRACKSIDE_MEMORY_TABLE", "trackside-listeners"))
    user = os.environ.get("VIDEO_LISTENER", "local")
    if args[0] == "forget":
        table.delete_item(Key={"user": user})
        print(f"forgot {user}")
    elif args[0] == "last-checked":
        item = table.get_item(Key={"user": user}).get("Item") or {"user": user, "horses": []}
        item["last_checked"] = args[1]
        table.put_item(Item=item)
        print(f"{user}: last checked {args[1]}, following {list(item.get('horses', []))}")
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    a = sys.argv[1:]
    if not a:
        raise SystemExit(__doc__)
    if a[0] == "run":
        run(a[1:])
    elif a[0] == "fetch-snapshot":
        fetch_snapshot(a[1])
    elif a[0] == "memory":
        memory(a[1:])
    else:
        raise SystemExit(__doc__)
