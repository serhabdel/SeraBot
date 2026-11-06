#!/usr/bin/env python3
"""SeraBot deploy CA broker (runs on the OVH VPS as user `deployca`).

Exchanges a GitHub Actions OIDC token for a short-lived SSH user
certificate, so no long-lived SSH key ever lives in GitHub.

POST /sign  {"token": <oidc jwt>, "pubkey": "ssh-ed25519 AAAA... comment"}
  -> 200 {"cert": "ssh-ed25519-cert-v01@openssh.com AAAA..."}
  The cert allows only principal `deploy`, is valid 10 minutes, and carries
  force-command="sudo /opt/serabot-deploy/deploy.sh" with all other
  extensions cleared (no pty, no forwarding, no user-rc).

The verified tag (from the JWT `ref` claim) and commit sha are written to
/opt/serabot-deploy/pending-tag and pending-sha; deploy.sh deploys exactly
that tag and refuses anything else.

GET /health -> 200 ok (unauthenticated, for smoke checks).

Only python3 stdlib + openssl are used. Bind is 0.0.0.0:8788; the JWT is the
only authentication. A simple per-IP rate limit applies.
"""

import base64
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DEPLOY_DIR = "/opt/serabot-deploy"
CA_KEY = os.path.join(DEPLOY_DIR, "ca_key")
PENDING_TAG = os.path.join(DEPLOY_DIR, "pending-tag")
PENDING_SHA = os.path.join(DEPLOY_DIR, "pending-sha")
JWKS_URL = "https://token.actions.githubusercontent.com/.well-known/jwks"
EXPECTED_ISS = "https://token.actions.githubusercontent.com"
EXPECTED_AUD = "serabot-deploy"
EXPECTED_REPO = "serhabdel/SeraBot"
EXPECTED_WORKFLOW = ".github/workflows/deploy-ovh.yml"
EXPECTED_ENV = "ovh-deploy"
MAX_BODY = 64 * 1024
RATE_LIMIT = 10  # requests per minute per source IP

_lock = threading.Lock()
_jwks_cache = {"fetched": 0.0, "keys": {}}
_rate = {}


def b64url_decode(data: str) -> bytes:
    return base64.urlsafe_b64decode(data + "=" * (-len(data) % 4))


def fetch_jwks() -> dict:
    now = time.time()
    with _lock:
        if now - _jwks_cache["fetched"] < 3600 and _jwks_cache["keys"]:
            return _jwks_cache["keys"]
    import urllib.request

    with urllib.request.urlopen(JWKS_URL, timeout=10) as resp:
        doc = json.load(resp)
    keys = {k["kid"]: k for k in doc["keys"]}
    with _lock:
        _jwks_cache["fetched"] = now
        _jwks_cache["keys"] = keys
    return keys


def verify_jwt(token: str) -> dict:
    header_b64, payload_b64, sig_b64 = token.split(".")
    header = json.loads(b64url_decode(header_b64))
    payload = json.loads(b64url_decode(payload_b64))
    if header.get("alg") != "RS256":
        raise ValueError("alg must be RS256")
    jwk = fetch_jwks().get(header.get("kid"))
    if not jwk or not jwk.get("x5c"):
        raise ValueError("unknown kid")
    with tempfile.TemporaryDirectory() as tmp:
        cert = os.path.join(tmp, "cert.pem")
        pub = os.path.join(tmp, "pub.pem")
        sig = os.path.join(tmp, "sig.bin")
        inp = os.path.join(tmp, "input")
        with open(cert, "w") as fh:
            fh.write(
                "-----BEGIN CERTIFICATE-----\n"
                + jwk["x5c"][0]
                + "\n-----END CERTIFICATE-----\n"
            )
        subprocess.run(
            ["openssl", "x509", "-in", cert, "-pubkey", "-noout"],
            check=True,
            stdout=open(pub, "w"),
        )
        with open(sig, "wb") as fh:
            fh.write(b64url_decode(sig_b64))
        with open(inp, "wb") as fh:
            fh.write(f"{header_b64}.{payload_b64}".encode())
        subprocess.run(
            ["openssl", "dgst", "-sha256", "-verify", pub, "-signature", sig, inp],
            check=True,
            stdout=subprocess.DEVNULL,
        )
    now = time.time()
    if payload.get("iss") != EXPECTED_ISS:
        raise ValueError("bad iss")
    if EXPECTED_AUD not in (payload.get("aud") or []):
        raise ValueError("bad aud")
    if int(payload.get("exp", 0)) < now:
        raise ValueError("expired")
    if payload.get("repository") != EXPECTED_REPO:
        raise ValueError("bad repository")
    # `workflow` is the workflow NAME ("deploy-ovh"); the file path lives in
    # `workflow_ref` as "<owner>/<repo>/<path>@<ref>". Checking `workflow`
    # against a path never matches, so pin the path out of `workflow_ref`
    # instead, which also proves the token came from this repository.
    workflow_ref = payload.get("workflow_ref", "")
    if not workflow_ref.startswith(f"{EXPECTED_REPO}/{EXPECTED_WORKFLOW}@"):
        raise ValueError("bad workflow_ref")
    if payload.get("environment") != EXPECTED_ENV:
        raise ValueError("bad environment")
    if payload.get("event_name") != "workflow_dispatch":
        raise ValueError("bad event")
    ref = payload.get("ref", "")
    if not ref.startswith("refs/tags/v"):
        raise ValueError("ref must be a v* tag")
    if not payload.get("sha"):
        raise ValueError("missing sha")
    return payload


def sign_pubkey(pubkey: str, payload: dict) -> str:
    if not pubkey.startswith("ssh-ed25519 ") or "\n" in pubkey or len(pubkey) > 512:
        raise ValueError("pubkey must be a single-line ssh-ed25519 key")
    tag = payload["ref"][len("refs/tags/"):]
    sha = payload["sha"]
    with tempfile.TemporaryDirectory() as tmp:
        keyfile = os.path.join(tmp, "key.pub")
        with open(keyfile, "w") as fh:
            fh.write(pubkey + "\n")
        subprocess.run(
            [
                "ssh-keygen", "-s", CA_KEY,
                "-I", f"deploy-{sha[:12]}-{tag}",
                "-n", "deploy",
                "-V", "+10m",
                "-O", "clear",
                "-O", "force-command=sudo /opt/serabot-deploy/deploy.sh",
                keyfile,
            ],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        # ssh-keygen strips the ".pub" suffix before appending "-cert.pub", so
        # signing key.pub writes key-cert.pub (not key.pub-cert.pub).
        with open(os.path.join(tmp, "key-cert.pub")) as fh:
            cert = fh.read().strip()
    with _lock:
        with open(PENDING_TAG, "w") as fh:
            fh.write(tag)
        with open(PENDING_SHA, "w") as fh:
            fh.write(sha)
    return cert


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # quiet
        pass

    def _rate_limit(self) -> bool:
        ip = self.client_address[0]
        now = time.time()
        with _lock:
            hits = [t for t in _rate.get(ip, []) if now - t < 60]
            if len(hits) >= RATE_LIMIT:
                return False
            hits.append(now)
            _rate[ip] = hits
        return True

    def do_GET(self):
        if self.path == "/health":
            body = b"ok\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if self.path != "/sign" or not self._rate_limit():
            self.send_response(404 if self.path != "/sign" else 429)
            self.end_headers()
            return
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            self.send_response(413)
            self.end_headers()
            return
        try:
            req = json.loads(self.rfile.read(length))
            payload = verify_jwt(req["token"])
            cert = sign_pubkey(req["pubkey"], payload)
            body = json.dumps({"cert": cert}).encode()
            self.send_response(200)
        except Exception as error:
            # Log why to the journal; the client only ever sees "rejected" so a
            # probe cannot learn which check failed.
            print(
                f"sign rejected: {type(error).__name__}: {error}",
                file=sys.stderr,
                flush=True,
            )
            body = json.dumps({"error": "rejected"}).encode()
            self.send_response(403)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    os.chdir(DEPLOY_DIR)
    server = ThreadingHTTPServer(("0.0.0.0", 8788), Handler)
    server.serve_forever()
