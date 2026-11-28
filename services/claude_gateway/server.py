"""Opt-in loopback-only claude-agent-sdk HTTP gateway for SeraBot.

Security invariants:
- Binds strictly to loopback addresses (127.0.0.1 / ::1); refuses any
  non-loopback host at startup.
- Fail-closed constant-time Bearer token auth against
  ``SERABOT_CLAUDE_GATEWAY_TOKEN``.
- Passes ``tools=[]`` and ``setting_sources=[]`` to ``ClaudeAgentOptions`` so
  no Claude Code built-in tools or local settings files are used.
- Captures requested ``mcp__serabot__<name>`` tool invocations and returns
  them to SeraBot's Rust ``gateway`` for policy, approval, and audit.
- Never logs secrets or raw prompt bodies.
"""

from __future__ import annotations

import argparse
import asyncio
from contextlib import asynccontextmanager
from datetime import datetime, timezone
import hmac
import http.client
import inspect
import ipaddress
import json
import logging
import math
import os
import re
import secrets
import select
import shlex
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any
import urllib.error
import urllib.parse
import urllib.request
import uuid

try:
    import pty
except ImportError:  # pragma: no cover - Windows
    pty = None  # type: ignore[assignment]

from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import JSONResponse
from starlette.routing import Route

logger = logging.getLogger("serabot.claude_gateway")

GATEWAY_VERSION = "0.74.35"
TOKEN_ENV_VAR = "SERABOT_CLAUDE_GATEWAY_TOKEN"
HOST_ENV_VAR = "SERABOT_CLAUDE_GATEWAY_HOST"
STATE_DIR_ENV_VAR = "SERABOT_CLAUDE_GATEWAY_STATE_DIR"
CLI_PATH_ENV_VAR = "SERABOT_CLAUDE_CLI_PATH"
DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 8791
DEFAULT_MODEL = "claude-sonnet-4-6"

AUTH_FLOW_TTL_SECS = 600
MAX_AUTH_FLOWS = 8
AUTH_START_TIMEOUT_SECS = 20.0
BROWSER_CAPTURE_GRACE_SECS = 2.0
MAX_AUTH_CODE_CHARS = 4096
AUTH_COMPLETE_TIMEOUT_SECS = 20.0
CLI_STATUS_TIMEOUT_SECS = 10.0

PROFILE_ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{0,47}$")
VALID_EFFORTS = frozenset({"low", "medium", "high", "max"})

_ANSI_RE = re.compile(
    r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])"
)
_CALLBACK_PATH_RE = re.compile(r"/[A-Za-z0-9/_-]{0,63}")
_HTTPS_URL_RE = re.compile(r"https://[^\s\"'<>\]\)]+")
_SECRET_REDACT_RE = re.compile(
    r"(?:sk-ant-[A-Za-z0-9_-]+|Bearer\s+\S+|eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)?)"
)

_STRIPPED_CLAUDE_ENV_VARS = frozenset(
    {
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ACCESS_TOKEN",
        "CLAUDE_CODE_SESSION_ATTENDED",
        "CLAUDE_CODE_SESSION_KIND",
        "CLAUDE_CODE_SESSION_NAME",
        "CLAUDE_CODE_SESSION_ORIGIN",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_CODE_BRIDGE_SESSION_ID",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_CHROME_MCP_ORG_DENIED",
        "CLAUDE_CODE_EVAL_INTERVIEW_SESSION",
        "CLAUDE_CODE_EXECPATH",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_EFFORT",
        "CLAUDE_PID",
    }
)

_PRESERVED_CLAUDE_HOST_VARS = frozenset(
    {
        "CLAUDE_CODE_HOST_AUTH_ENV_VAR",
        "CLAUDE_CODE_HOST_AUTH_REFRESH_TIMEOUT_MS",
        "CLAUDE_CODE_HOST_CREDS_FILE",
    }
)

# Pending CLI sign-in flows: flow_id -> flow state dict.
_pending_auth_flows: dict[str, dict[str, Any]] = {}

# Per-profile cached rate_limit_info from SDK turn messages/events.
_last_rate_limits: dict[str, dict[str, Any]] = {}

ANTHROPIC_OAUTH_USAGE_URL = "https://api.anthropic.com/api/oauth/usage"
USAGE_TIMEOUT_SECS = 15.0
USAGE_MAX_BODY_BYTES = 64 * 1024
_USAGE_WINDOW_SPECS: tuple[tuple[str, str], ...] = (
    ("five_hour", "5h"),
    ("seven_day", "7d"),
    ("seven_day_sonnet", "7d_sonnet"),
    ("seven_day_opus", "7d_opus"),
)

SUPPORTED_MODELS: list[dict[str, str]] = [
    {"id": "claude-sonnet-4-6"},
    {"id": "claude-opus-4-6"},
    {"id": "claude-sonnet-4-5"},
    {"id": "claude-opus-4-5"},
    {"id": "claude-haiku-4-5"},
]

MCP_SERVER_NAME = "serabot"
MCP_TOOL_PREFIX = f"mcp__{MCP_SERVER_NAME}__"


def validate_loopback_host(host: str) -> str:
    """Validate that ``host`` is a loopback IP (127.0.0.1 / ::1).

    Raises ``ValueError`` for any non-loopback IP or hostname.
    """
    cleaned = host.strip()
    if cleaned.startswith("[") and cleaned.endswith("]"):
        cleaned = cleaned[1:-1]
    if not cleaned:
        raise ValueError("Host must not be empty; only 127.0.0.1 / ::1 are allowed.")
    try:
        addr = ipaddress.ip_address(cleaned)
    except ValueError as exc:
        raise ValueError(
            f"Refusing non-loopback host {host!r}; only 127.0.0.1 / ::1 are allowed."
        ) from exc
    if not addr.is_loopback:
        raise ValueError(
            f"Refusing non-loopback host {host!r}; only 127.0.0.1 / ::1 are allowed."
        )
    return str(addr)


def _state_dir() -> Path:
    raw = os.environ.get(STATE_DIR_ENV_VAR, "").strip()
    if raw:
        return Path(raw).expanduser()
    return Path.home() / ".serabot" / "claude-gateway"


def _validate_profile_id(profile: str) -> str:
    if not isinstance(profile, str) or not PROFILE_ID_RE.match(profile):
        raise ValueError(
            "invalid profile id; must match ^[a-z0-9][a-z0-9_-]{0,47}$"
        )
    return profile


def _resolve_profile_dir(
    profile: str | None, *, create: bool = False
) -> tuple[str, Path | None]:
    if profile is None or profile == "" or profile == "default":
        return "default", None
    validated = _validate_profile_id(profile)
    profile_dir = _state_dir() / "profiles" / validated
    if create:
        profile_dir.parent.mkdir(parents=True, exist_ok=True)
        profile_dir.mkdir(parents=True, exist_ok=True)
        try:
            os.chmod(profile_dir, 0o700)
        except OSError:
            pass
    return validated, profile_dir


def _clean_claude_env(
    profile_dir: Path | None, strip_api_key: bool = False
) -> dict[str, str]:
    """Build a sanitized environment for spawning the Claude CLI / SDK."""
    env: dict[str, str] = {}
    for key, value in os.environ.items():
        if key in _STRIPPED_CLAUDE_ENV_VARS:
            continue
        if key.startswith("CLAUDE_BG_"):
            continue
        if (
            key.startswith("CLAUDE_CODE_HOST_")
            and key not in _PRESERVED_CLAUDE_HOST_VARS
        ):
            continue
        if key == "AI_AGENT" and value.startswith("claude-code"):
            continue
        env[key] = value

    env["CLAUDE_AGENT_SDK_CLIENT_APP"] = f"serabot-gateway/{GATEWAY_VERSION}"
    env["DISABLE_AUTOUPDATER"] = "1"

    if profile_dir is not None and str(profile_dir) != "default":
        env["CLAUDE_CONFIG_DIR"] = str(profile_dir)
    else:
        env.pop("CLAUDE_CONFIG_DIR", None)

    if strip_api_key:
        env.pop("ANTHROPIC_API_KEY", None)

    _apply_claude_credential(env, profile_dir)
    return env


PROFILE_TOKEN_FILE = "serabot-claude-token.json"
_CLAUDE_TOKEN_RE = re.compile(r"^sk-ant-(oat|api)[0-9]{2}-[A-Za-z0-9_-]{16,400}$")
_PLAN_AUTH_VARS = ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN")


def _claude_token_kind(token: str) -> str | None:
    """``oauth_token`` for a ``claude setup-token`` token (``sk-ant-oat…``,
    billed to the Claude plan), ``api_key`` for a Console key
    (``sk-ant-api…``, billed pay-as-you-go), else ``None``."""
    match = _CLAUDE_TOKEN_RE.match(token)
    if match is None:
        return None
    return "oauth_token" if match.group(1) == "oat" else "api_key"


def _read_profile_token(profile_dir: Path | None) -> tuple[str, str] | None:
    """The Claude credential stored for one gateway account, if any."""
    if profile_dir is None:
        return None
    try:
        doc = json.loads((profile_dir / PROFILE_TOKEN_FILE).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(doc, dict):
        return None
    token = doc.get("token")
    if not isinstance(token, str):
        return None
    kind = _claude_token_kind(token)
    return (kind, token) if kind is not None else None


def _store_profile_token(profile_dir: Path, token: str) -> str:
    """Store a Claude credential for one account, owner-only (0600)."""
    kind = _claude_token_kind(token)
    if kind is None:
        raise ValueError("not a Claude token")
    target = profile_dir / PROFILE_TOKEN_FILE
    tmp = profile_dir / (PROFILE_TOKEN_FILE + ".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump({"kind": kind, "token": token}, handle)
    os.replace(tmp, target)
    try:
        os.chmod(target, 0o600)
    except OSError:
        pass
    return kind


def _has_plan_login(profile_dir: Path | None) -> bool:
    base = profile_dir if profile_dir is not None else Path.home() / ".claude"
    return (base / ".credentials.json").is_file()


def _apply_claude_credential(env: dict[str, str], profile_dir: Path | None) -> None:
    """Pick the Claude credential the CLI/SDK uses for one account.

    - A stored ``claude setup-token`` token goes in ``CLAUDE_CODE_OAUTH_TOKEN``
      and every API key is removed, so usage counts against the Claude plan.
    - A stored Console key goes in ``ANTHROPIC_API_KEY`` (pay-as-you-go).
    - Otherwise a plan login (``claude auth login`` or a host
      ``CLAUDE_CODE_OAUTH_TOKEN``) wins: an ``ANTHROPIC_API_KEY`` left in the
      environment would silently switch it to API billing.

    A named account never inherits the host's OAuth token: it uses only its
    own login.
    """
    if profile_dir is not None:
        env.pop("CLAUDE_CODE_OAUTH_TOKEN", None)
    stored = _read_profile_token(profile_dir)
    if stored is not None:
        kind, token = stored
        if kind == "oauth_token":
            for name in _PLAN_AUTH_VARS:
                env.pop(name, None)
            env["CLAUDE_CODE_OAUTH_TOKEN"] = token
        else:
            env.pop("CLAUDE_CODE_OAUTH_TOKEN", None)
            env["ANTHROPIC_API_KEY"] = token
        return
    if env.get("CLAUDE_CODE_OAUTH_TOKEN") or _has_plan_login(profile_dir):
        for name in _PLAN_AUTH_VARS:
            env.pop(name, None)


def _find_claude_cli() -> str | None:
    override = os.environ.get(CLI_PATH_ENV_VAR)
    if override is not None:
        cleaned = override.strip()
        if not cleaned:
            return None
        if os.path.isfile(cleaned):
            return cleaned
        return shutil.which(cleaned)

    which_cli = shutil.which("claude")
    if which_cli:
        return which_cli

    exe_name = "claude.exe" if os.name == "nt" else "claude"
    home = Path.home()
    candidates: list[Path] = [
        home / ".local" / "bin" / exe_name,
        home / ".claude" / "local" / exe_name,
        home / ".npm-global" / "bin" / exe_name,
        Path("/usr/local/bin") / exe_name,
    ]
    # claude-agent-sdk bundles a native Claude Code binary under _bundled/.
    try:
        import importlib.util

        spec = importlib.util.find_spec("claude_agent_sdk")
        if spec is not None and spec.origin:
            sdk_dir = Path(spec.origin).resolve().parent
            candidates.append(sdk_dir / "_bundled" / exe_name)
    except Exception:
        pass

    for candidate in candidates:
        try:
            if candidate.is_file() and (os.name == "nt" or os.access(candidate, os.X_OK)):
                return str(candidate)
        except OSError:
            continue
    return None


def _cli_version(cli_path: str | None = None) -> str | None:
    resolved = cli_path if cli_path is not None else _find_claude_cli()
    if not resolved:
        return None
    try:
        proc = subprocess.run(
            [resolved, "--version"],
            env=_clean_claude_env(None),
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    out = (proc.stdout or "").strip()
    return out.splitlines()[0].strip() if out else None


def _read_credentials_fallback(profile_dir: Path | None) -> dict[str, Any] | None:
    base_dir = profile_dir if profile_dir is not None else Path.home() / ".claude"
    creds_path = base_dir / ".credentials.json"
    claude_json_path = base_dir / ".claude.json"

    oauth_account: dict[str, Any] = {}
    try:
        with open(claude_json_path, "r", encoding="utf-8") as handle:
            cj = json.load(handle)
        if isinstance(cj, dict) and isinstance(cj.get("oauthAccount"), dict):
            oauth_account = cj["oauthAccount"]
    except (OSError, ValueError):
        pass

    try:
        with open(creds_path, "r", encoding="utf-8") as handle:
            doc = json.load(handle)
    except (OSError, ValueError):
        doc = None

    oauth = doc.get("claudeAiOauth") if isinstance(doc, dict) else None
    if isinstance(oauth, dict) and oauth.get("accessToken"):
        acct = oauth.get("account") if isinstance(oauth.get("account"), dict) else {}
        return {
            "loggedIn": True,
            "authMethod": "claudeai",
            "email": acct.get("emailAddress") or oauth_account.get("emailAddress"),
            "orgId": acct.get("organizationUuid")
            or oauth_account.get("organizationUuid"),
            "orgName": acct.get("organizationName")
            or oauth_account.get("organizationName"),
            "subscriptionType": oauth.get("subscriptionType")
            or acct.get("subscriptionType")
            or oauth_account.get("billingType"),
        }

    if oauth_account and (
        oauth_account.get("emailAddress") or oauth_account.get("accountUuid")
    ):
        return {
            "loggedIn": True,
            "authMethod": "claudeai",
            "email": oauth_account.get("emailAddress"),
            "orgId": oauth_account.get("organizationUuid"),
            "orgName": oauth_account.get("organizationName"),
            "subscriptionType": oauth_account.get("billingType"),
        }

    return None


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(
        self, req: Any, fp: Any, code: int, msg: str, headers: Any, newurl: str
    ) -> Any:
        return None


def _utc_rfc3339_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _format_resets_at(raw: Any) -> str | None:
    if isinstance(raw, str):
        cleaned = raw.strip()
        return cleaned or None
    if isinstance(raw, (int, float)) and not isinstance(raw, bool):
        if math.isfinite(raw):
            try:
                return datetime.fromtimestamp(float(raw), tz=timezone.utc).strftime(
                    "%Y-%m-%dT%H:%M:%SZ"
                )
            except (OSError, OverflowError, ValueError):
                return None
    return None


def _read_oauth_access_token(profile_dir: Path | None) -> str | None:
    stored = _read_profile_token(profile_dir)
    if stored is not None:
        return stored[1] if stored[0] == "oauth_token" else None
    base_dir = profile_dir if profile_dir is not None else Path.home() / ".claude"
    creds_path = base_dir / ".credentials.json"
    try:
        with open(creds_path, "r", encoding="utf-8") as handle:
            doc = json.load(handle)
    except (OSError, ValueError):
        return None

    if not isinstance(doc, dict):
        return None
    oauth = doc.get("claudeAiOauth")
    if not isinstance(oauth, dict):
        return None
    token = oauth.get("accessToken")
    if isinstance(token, str) and token.strip():
        return token.strip()
    return None


def _parse_oauth_usage_snapshot(
    data: dict[str, Any], *, sampled_at: str | None = None
) -> dict[str, Any] | None:
    windows: list[dict[str, Any]] = []
    for key, label in _USAGE_WINDOW_SPECS:
        entry = data.get(key)
        if not isinstance(entry, dict):
            continue
        util = entry.get("utilization")
        if isinstance(util, bool) or not isinstance(util, (int, float)):
            continue
        if not math.isfinite(util):
            continue
        used_percent = max(0.0, min(100.0, float(util)))
        remaining = round(max(0.0, min(1.0, 1.0 - used_percent / 100.0)), 6)
        win: dict[str, Any] = {
            "scope": "subscription",
            "label": label,
            "used_percent": used_percent,
            "remaining": remaining,
        }
        resets_at = _format_resets_at(entry.get("resets_at"))
        if resets_at is not None:
            win["resets_at"] = resets_at
        windows.append(win)

    if not windows:
        return None

    return {
        "sampled_at": sampled_at or _utc_rfc3339_now(),
        "source": "claude",
        "state": "ok",
        "windows": windows,
    }


def _fetch_oauth_usage(access_token: str) -> dict[str, Any] | None:
    req = urllib.request.Request(
        ANTHROPIC_OAUTH_USAGE_URL,
        method="GET",
        headers={
            "Authorization": f"Bearer {access_token}",
            "anthropic-beta": "oauth-2025-04-20",
            "User-Agent": "claude-code/2.0.0",
            "Accept": "application/json",
        },
    )
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        with opener.open(req, timeout=USAGE_TIMEOUT_SECS) as resp:
            status = getattr(resp, "status", 200)
            if status != 200:
                return None
            raw = resp.read(USAGE_MAX_BODY_BYTES + 1)
    except (urllib.error.URLError, OSError, TimeoutError, ValueError) as exc:
        logger.debug("claude oauth usage request failed: %s", type(exc).__name__)
        return None

    if len(raw) > USAGE_MAX_BODY_BYTES:
        logger.warning(
            "claude oauth usage response exceeded %d bytes", USAGE_MAX_BODY_BYTES
        )
        return None

    try:
        data = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return None

    if not isinstance(data, dict):
        return None
    return _parse_oauth_usage_snapshot(data)


def _extract_rate_limit_info(msg: Any) -> dict[str, Any] | None:
    raw: Any = None
    if isinstance(msg, dict):
        raw = msg.get("rate_limit_info")
        if raw is None:
            raw = msg.get("rate_limits")
    else:
        raw = getattr(msg, "rate_limit_info", None)
        if raw is None:
            raw = getattr(msg, "rate_limits", None)

    if raw is None:
        return None
    if isinstance(raw, dict):
        return dict(raw) if raw else None
    if hasattr(raw, "__dict__"):
        extracted = {k: v for k, v in vars(raw).items() if not k.startswith("_")}
        return extracted if extracted else None
    return None


def _quota_snapshot_from_rate_limits(
    info: dict[str, Any], *, sampled_at: str | None = None
) -> dict[str, Any] | None:
    direct = _parse_oauth_usage_snapshot(info, sampled_at=sampled_at)
    if direct is not None:
        return direct

    rate_type = info.get("rateLimitType") or info.get("rate_limit_type")
    label_map = dict(_USAGE_WINDOW_SPECS)
    label = label_map.get(str(rate_type), str(rate_type)) if rate_type else "5h"

    used_percent: float | None = None
    raw_used = info.get("used_percent")
    if (
        not isinstance(raw_used, bool)
        and isinstance(raw_used, (int, float))
        and math.isfinite(raw_used)
    ):
        used_percent = max(0.0, min(100.0, float(raw_used)))
    else:
        raw_util = info.get("utilization")
        if (
            not isinstance(raw_util, bool)
            and isinstance(raw_util, (int, float))
            and math.isfinite(raw_util)
        ):
            val = float(raw_util)
            if 0.0 <= val <= 1.0:
                val *= 100.0
            used_percent = max(0.0, min(100.0, val))
        elif info.get("status") == "rejected":
            used_percent = 100.0

    if used_percent is None:
        return None

    remaining = round(max(0.0, min(1.0, 1.0 - used_percent / 100.0)), 6)
    win: dict[str, Any] = {
        "scope": "subscription",
        "label": label,
        "used_percent": used_percent,
        "remaining": remaining,
    }
    resets_at = _format_resets_at(info.get("resets_at") or info.get("resetsAt"))
    if resets_at is not None:
        win["resets_at"] = resets_at

    return {
        "sampled_at": sampled_at or _utc_rfc3339_now(),
        "source": "claude",
        "state": "ok",
        "windows": [win],
    }


def _read_cli_status(profile_dir: Path | None) -> dict[str, Any] | None:
    cli_path = _find_claude_cli()
    if cli_path:
        try:
            proc = subprocess.run(
                [cli_path, "auth", "status", "--json"],
                env=_clean_claude_env(profile_dir),
                capture_output=True,
                text=True,
                timeout=CLI_STATUS_TIMEOUT_SECS,
                check=False,
            )
            raw_out = (proc.stdout or "").strip()
            if proc.returncode == 0 and raw_out:
                data = json.loads(raw_out)
                if isinstance(data, dict) and "loggedIn" in data:
                    if not data.get("loggedIn"):
                        return None
                    return {
                        "loggedIn": True,
                        "authMethod": data.get("authMethod"),
                        "email": data.get("email"),
                        "orgId": data.get("orgId"),
                        "orgName": data.get("orgName"),
                        "subscriptionType": data.get("subscriptionType"),
                    }
        except (OSError, ValueError, subprocess.SubprocessError):
            pass

    stored = _read_profile_token(profile_dir)
    if stored is not None:
        return {
            "loggedIn": True,
            "authMethod": stored[0],
            "email": None,
            "orgId": None,
            "orgName": None,
            "subscriptionType": None,
        }
    return _read_credentials_fallback(profile_dir)


def _list_profiles() -> list[dict[str, Any]]:
    profiles: list[dict[str, Any]] = []
    default_acct = _read_cli_status(None)
    profiles.append(
        {
            "profile": "default",
            "signed_in": default_acct is not None,
            "account": default_acct,
        }
    )

    profiles_root = _state_dir() / "profiles"
    if profiles_root.is_dir():
        try:
            entries = sorted(profiles_root.iterdir(), key=lambda p: p.name)
        except OSError:
            entries = []
        for entry in entries:
            if not entry.is_dir():
                continue
            if not PROFILE_ID_RE.match(entry.name) or entry.name == "default":
                continue
            acct = _read_cli_status(entry)
            profiles.append(
                {
                    "profile": entry.name,
                    "signed_in": acct is not None,
                    "account": acct,
                }
            )
    return profiles


def _import_sdk() -> Any:
    import claude_agent_sdk  # type: ignore[import-not-found]

    return claude_agent_sdk


def _is_sdk_installed() -> bool:
    try:
        _import_sdk()
        return True
    except ImportError:
        return False


def _check_bearer_auth(request: Request) -> JSONResponse | None:
    expected = os.environ.get(TOKEN_ENV_VAR, "")
    if not expected:
        return JSONResponse(
            {"error": "gateway token not configured"},
            status_code=503,
        )

    auth_header = request.headers.get("authorization", "")
    scheme, _, provided = auth_header.partition(" ")
    if scheme.lower() != "bearer" or not provided:
        return JSONResponse({"error": "unauthorized"}, status_code=401)

    if not hmac.compare_digest(
        provided.encode("utf-8"),
        expected.encode("utf-8"),
    ):
        return JSONResponse({"error": "unauthorized"}, status_code=401)

    return None


def _redact_secrets(text: str) -> str:
    redacted = _SECRET_REDACT_RE.sub("[REDACTED]", text)
    if len(redacted) > 300:
        return redacted[:297] + "..."
    return redacted


def _classify_turn_error(exc: Exception) -> tuple[int, str, str]:
    if isinstance(exc, ImportError):
        return 503, "sdk_missing", "claude-agent-sdk is not installed"

    name = type(exc).__name__.lower()
    raw = str(exc)
    lower = raw.lower()

    if (
        isinstance(exc, FileNotFoundError)
        or "clinotfound" in name
        or "cli not found" in lower
        or "claude not found" in lower
        or "enoent" in lower
    ):
        return 503, "cli_missing", "claude CLI is not installed or not found"

    if any(
        tok in lower
        for tok in (
            "unauthorized",
            "authentication",
            "not logged in",
            "invalid api key",
            "invalid_api_key",
            "expired token",
            "oauth",
            "401",
        )
    ):
        return 401, "auth_failed", _redact_secrets(raw) or "authentication failed"

    if any(
        tok in lower
        for tok in ("rate limit", "rate_limit", "429", "too many requests", "overloaded")
    ):
        return 429, "rate_limited", _redact_secrets(raw) or "rate limited"

    if any(
        tok in lower
        for tok in ("billing", "credit", "quota", "payment", "402", "insufficient")
    ):
        return 402, "billing_failed", _redact_secrets(raw) or "billing error"

    if any(
        tok in lower
        for tok in ("model not found", "unknown model", "not_found_error", "404")
    ):
        return 404, "model_not_found", _redact_secrets(raw) or "model not found"

    return 502, "turn_failed", _redact_secrets(raw) or "turn failed"


def _render_content(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: list[str] = []
        for block in content:
            if isinstance(block, str):
                parts.append(block)
            elif isinstance(block, dict):
                btype = block.get("type")
                if btype == "text" and isinstance(block.get("text"), str):
                    parts.append(block["text"])
                elif btype == "tool_use":
                    parts.append(
                        f"[tool_use {block.get('name', '')}: "
                        f"{json.dumps(block.get('input', {}))}]"
                    )
                elif btype == "tool_result":
                    parts.append(
                        f"[tool_result {block.get('tool_use_id', '')}: "
                        f"{_render_content(block.get('content', ''))}]"
                    )
        return "\n".join(p for p in parts if p)
    return str(content) if content is not None else ""


def _format_messages_prompt(messages: list[Any]) -> str:
    if len(messages) == 1 and isinstance(messages[0], dict):
        if messages[0].get("role") == "user":
            return _render_content(messages[0].get("content", ""))

    rendered: list[str] = []
    for msg in messages:
        if not isinstance(msg, dict):
            continue
        role = str(msg.get("role", "user"))
        text = _render_content(msg.get("content", ""))
        if text:
            rendered.append(f"[{role}]\n{text}")
    return "\n\n".join(rendered)


def _is_instance_or_attr(obj: Any, cls: Any, attr_name: str) -> bool:
    if isinstance(cls, type) and isinstance(obj, cls):
        return True
    return hasattr(obj, attr_name)


def _build_agent_options(
    sdk: Any,
    *,
    model: str,
    system_prompt: str,
    mcp_servers: dict[str, Any],
    allowed_tools: list[str],
    max_turns: int,
    clean_env: dict[str, str],
    effort: str | None,
) -> Any:
    kwargs: dict[str, Any] = {
        "model": model,
        "system_prompt": system_prompt,
        "tools": [],
        "setting_sources": [],
        "permission_mode": "default",
        "mcp_servers": mcp_servers,
        "allowed_tools": allowed_tools,
        "max_turns": max_turns,
    }
    try:
        sig = inspect.signature(sdk.ClaudeAgentOptions)
        params = sig.parameters
        accepts_varkw = any(
            p.kind == inspect.Parameter.VAR_KEYWORD for p in params.values()
        )
        if accepts_varkw or "env" in params:
            kwargs["env"] = clean_env
        if effort is not None and (accepts_varkw or "effort" in params):
            kwargs["effort"] = effort
        cli_path = _find_claude_cli()
        if cli_path and (accepts_varkw or "cli_path" in params):
            kwargs["cli_path"] = cli_path
    except (TypeError, ValueError):
        pass

    return sdk.ClaudeAgentOptions(**kwargs)


async def _run_turn(
    payload: dict[str, Any],
    *,
    profile_dir: Path | None = None,
    profile_key: str | None = None,
    effort: str | None = None,
) -> dict[str, Any]:
    resolved_profile_key = (
        profile_key
        if profile_key is not None
        else (profile_dir.name if profile_dir is not None else "default")
    )
    sdk = _import_sdk()

    model = str(payload.get("model") or DEFAULT_MODEL)
    system = payload.get("system")
    system_prompt = str(system) if system is not None else ""
    messages = payload.get("messages") or []
    if not isinstance(messages, list):
        raise ValueError("'messages' must be a list")
    raw_tools = payload.get("tools") or []
    if not isinstance(raw_tools, list):
        raise ValueError("'tools' must be a list")

    prompt = _format_messages_prompt(messages)

    captured_calls: list[dict[str, Any]] = []
    sdk_tools: list[Any] = []
    allowed_tools: list[str] = []

    for tool_def in raw_tools:
        if not isinstance(tool_def, dict) or not tool_def.get("name"):
            raise ValueError("Each tool entry must be an object with a 'name'")
        t_name = str(tool_def["name"])
        t_desc = str(tool_def.get("description") or "")
        t_schema = (
            tool_def.get("input_schema")
            or tool_def.get("parameters")
            or {"type": "object", "properties": {}}
        )

        def _make_handler(captured_name: str) -> Any:
            async def _capture(args: dict[str, Any]) -> dict[str, Any]:
                captured_calls.append(
                    {
                        "id": f"call_{uuid.uuid4().hex[:12]}",
                        "name": captured_name,
                        "arguments": dict(args or {}),
                    }
                )
                return {
                    "content": [
                        {
                            "type": "text",
                            "text": "Tool execution delegated to SeraBot gateway.",
                        }
                    ]
                }

            return _capture

        decorated = sdk.tool(t_name, t_desc, t_schema)(_make_handler(t_name))
        sdk_tools.append(decorated)
        allowed_tools.append(f"{MCP_TOOL_PREFIX}{t_name}")

    mcp_servers: dict[str, Any] = {}
    if sdk_tools:
        mcp_server = sdk.create_sdk_mcp_server(
            name=MCP_SERVER_NAME,
            version="1.0.0",
            tools=sdk_tools,
        )
        mcp_servers[MCP_SERVER_NAME] = mcp_server

    clean_env = _clean_claude_env(
        profile_dir, strip_api_key=(profile_dir is not None)
    )
    options = _build_agent_options(
        sdk,
        model=model,
        system_prompt=system_prompt,
        mcp_servers=mcp_servers,
        allowed_tools=allowed_tools,
        max_turns=2 if sdk_tools else 1,
        clean_env=clean_env,
        effort=effort,
    )

    assistant_msg_cls = getattr(sdk, "AssistantMessage", None)
    result_msg_cls = getattr(sdk, "ResultMessage", None)
    text_block_cls = getattr(sdk, "TextBlock", None)
    tool_use_block_cls = getattr(sdk, "ToolUseBlock", None)

    text_parts: list[str] = []
    assistant_tool_calls: list[dict[str, Any]] = []
    seen_tool_ids: set[str] = set()
    usage: dict[str, Any] = {}

    stream = sdk.query(prompt=prompt, options=options)
    try:
        async for msg in stream:
            rate_info = _extract_rate_limit_info(msg)
            if rate_info is not None:
                _last_rate_limits[resolved_profile_key] = {
                    "sampled_at": _utc_rfc3339_now(),
                    "info": rate_info,
                }

            if _is_instance_or_attr(msg, assistant_msg_cls, "content"):
                for block in getattr(msg, "content", []) or []:
                    is_tool_block = (
                        isinstance(tool_use_block_cls, type)
                        and isinstance(block, tool_use_block_cls)
                    ) or hasattr(block, "input") and hasattr(block, "name")

                    if is_tool_block:
                        raw_name = str(getattr(block, "name", ""))
                        clean_name = (
                            raw_name[len(MCP_TOOL_PREFIX) :]
                            if raw_name.startswith(MCP_TOOL_PREFIX)
                            else raw_name
                        )
                        call_id = str(
                            getattr(block, "id", None)
                            or f"call_{uuid.uuid4().hex[:12]}"
                        )
                        if call_id not in seen_tool_ids:
                            seen_tool_ids.add(call_id)
                            assistant_tool_calls.append(
                                {
                                    "id": call_id,
                                    "name": clean_name,
                                    "arguments": dict(
                                        getattr(block, "input", None) or {}
                                    ),
                                }
                            )
                    elif (
                        isinstance(text_block_cls, type)
                        and isinstance(block, text_block_cls)
                    ) or isinstance(getattr(block, "text", None), str):
                        text_parts.append(str(block.text))

                if assistant_tool_calls or captured_calls:
                    break

            elif _is_instance_or_attr(msg, result_msg_cls, "subtype"):
                msg_usage = getattr(msg, "usage", None)
                if isinstance(msg_usage, dict):
                    usage = msg_usage
                if not text_parts and isinstance(getattr(msg, "result", None), str):
                    text_parts.append(msg.result)
    finally:
        aclose = getattr(stream, "aclose", None)
        if callable(aclose):
            await aclose()

    tool_calls = assistant_tool_calls or captured_calls
    finish_reason = "tool_use" if tool_calls else "stop"

    return {
        "content": "".join(text_parts),
        "tool_calls": tool_calls,
        "usage": usage,
        "finish_reason": finish_reason,
    }


def _strip_ansi(text: str) -> str:
    return _ANSI_RE.sub("", text)


def _extract_https_url(text: str) -> str | None:
    """The first https URL the CLI printed, once it is complete.

    A URL counts only when something follows it (the line break the CLI
    prints after it), so a URL split across two reads is never cut short.
    """
    cleaned = _strip_ansi(text)
    for match in _HTTPS_URL_RE.finditer(cleaned):
        if match.end() < len(cleaned):
            return match.group(0)
    return None


def _make_browser_capture() -> tuple[str, Path, Path] | None:
    """A one-shot ``$BROWSER`` that records the URL the CLI wants opened.

    ``claude auth login`` hands its *automatic* sign-in URL (whose redirect is
    the CLI's own ``http://localhost:<port>/callback`` listener) to
    ``$BROWSER`` and only prints the *manual* URL (whose last page shows a
    code to paste). Capturing the first lets the desktop finish the sign-in
    without any copy and paste. POSIX only; Windows falls back to the
    printed URL and a pasted code.
    """
    if os.name == "nt":
        return None
    try:
        capture_dir = Path(tempfile.mkdtemp(prefix="serabot-claude-login-"))
        os.chmod(capture_dir, 0o700)
        url_file = capture_dir / "url"
        script = capture_dir / "open-browser"
        script.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$1\" > {shlex.quote(str(url_file))}\n"
            "exit 0\n",
            encoding="utf-8",
        )
        os.chmod(script, 0o700)
    except OSError:
        return None
    return str(script), url_file, capture_dir


def _read_captured_url(url_file: Path | None) -> str | None:
    if url_file is None:
        return None
    try:
        text = url_file.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None
    first = text.splitlines()[0].strip() if text else ""
    if not first.startswith("https://") or len(first) > 4096:
        return None
    return first


def _parse_cli_callback(auth_url: str | None) -> dict[str, Any] | None:
    """The CLI's own loopback callback, read from its automatic sign-in URL.

    Only a plain-HTTP loopback redirect on an unprivileged port is accepted;
    anything else means the URL is not the CLI's localhost flow.
    """
    if not auth_url:
        return None
    try:
        parsed = urllib.parse.urlsplit(auth_url)
        query = urllib.parse.parse_qs(parsed.query)
    except ValueError:
        return None
    if parsed.scheme != "https":
        return None
    redirect = (query.get("redirect_uri") or [""])[0]
    state = (query.get("state") or [""])[0]
    if not redirect or not state or len(state) > 512:
        return None
    try:
        target = urllib.parse.urlsplit(redirect)
        port = target.port
    except ValueError:
        return None
    if target.scheme != "http" or target.hostname not in ("localhost", "127.0.0.1", "::1"):
        return None
    if port is None or port < 1024:
        return None
    if target.query or target.fragment or target.username or target.password:
        return None
    path = target.path or "/"
    if not _CALLBACK_PATH_RE.fullmatch(path):
        return None
    return {"port": port, "path": path, "state": state}


def _terminate_proc(proc: Any) -> None:
    if proc is None:
        return
    try:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=2)
    except Exception:
        pass


def _cleanup_flow(flow: dict[str, Any]) -> None:
    _terminate_proc(flow.get("proc"))
    master_fd = flow.get("master_fd")
    if master_fd is not None:
        try:
            os.close(master_fd)
        except OSError:
            pass
        flow["master_fd"] = None
    proc = flow.get("proc")
    if proc is not None:
        for stream_name in ("stdin", "stdout", "stderr"):
            stream = getattr(proc, stream_name, None)
            if stream is not None:
                try:
                    stream.close()
                except OSError:
                    pass
    capture_dir = flow.get("capture_dir")
    if capture_dir is not None:
        shutil.rmtree(capture_dir, ignore_errors=True)
        flow["capture_dir"] = None


def _prune_flows() -> None:
    now = time.time()
    expired_ids = [
        fid for fid, fl in _pending_auth_flows.items() if now >= fl["expires"]
    ]
    for fid in expired_ids:
        fl = _pending_auth_flows.pop(fid, None)
        if fl is not None:
            _cleanup_flow(fl)

    while len(_pending_auth_flows) >= MAX_AUTH_FLOWS:
        oldest_id = min(
            _pending_auth_flows,
            key=lambda fid: _pending_auth_flows[fid].get("created_at", 0.0),
        )
        fl = _pending_auth_flows.pop(oldest_id, None)
        if fl is not None:
            _cleanup_flow(fl)


def _drain_flow_output(flow: dict[str, Any]) -> None:
    master_fd = flow.get("master_fd")
    if master_fd is not None:
        while True:
            try:
                ready, _, _ = select.select([master_fd], [], [], 0.0)
            except (OSError, ValueError):
                break
            if not ready:
                break
            try:
                chunk = os.read(master_fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            flow["output_buf"] += chunk.decode("utf-8", errors="replace")
        return

    proc = flow.get("proc")
    stdout = getattr(proc, "stdout", None) if proc is not None else None
    if stdout is not None and proc.poll() is not None:
        try:
            rest = stdout.read()
            if rest:
                flow["output_buf"] += (
                    rest.decode("utf-8", errors="replace")
                    if isinstance(rest, bytes)
                    else str(rest)
                )
        except Exception:
            pass


def _spawn_login(cli_path: str, profile_dir: Path | None) -> dict[str, Any]:
    """Start ``claude auth login`` and wait for its sign-in URLs.

    Returns the flow pieces: the process (and pty), the printed manual URL,
    and — when ``$BROWSER`` could be captured — the automatic URL with the
    CLI's loopback callback.
    """
    env = _clean_claude_env(profile_dir, strip_api_key=True)
    env.pop("NO_BROWSER", None)
    capture = _make_browser_capture()
    url_file: Path | None = None
    capture_dir: Path | None = None
    if capture is not None:
        env["BROWSER"], url_file, capture_dir = capture
    else:
        env["BROWSER"] = "true"
    cmd = [cli_path, "auth", "login", "--claudeai"]

    master_fd: int | None = None
    output_buf = ""
    manual_url: str | None = None
    deadline = time.monotonic() + AUTH_START_TIMEOUT_SECS

    try:
        if pty is not None and os.name != "nt":
            master_fd, slave_fd = pty.openpty()
            try:
                proc = subprocess.Popen(
                    cmd,
                    stdin=slave_fd,
                    stdout=slave_fd,
                    stderr=slave_fd,
                    env=env,
                    close_fds=True,
                )
            finally:
                os.close(slave_fd)
            while time.monotonic() < deadline:
                remaining = max(0.05, min(0.2, deadline - time.monotonic()))
                try:
                    ready, _, _ = select.select([master_fd], [], [], remaining)
                except (OSError, ValueError):
                    break
                if ready:
                    try:
                        chunk = os.read(master_fd, 4096)
                    except OSError:
                        break
                    if not chunk:
                        break
                    output_buf += chunk.decode("utf-8", errors="replace")
                    manual_url = _extract_https_url(output_buf)
                    if manual_url:
                        break
                elif proc.poll() is not None:
                    break
        else:
            proc = subprocess.Popen(
                cmd,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                env=env,
            )
            while time.monotonic() < deadline and proc.stdout is not None:
                line = proc.stdout.readline()
                if not line:
                    if proc.poll() is not None:
                        break
                    time.sleep(0.05)
                    continue
                output_buf += (
                    line.decode("utf-8", errors="replace")
                    if isinstance(line, bytes)
                    else str(line)
                )
                manual_url = _extract_https_url(output_buf)
                if manual_url:
                    break
    except Exception:
        if capture_dir is not None:
            shutil.rmtree(capture_dir, ignore_errors=True)
        raise

    flow: dict[str, Any] = {
        "proc": proc,
        "master_fd": master_fd,
        "output_buf": output_buf,
        "capture_dir": capture_dir,
    }
    if not manual_url:
        _cleanup_flow(flow)
        raise RuntimeError("claude auth login did not emit an authorization URL")

    # The CLI calls $BROWSER before it prints the fallback URL; allow a
    # moment for the helper's write to land.
    auto_url = _read_captured_url(url_file)
    grace = time.monotonic() + BROWSER_CAPTURE_GRACE_SECS
    while auto_url is None and url_file is not None and time.monotonic() < grace:
        time.sleep(0.05)
        auto_url = _read_captured_url(url_file)

    callback = _parse_cli_callback(auto_url)
    flow["manual_url"] = manual_url
    flow["auto_url"] = auto_url if callback is not None else None
    flow["callback"] = callback
    return flow


PASTE_PROMPT = "Paste code here if prompted >"


def _wait_for_exit(flow: dict[str, Any], since: int | None = None) -> int | None:
    """Wait for ``claude auth login`` to exit and return its code.

    With ``since`` (the output length before a code was pasted), returns
    ``None`` when the CLI asked for the code again instead of exiting: the
    paste was refused but the sign-in is still open.
    """
    proc = flow.get("proc")
    deadline = time.monotonic() + AUTH_COMPLETE_TIMEOUT_SECS
    while time.monotonic() < deadline:
        _drain_flow_output(flow)
        rc = proc.poll() if proc is not None else None
        if rc is not None:
            _drain_flow_output(flow)
            return int(rc)
        if since is not None and PASTE_PROMPT in _strip_ansi(flow["output_buf"][since:]):
            return None
        time.sleep(0.05)

    _terminate_proc(proc)
    raise TimeoutError("timed out waiting for claude auth login to finish")


def _write_code_and_wait(flow: dict[str, Any], code: str) -> int | None:
    _drain_flow_output(flow)
    since = len(flow["output_buf"])
    payload = (code.strip() + "\n").encode("utf-8")
    master_fd = flow.get("master_fd")
    proc = flow.get("proc")

    if master_fd is not None:
        os.write(master_fd, payload)
    elif proc is not None and proc.stdin is not None:
        proc.stdin.write(payload)
        proc.stdin.flush()

    return _wait_for_exit(flow, since=since if master_fd is not None else None)


def _relay_callback_and_wait(
    flow: dict[str, Any], code: str, state: str
) -> int | None:
    """Hand a browser redirect to the CLI's own loopback listener.

    The browser that finished the sign-in may sit on another machine than
    this gateway (the desktop caught the redirect there); the CLI that is
    waiting for it only listens on this machine's loopback. Only the
    callback port and path the CLI itself announced are ever contacted.
    """
    callback = flow["callback"]
    target = callback["path"] + "?" + urllib.parse.urlencode(
        {"code": code, "state": state}
    )
    delivered = False
    for host in ("127.0.0.1", "::1"):
        conn = http.client.HTTPConnection(
            host, callback["port"], timeout=AUTH_COMPLETE_TIMEOUT_SECS
        )
        try:
            conn.request(
                "GET", target, headers={"Host": f"localhost:{callback['port']}"}
            )
            response = conn.getresponse()
            response.read(64 * 1024)
            delivered = True
            break
        except (OSError, http.client.HTTPException):
            continue
        finally:
            conn.close()
    if not delivered:
        raise ConnectionError("the Claude CLI sign-in listener is not reachable")
    return _wait_for_exit(flow)


def _cli_lines(text: str) -> list[str]:
    """The CLI's output as lines, with the paste prompt split off its own
    line (the CLI prints its answer right after the prompt)."""
    cleaned = _strip_ansi(text).replace("\r", "\n").replace(PASTE_PROMPT, "\n")
    return [line.strip() for line in cleaned.splitlines() if line.strip()]


def _cli_failure_message(text: str, rc: int | None, code: str | None) -> str:
    """The CLI's own reason for a refused sign-in, safe to show the member.

    The pty echoes a pasted code back, so the code is masked wherever it
    appears and the echo line itself is skipped.
    """
    secret = (code or "").strip()
    for line in reversed(_cli_lines(text)):
        if secret and line == secret:
            continue
        if line.startswith(("Signing in", "Opening browser", "If the browser")):
            continue
        if line.startswith("https://"):
            continue
        if secret:
            line = line.replace(secret, "…")
        if line.startswith("Couldn't log in:"):
            line = line[len("Couldn't log in:") :].strip()
        if "status code 400" in line or "status code 401" in line:
            return (
                "Claude did not accept the sign-in. The code may have expired or "
                "been used already. Start the sign-in again."
            )
        message = _redact_secrets(line)
        if message:
            return message[:240]
    if rc is None:
        return "Claude asked for the code again. Copy all of it, including the # in the middle."
    return f"Claude did not finish the sign-in (claude auth login exited {rc})."


def _finish_flow(
    flow: dict[str, Any], rc: int, code: str | None = None
) -> dict[str, Any]:
    """Close the CLI and remember how the sign-in ended, for later polls."""
    _cleanup_flow(flow)
    flow["proc"] = None
    if rc == 0:
        account = _read_cli_status(flow["profile_dir"])
        if account is not None:
            result = {"status": "ok", "profile": flow["profile"], "account": account}
            logger.info("CLI sign-in stored credentials for profile=%s", flow["profile"])
        else:
            result = {
                "status": "denied",
                "code": "auth_complete_failed",
                "error": "Claude finished the sign-in but no login was stored.",
            }
    else:
        result = {
            "status": "denied",
            "code": "auth_code_rejected",
            "error": _cli_failure_message(flow.get("output_buf", ""), rc, code),
        }
    flow["result"] = result
    flow["completing"] = False
    return result


async def health(request: Request) -> JSONResponse:
    cli_path = _find_claude_cli()
    return JSONResponse(
        {
            "status": "ok",
            "sdk": "claude-agent-sdk",
            "version": GATEWAY_VERSION,
            "auth_configured": bool(os.environ.get(TOKEN_ENV_VAR)),
            "cli_installed": cli_path is not None,
            "sdk_installed": _is_sdk_installed(),
        }
    )


async def list_models(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error
    return JSONResponse({"data": SUPPORTED_MODELS})


async def turn(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    try:
        payload = await request.json()
    except Exception:
        return JSONResponse({"error": "invalid JSON body"}, status_code=400)

    if not isinstance(payload, dict):
        return JSONResponse({"error": "JSON body must be an object"}, status_code=400)

    raw_profile = payload.get("profile")
    if raw_profile is None:
        raw_profile = request.headers.get("x-claude-profile")
    if raw_profile is not None and not isinstance(raw_profile, str):
        return JSONResponse(
            {"error": "profile must be a string", "code": "invalid_profile"},
            status_code=400,
        )
    try:
        profile_key, profile_dir = _resolve_profile_dir(raw_profile, create=False)
    except ValueError as exc:
        return JSONResponse(
            {"error": str(exc), "code": "invalid_profile"}, status_code=400
        )

    effort = payload.get("effort")
    if effort is not None:
        if not isinstance(effort, str) or effort not in VALID_EFFORTS:
            return JSONResponse(
                {
                    "error": "effort must be one of: low, medium, high, max",
                    "code": "invalid_effort",
                },
                status_code=400,
            )

    try:
        result = await _run_turn(
            payload, profile_dir=profile_dir, profile_key=profile_key, effort=effort
        )
    except ValueError as exc:
        return JSONResponse({"error": str(exc)}, status_code=400)
    except Exception as exc:
        status_code, err_code, err_msg = _classify_turn_error(exc)
        logger.warning("turn failed code=%s status=%d", err_code, status_code)
        return JSONResponse(
            {"error": err_msg, "code": err_code}, status_code=status_code
        )

    logger.info(
        "turn completed model=%s finish_reason=%s tool_calls=%d",
        payload.get("model") or DEFAULT_MODEL,
        result["finish_reason"],
        len(result["tool_calls"]),
    )
    return JSONResponse(result)


async def auth_status(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    cli_path = _find_claude_cli()
    cli_installed = cli_path is not None
    cli_ver = _cli_version(cli_path) if cli_installed else None
    profiles = _list_profiles()
    default_entry = profiles[0]
    default_cli_login = bool(default_entry["signed_in"])

    signed_in_entry = next((p for p in profiles if p["signed_in"]), None)
    return JSONResponse(
        {
            "signed_in": signed_in_entry is not None,
            "account": signed_in_entry["account"] if signed_in_entry else None,
            "cli_installed": cli_installed,
            "cli_version": cli_ver,
            "sdk_installed": _is_sdk_installed(),
            "default_cli_login": default_cli_login,
            "profiles": profiles,
        }
    )


async def auth_start(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    body_bytes = await request.body()
    if body_bytes.strip():
        try:
            payload = json.loads(body_bytes)
        except Exception:
            return JSONResponse({"error": "invalid JSON body"}, status_code=400)
        if not isinstance(payload, dict):
            return JSONResponse(
                {"error": "JSON body must be an object"}, status_code=400
            )
    else:
        payload = {}

    raw_profile = payload.get("profile")
    if raw_profile is None or raw_profile == "":
        profile = f"acct_{secrets.token_hex(4)}"
    else:
        if not isinstance(raw_profile, str):
            return JSONResponse(
                {"error": "profile must be a string", "code": "invalid_profile"},
                status_code=400,
            )
        try:
            profile = _validate_profile_id(raw_profile)
        except ValueError as exc:
            return JSONResponse(
                {"error": str(exc), "code": "invalid_profile"}, status_code=400
            )

    cli_path = _find_claude_cli()
    if not cli_path:
        return JSONResponse(
            {
                "error": "claude CLI is not installed on this machine",
                "code": "claude_cli_missing",
            },
            status_code=503,
        )

    _prune_flows()
    profile_name, profile_dir = _resolve_profile_dir(profile, create=True)

    try:
        flow = await asyncio.to_thread(_spawn_login, cli_path, profile_dir)
    except Exception as exc:
        logger.warning("auth start failed: %s", type(exc).__name__)
        return JSONResponse(
            {
                "error": "claude auth login did not start; check that `claude --version` works on the gateway host",
                "code": "auth_start_failed",
            },
            status_code=502,
        )

    flow_id = secrets.token_urlsafe(16)
    now = time.time()
    flow.update(
        {
            "flow_id": flow_id,
            "profile": profile_name,
            "profile_dir": profile_dir,
            "created_at": now,
            "expires": now + AUTH_FLOW_TTL_SECS,
            "completing": False,
        }
    )
    _pending_auth_flows[flow_id] = flow
    callback = flow.get("callback")
    reply: dict[str, Any] = {
        "flow_id": flow_id,
        # The automatic URL when the CLI's callback is known: the browser
        # redirect then finishes the sign-in with no code to paste.
        "url": flow.get("auto_url") or flow["manual_url"],
        "manual_url": flow["manual_url"],
        "profile": profile_name,
        "expires_in": AUTH_FLOW_TTL_SECS,
        "mode": "callback" if callback is not None else "paste",
    }
    if callback is not None:
        reply["callback"] = {"port": callback["port"], "path": callback["path"]}
        reply["state"] = callback["state"]
    return JSONResponse(reply)


def _parse_pasted_callback(text: str) -> tuple[str, str] | None:
    """``code`` and ``state`` from a pasted ``http://localhost:…/callback?…`` URL."""
    if not text.startswith(("http://localhost", "http://127.0.0.1", "http://[::1]")):
        return None
    try:
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(text).query)
    except ValueError:
        return None
    code = (query.get("code") or [""])[0].strip()
    state = (query.get("state") or [""])[0].strip()
    if not code or not state:
        return None
    return code, state


def _flow_result_reply(flow: dict[str, Any]) -> dict[str, Any]:
    result = dict(flow["result"])
    result.setdefault("profile", flow["profile"])
    return result


async def auth_flow_status(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    flow_id = request.path_params.get("flow_id", "")
    flow = _pending_auth_flows.get(flow_id)
    if flow is None:
        return JSONResponse(
            {"error": "unknown sign-in flow", "code": "flow_not_found"},
            status_code=404,
        )

    # A finished flow keeps answering until it expires, so a poll that
    # races a pasted code or a relayed callback still sees how it ended.
    if "result" in flow:
        return JSONResponse(_flow_result_reply(flow))

    if time.time() >= flow["expires"]:
        _pending_auth_flows.pop(flow_id, None)
        _cleanup_flow(flow)
        return JSONResponse({"status": "expired"})

    if flow.get("completing"):
        return JSONResponse({"status": "pending", "profile": flow["profile"]})

    _drain_flow_output(flow)
    proc = flow.get("proc")
    rc = proc.poll() if proc is not None else None
    if rc is None:
        return JSONResponse({"status": "pending", "profile": flow["profile"]})

    # The CLI finished by itself: its own localhost callback was reached.
    _finish_flow(flow, rc)
    return JSONResponse(_flow_result_reply(flow))


async def auth_complete(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    try:
        payload = await request.json()
    except Exception:
        return JSONResponse({"error": "invalid JSON body"}, status_code=400)
    if not isinstance(payload, dict):
        return JSONResponse({"error": "JSON body must be an object"}, status_code=400)

    flow_id = payload.get("flow_id")
    code = payload.get("code")
    state = payload.get("state")
    if not isinstance(flow_id, str) or not isinstance(code, str):
        return JSONResponse(
            {"error": "flow_id and code are required"}, status_code=400
        )
    code = code.strip()
    if not flow_id or not code:
        return JSONResponse(
            {"error": "flow_id and code are required"}, status_code=400
        )
    if len(code) > MAX_AUTH_CODE_CHARS or any(ord(ch) < 0x20 or ord(ch) == 0x7F for ch in code):
        return JSONResponse(
            {"error": "that does not look like a sign-in code", "code": "invalid_code"},
            status_code=400,
        )
    if state is not None and (not isinstance(state, str) or not state):
        return JSONResponse(
            {"error": "state must be a non-empty string", "code": "invalid_state"},
            status_code=400,
        )

    flow = _pending_auth_flows.get(flow_id)
    if flow is None:
        return JSONResponse(
            {"error": "unknown or used sign-in flow", "code": "flow_not_found"},
            status_code=404,
        )

    if "result" in flow:
        result = flow["result"]
        if result.get("status") == "ok":
            return JSONResponse(
                {"ok": True, "profile": flow["profile"], "account": result["account"]}
            )
        return JSONResponse(
            {
                "error": "this sign-in already ended; start a new one",
                "code": "flow_used",
            },
            status_code=409,
        )

    if flow.get("completing"):
        return JSONResponse(
            {"error": "this sign-in is already finishing", "code": "flow_busy"},
            status_code=409,
        )

    if time.time() >= flow["expires"]:
        _pending_auth_flows.pop(flow_id, None)
        _cleanup_flow(flow)
        return JSONResponse(
            {"error": "sign-in expired; start again", "code": "flow_expired"},
            status_code=410,
        )

    if state is None and flow.get("callback") is not None:
        # The member copied the browser's address bar after a localhost
        # redirect that did not load: relay it like a caught redirect.
        pasted = _parse_pasted_callback(code)
        if pasted is not None:
            code, state = pasted

    if state is not None:
        callback = flow.get("callback")
        if callback is None:
            return JSONResponse(
                {
                    "error": "this sign-in has no browser callback; paste the code instead",
                    "code": "no_callback",
                },
                status_code=400,
            )
        if not hmac.compare_digest(state.encode("utf-8"), callback["state"].encode("utf-8")):
            return JSONResponse(
                {"error": "the sign-in state does not match", "code": "state_mismatch"},
                status_code=400,
            )

        def runner() -> int:
            return _relay_callback_and_wait(flow, code, state)
    else:

        def runner() -> int:
            return _write_code_and_wait(flow, code)

    proc = flow.get("proc")
    flow["completing"] = True
    try:
        if proc is not None and proc.poll() is not None:
            # Finished on its own (the CLI's localhost callback was reached).
            rc = int(proc.returncode)
        else:
            rc = await asyncio.to_thread(runner)
    except Exception as exc:
        logger.warning("auth complete failed: %s", type(exc).__name__)
        _cleanup_flow(flow)
        flow["proc"] = None
        flow["completing"] = False
        flow["result"] = {
            "status": "denied",
            "code": "auth_complete_failed",
            "error": "Claude did not finish the sign-in. Start again.",
        }
        return JSONResponse(
            {"error": flow["result"]["error"], "code": "auth_complete_failed"},
            status_code=502,
        )

    if rc is None:
        # The CLI asked for the code again: the sign-in stays open.
        flow["completing"] = False
        return JSONResponse(
            {
                "error": _cli_failure_message(flow.get("output_buf", ""), None, code),
                "code": "code_incomplete",
            },
            status_code=400,
        )

    result = _finish_flow(flow, rc, code)
    if result["status"] == "ok":
        return JSONResponse(
            {"ok": True, "profile": flow["profile"], "account": result["account"]}
        )
    return JSONResponse(
        {"error": result["error"], "code": result["code"]}, status_code=400
    )


async def auth_token(request: Request) -> JSONResponse:
    """Add an account from a pasted Claude credential.

    ``claude setup-token`` (run on any computer signed in to the Claude plan)
    prints a long-lived ``sk-ant-oat01-…`` token: turns then run on the
    plan's usage limits. An ``sk-ant-api…`` Console key bills pay-as-you-go.
    The credential stays in this gateway (owner-only file in the account's
    profile); it is never returned or logged.
    """
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error
    try:
        payload = await request.json()
    except Exception:
        return JSONResponse({"error": "invalid JSON body"}, status_code=400)
    if not isinstance(payload, dict):
        return JSONResponse({"error": "JSON body must be an object"}, status_code=400)
    token = payload.get("token")
    token = token.strip() if isinstance(token, str) else ""
    kind = _claude_token_kind(token)
    if kind is None:
        return JSONResponse(
            {
                "error": "Paste the token `claude setup-token` prints (sk-ant-oat01-…) or an Anthropic API key (sk-ant-api…).",
                "code": "invalid_token",
            },
            status_code=400,
        )
    raw_profile = payload.get("profile")
    if raw_profile in (None, ""):
        profile = f"acct_{secrets.token_hex(4)}"
    elif not isinstance(raw_profile, str) or raw_profile == "default":
        return JSONResponse(
            {"error": "a token account needs its own profile", "code": "invalid_profile"},
            status_code=400,
        )
    else:
        try:
            profile = _validate_profile_id(raw_profile)
        except ValueError as exc:
            return JSONResponse(
                {"error": str(exc), "code": "invalid_profile"}, status_code=400
            )
    verify = bool(payload.get("verify", False))
    quota = None
    if verify:
        ok, reject_code, quota = await asyncio.to_thread(_probe_anthropic_token, kind, token)
        if not ok:
            msg = (
                "Anthropic rejected this API key. Check the key and try again."
                if kind == "api_key"
                else "Claude rejected this setup-token. Run `claude setup-token` again and paste the fresh token."
            )
            return JSONResponse(
                {"error": msg, "code": reject_code or "invalid_token"},
                status_code=401,
            )
    profile_name, profile_dir = _resolve_profile_dir(profile, create=True)
    assert profile_dir is not None
    try:
        _store_profile_token(profile_dir, token)
    except (OSError, ValueError) as exc:
        logger.warning("could not store a Claude token: %s", type(exc).__name__)
        return JSONResponse(
            {"error": "could not store the token on the gateway", "code": "store_failed"},
            status_code=500,
        )
    account = await asyncio.to_thread(_read_cli_status, profile_dir) or {}
    account = {**account, "loggedIn": True, "authMethod": kind}
    logger.info("token account stored profile=%s kind=%s", profile_name, kind)
    body: dict[str, Any] = {
        "ok": True,
        "profile": profile_name,
        "account": account,
        "billing": "plan" if kind == "oauth_token" else "api",
    }
    if quota is not None:
        body["quota"] = quota
    return JSONResponse(body)


async def auth_import_cli(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    account = _read_cli_status(None)
    if account is None:
        return JSONResponse(
            {
                "error": "no existing claude CLI login found",
                "code": "not_signed_in",
            },
            status_code=404,
        )
    return JSONResponse({"ok": True, "profile": "default", "account": account})


async def auth_cancel(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    try:
        payload = await request.json()
    except Exception:
        return JSONResponse({"error": "invalid JSON body"}, status_code=400)
    if not isinstance(payload, dict):
        return JSONResponse({"error": "JSON body must be an object"}, status_code=400)

    flow_id = str(payload.get("flow_id") or "")
    if flow_id:
        flow = _pending_auth_flows.pop(flow_id, None)
        if flow is not None:
            _cleanup_flow(flow)
    return JSONResponse({"ok": True})


async def auth_usage(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    raw_profile = request.query_params.get("profile")
    try:
        profile_key, profile_dir = _resolve_profile_dir(raw_profile, create=False)
    except ValueError as exc:
        return JSONResponse(
            {"error": str(exc), "code": "invalid_profile"}, status_code=400
        )

    token = _read_oauth_access_token(profile_dir)
    if token is not None:
        snapshot = await asyncio.to_thread(_fetch_oauth_usage, token)
        if snapshot is not None:
            return JSONResponse({"available": True, "quota": snapshot})

    cached = _last_rate_limits.get(profile_key)
    if cached is not None:
        snapshot = _quota_snapshot_from_rate_limits(
            cached["info"], sampled_at=cached.get("sampled_at")
        )
        if snapshot is not None:
            return JSONResponse({"available": True, "quota": snapshot})

    return JSONResponse({"available": False, "quota": None})


def _probe_anthropic_token(kind: str, token: str) -> tuple[bool, str | None, dict[str, Any] | None]:
    """Verify an Anthropic OAuth token or Console API key against Anthropic.

    Returns ``(ok, reject_code, quota_snapshot)``.
    - ``ok=True`` when Anthropic accepts the credential (HTTP 200) or when the
      probe fails only due to transient network/unreachable errors (so offline
      or rate-limited checks don't falsely mark a live session broken).
    - ``ok=False`` with ``reject_code`` (``"needs_reauth"`` or
      ``"credentials_rejected"``) when Anthropic explicitly returns 401/403.
    """
    opener = urllib.request.build_opener(_NoRedirect)
    if kind == "api_key":
        req = urllib.request.Request(
            "https://api.anthropic.com/v1/models",
            method="GET",
            headers={
                "x-api-key": token,
                "anthropic-version": "2023-06-01",
                "Accept": "application/json",
            },
        )
        try:
            with opener.open(req, timeout=USAGE_TIMEOUT_SECS) as resp:
                if getattr(resp, "status", 200) in (401, 403):
                    return (False, "credentials_rejected", None)
                return (True, None, None)
        except urllib.error.HTTPError as exc:
            if exc.code in (401, 403):
                return (False, "credentials_rejected", None)
            return (True, None, None)
        except (urllib.error.URLError, OSError, TimeoutError, ValueError):
            return (True, None, None)

    req = urllib.request.Request(
        ANTHROPIC_OAUTH_USAGE_URL,
        method="GET",
        headers={
            "Authorization": f"Bearer {token}",
            "anthropic-beta": "oauth-2025-04-20",
            "User-Agent": "claude-code/2.0.0",
            "Accept": "application/json",
        },
    )
    try:
        with opener.open(req, timeout=USAGE_TIMEOUT_SECS) as resp:
            raw = resp.read(USAGE_MAX_BODY_BYTES + 1)
    except urllib.error.HTTPError as exc:
        if exc.code in (401, 403):
            return (False, "needs_reauth", None)
        return (True, None, None)
    except (urllib.error.URLError, OSError, TimeoutError, ValueError):
        return (True, None, None)

    if len(raw) > USAGE_MAX_BODY_BYTES:
        return (True, None, None)
    try:
        data = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return (True, None, None)
    snapshot = _parse_oauth_usage_snapshot(data) if isinstance(data, dict) else None
    return (True, None, snapshot)


async def auth_verify(request: Request) -> JSONResponse:
    """Verify a stored profile's Claude subscription or API key against Anthropic."""
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error

    raw_profile: str | None = request.headers.get("x-claude-profile")
    if raw_profile is None:
        try:
            body = await request.json()
            if isinstance(body, dict) and isinstance(body.get("profile"), str):
                raw_profile = body["profile"]
        except Exception:
            pass
    try:
        profile_key, profile_dir = _resolve_profile_dir(raw_profile, create=False)
    except ValueError as exc:
        return JSONResponse(
            {"ok": False, "error": str(exc), "code": "invalid_profile"},
            status_code=400,
        )

    stored = _read_profile_token(profile_dir)
    if stored is not None:
        kind, token = stored
        ok, reject_code, quota = await asyncio.to_thread(
            _probe_anthropic_token, kind, token
        )
        if not ok:
            msg = (
                "Anthropic rejected this API key."
                if kind == "api_key"
                else "Claude rejected this setup-token. Run `claude setup-token` again or sign in with your browser."
            )
            return JSONResponse(
                {"ok": False, "profile": profile_key, "code": reject_code, "error": msg},
                status_code=401,
            )
        return JSONResponse(
            {
                "ok": True,
                "profile": profile_key,
                "mode": kind,
                "billing": "api" if kind == "api_key" else "plan",
                "quota": quota,
                "model_count": len(SUPPORTED_MODELS),
            }
        )

    account = await asyncio.to_thread(_read_cli_status, profile_dir)
    if account is None:
        return JSONResponse(
            {
                "ok": False,
                "profile": profile_key,
                "code": "not_signed_in",
                "error": "This Claude profile is not signed in yet.",
            },
            status_code=401,
        )

    oauth_token = _read_oauth_access_token(profile_dir)
    quota = None
    if oauth_token is not None:
        ok, reject_code, quota = await asyncio.to_thread(
            _probe_anthropic_token, "oauth_token", oauth_token
        )
        if not ok:
            return JSONResponse(
                {
                    "ok": False,
                    "profile": profile_key,
                    "code": reject_code or "needs_reauth",
                    "error": "Claude session expired. Sign in again.",
                },
                status_code=401,
            )
    return JSONResponse(
        {
            "ok": True,
            "profile": profile_key,
            "mode": "cli_login",
            "billing": "plan",
            "account": account,
            "quota": quota,
            "model_count": len(SUPPORTED_MODELS),
        }
    )


@asynccontextmanager
async def lifespan(_app: Starlette):
    env_host = os.environ.get(HOST_ENV_VAR, DEFAULT_HOST)
    validate_loopback_host(env_host)
    try:
        yield
    finally:
        for fid in list(_pending_auth_flows):
            fl = _pending_auth_flows.pop(fid, None)
            if fl is not None:
                _cleanup_flow(fl)


app = Starlette(
    routes=[
        Route("/health", health, methods=["GET"]),
        Route("/v1/models", list_models, methods=["GET"]),
        Route("/v1/turn", turn, methods=["POST"]),
        Route("/v1/auth/status", auth_status, methods=["GET"]),
        Route("/v1/auth/start", auth_start, methods=["POST"]),
        Route("/v1/auth/flow/{flow_id}", auth_flow_status, methods=["GET"]),
        Route("/v1/auth/complete", auth_complete, methods=["POST"]),
        Route("/v1/auth/import-cli", auth_import_cli, methods=["POST"]),
        Route("/v1/auth/token", auth_token, methods=["POST"]),
        Route("/v1/auth/verify", auth_verify, methods=["POST"]),
        Route("/v1/auth/cancel", auth_cancel, methods=["POST"]),
        Route("/v1/auth/usage", auth_usage, methods=["GET"]),
    ],
    lifespan=lifespan,
)


def main(argv: list[str] | None = None) -> None:
    import uvicorn

    parser = argparse.ArgumentParser(
        description="SeraBot opt-in loopback claude-agent-sdk gateway"
    )
    parser.add_argument(
        "--host",
        default=os.environ.get(HOST_ENV_VAR, DEFAULT_HOST),
        help="Loopback bind host (127.0.0.1 or ::1 only)",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=DEFAULT_PORT,
        help="Loopback bind port (default: 8791)",
    )
    args = parser.parse_args(argv)

    validated_host = validate_loopback_host(args.host)
    os.environ[HOST_ENV_VAR] = validated_host
    logger.info("starting serabot-claude-gateway on %s:%d", validated_host, args.port)
    uvicorn.run(app, host=validated_host, port=args.port, log_level="info")


if __name__ == "__main__":
    main()
