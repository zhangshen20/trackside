#!/usr/bin/env python3
"""Sign in to the deployed Trackside as a real user and call two tools.

    scripts/signin_check.py

Does the whole Alexa+-style user flow except typing your password: reads the sign-in
client from the stack, starts a one-shot callback server on http://localhost:6274 (stop MCP
Inspector first, it uses the same port), opens Cognito's login page in your browser with a
PKCE S256 challenge and the scopes the server advertises, exchanges the code for a token,
then calls follow_horse and my_stable with it. Sign up on that page if you have no user yet.

Needs python3 and the AWS CLI. Set AWS_PROFILE / TRACKSIDE_ROLE_ARN as for deploy/deploy.sh.
Prints no secrets or tokens.
"""

import base64
import hashlib
import http.server
import json
import os
import secrets
import subprocess
import sys
import urllib.parse
import urllib.request
import webbrowser

MCP_URL = os.environ.get("MCP_URL", "https://mcp.racingaidataset.com.au/mcp")
STACK = os.environ.get("STACK", "trackside-mcp")
REGION = os.environ.get("AWS_REGION", "ap-southeast-2")
REDIRECT = "http://localhost:6274/oauth/callback"
HORSE = os.environ.get("HORSE", "Extragalactic")
DATE = os.environ.get("DATE", "2026-09-27")


def aws(*args, env):
    out = subprocess.run(["aws", "--region", REGION, "--output", "json", *args],
                         check=True, capture_output=True, text=True, env=env)
    return json.loads(out.stdout)


def aws_env():
    env = dict(os.environ)
    role = env.get("TRACKSIDE_ROLE_ARN")
    if role and not env.get("AWS_SESSION_TOKEN"):
        c = aws("sts", "assume-role", "--role-arn", role, "--role-session-name", "trackside-signin",
                env=env)["Credentials"]
        env.update(AWS_ACCESS_KEY_ID=c["AccessKeyId"], AWS_SECRET_ACCESS_KEY=c["SecretAccessKey"],
                   AWS_SESSION_TOKEN=c["SessionToken"])
    return env


def get_json(url):
    with urllib.request.urlopen(url, timeout=20) as r:
        return json.load(r)


def wait_for_code(state):
    got = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
            got.update({k: v[0] for k, v in q.items()})
            ok = "code" in got and got.get("state") == state
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            msg = "Signed in. You can close this tab and go back to the terminal." if ok else \
                f"Sign-in failed: {got.get('error', '')} {got.get('error_description', '')}"
            self.wfile.write(f"<p style='font:18px sans-serif'>{msg}</p>".encode())

        def log_message(self, *a):
            pass

    try:
        server = http.server.HTTPServer(("localhost", 6274), Handler)
    except OSError:
        sys.exit("Port 6274 is busy: stop MCP Inspector (Ctrl+C in its terminal) and run this again.")
    while "code" not in got and "error" not in got:
        server.handle_request()
    if "error" in got:
        sys.exit(f"Cognito refused the sign-in: {got['error']} {got.get('error_description', '')}")
    if got.get("state") != state:
        sys.exit("State mismatch in the callback; not using that code.")
    return got["code"]


def mcp(token, method, params=None):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}}).encode()
    req = urllib.request.Request(MCP_URL, body, {
        "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": "2025-11-25", "Authorization": f"Bearer {token}"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def main():
    env = aws_env()
    outputs = {o["OutputKey"]: o["OutputValue"] for o in
               aws("cloudformation", "describe-stacks", "--stack-name", STACK, env=env)["Stacks"][0]["Outputs"]}
    client = aws("cognito-idp", "describe-user-pool-client", "--user-pool-id", outputs["UserPoolId"],
                 "--client-id", outputs["UserClientId"], env=env)["UserPoolClient"]

    # Discover everything from the server, as Alexa+ would.
    base = MCP_URL.rsplit("/mcp", 1)[0]
    prm = get_json(f"{base}/.well-known/oauth-protected-resource")
    asm = get_json(f"{prm['authorization_servers'][0]}/.well-known/oauth-authorization-server")
    scope = " ".join(["openid", *prm["scopes_supported"]])
    print(f"Metadata: scopes {prm['scopes_supported']}, PKCE {asm['code_challenge_methods_supported']}")

    verifier = secrets.token_urlsafe(64)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    state = secrets.token_urlsafe(16)
    url = asm["authorization_endpoint"] + "?" + urllib.parse.urlencode({
        "response_type": "code", "client_id": client["ClientId"], "redirect_uri": REDIRECT,
        "scope": scope, "state": state, "code_challenge": challenge, "code_challenge_method": "S256"})
    print("Opening the sign-in page in your browser. Sign in (or Sign up) there.")
    print(f"If no tab opens, paste this into Chrome:\n{url}\n")
    webbrowser.open(url)
    code = wait_for_code(state)

    basic = base64.b64encode(f"{client['ClientId']}:{client['ClientSecret']}".encode()).decode()
    req = urllib.request.Request(asm["token_endpoint"], urllib.parse.urlencode({
        "grant_type": "authorization_code", "code": code, "redirect_uri": REDIRECT,
        "code_verifier": verifier}).encode(), {
        "Authorization": f"Basic {basic}", "Content-Type": "application/x-www-form-urlencoded"})
    with urllib.request.urlopen(req, timeout=20) as r:
        token = json.load(r)["access_token"]
    claims = json.loads(base64.urlsafe_b64decode(token.split(".")[1] + "=="))
    print(f"Signed in. Token scopes: {claims.get('scope')}")

    status, listed = mcp(token, "tools/list")
    print(f"tools/list: HTTP {status}, {len(listed.get('result', {}).get('tools', []))} tools")
    for name, args in [("follow_horse", {"horse": HORSE}), ("my_stable", {"date": DATE})]:
        status, res = mcp(token, "tools/call", {"name": name, "arguments": args})
        text = " ".join(c.get("text", "") for c in res.get("result", {}).get("content", []))
        print(f"{name}: HTTP {status}\n  {text or res}")


if __name__ == "__main__":
    main()
