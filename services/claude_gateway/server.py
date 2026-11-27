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
from contextlib import asynccontextmanager
import hmac
import ipaddress
import json
import logging
import os
import time
from pathlib import Path
from typing import Any
import uuid

from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import JSONResponse
from starlette.routing import Route

logger = logging.getLogger("serabot.claude_gateway")

TOKEN_ENV_VAR = "SERABOT_CLAUDE_GATEWAY_TOKEN"
HOST_ENV_VAR = "SERABOT_CLAUDE_GATEWAY_HOST"
DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 8791
DEFAULT_MODEL = "claude-sonnet-4-5"

# Browser sign-in ("Sign in with browser" on the provider card) drives the
# SAME first-party OAuth flow `claude auth login` uses: Anthropic's own CLI
# client id against claude.ai. This is not the third-party claude.ai OAuth
# that Anthropic shut down in 2026 — that one SeraBot retired in 0.74.25.
# The gateway is the only component that ever sees these URLs/tokens; it
# stores the result where the Claude CLI/SDK reads its login
# (~/.claude/.credentials.json by default), mode 0600.
CLAUDE_CLI_CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5c5e196af2c3"
CLAUDE_OAUTH_AUTHORIZE_URL = "https://claude.ai/oauth/authorize"
CLAUDE_OAUTH_TOKEN_URL = "https://api.anthropic.com/api/oauth/token"
CLAUDE_OAUTH_SCOPES = "user:profile user:inference"
AUTH_FLOW_TTL_SECS = 600
MAX_AUTH_BODY_BYTES = 64 * 1024
CREDS_PATH_ENV_VAR = "SERABOT_CLAUDE_GATEWAY_CREDS_PATH"
DEFAULT_CREDS_PATH = os.path.expanduser("~/.claude/.credentials.json")

# Pending browser sign-ins: state -> {verifier, redirect_uri, expires}.
# Verifiers never leave this process.
_pending_auth_flows: dict[str, dict[str, Any]] = {}

SUPPORTED_MODELS: list[dict[str, str]] = [
    {"id": "claude-sonnet-4-5"},
    {"id": "claude-opus-4-5"},
    {"id": "claude-haiku-4-5"},
    {"id": "claude-sonnet-4-6"},
    {"id": "claude-opus-4-6"},
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


def _import_sdk() -> Any:
    import claude_agent_sdk  # type: ignore[import-not-found]

    return claude_agent_sdk


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


async def _run_turn(payload: dict[str, Any]) -> dict[str, Any]:
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

    options = sdk.ClaudeAgentOptions(
        model=model,
        system_prompt=system_prompt,
        tools=[],
        setting_sources=[],
        permission_mode="default",
        mcp_servers=mcp_servers,
        allowed_tools=allowed_tools,
        max_turns=2 if sdk_tools else 1,
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


async def health(request: Request) -> JSONResponse:
    return JSONResponse(
        {
            "status": "ok",
            "sdk": "claude-agent-sdk",
            "auth_configured": bool(os.environ.get(TOKEN_ENV_VAR)),
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

    try:
        result = await _run_turn(payload)
    except ValueError as exc:
        return JSONResponse({"error": str(exc)}, status_code=400)

    logger.info(
        "turn completed model=%s finish_reason=%s tool_calls=%d",
        payload.get("model") or DEFAULT_MODEL,
        result["finish_reason"],
        len(result["tool_calls"]),
    )
    return JSONResponse(result)


def _validate_loopback_redirect_uri(uri: str) -> str:
    """The browser redirect must be an http loopback URL on an unprivileged
    port with no credentials, query or fragment (the member's desktop runs
    the loopback listener)."""
    from urllib.parse import urlparse

    parsed = urlparse(uri)
    host = (parsed.hostname or "").strip("[]")
    if (
        parsed.scheme != "http"
        or host not in ("127.0.0.1", "localhost", "::1")
        or parsed.port is None
        or parsed.port < 1024
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
    ):
        raise ValueError("redirect_uri must be an http loopback URL (port >= 1024)")
    return uri


def _new_pkce() -> tuple[str, str]:
    import base64
    import hashlib
    import secrets

    verifier = secrets.token_urlsafe(64)
    digest = hashlib.sha256(verifier.encode("ascii")).digest()
    challenge = base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")
    return verifier, challenge


def _exchange_code(code: str, redirect_uri: str, verifier: str) -> dict[str, Any]:
    """Exchange the one-time code at the pinned token URL. Redirects are
    refused, the body is capped, and only non-secret account fields return."""
    import urllib.error
    import urllib.parse
    import urllib.request

    form = urllib.parse.urlencode(
        {
            "grant_type": "authorization_code",
            "client_id": CLAUDE_CLI_CLIENT_ID,
            "code": code,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
        }
    ).encode("utf-8")

    class _NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: D102
            return None

    request = urllib.request.Request(
        CLAUDE_OAUTH_TOKEN_URL,
        data=form,
        headers={"Content-Type": "application/x-www-form-urlencoded"},
        method="POST",
    )
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        with opener.open(request, timeout=30) as response:
            body = response.read(MAX_AUTH_BODY_BYTES + 1)
    except urllib.error.HTTPError as exc:
        raise ValueError(f"token exchange refused with HTTP {exc.code}") from exc
    except urllib.error.URLError as exc:
        raise ValueError("could not reach the token endpoint") from exc
    if len(body) > MAX_AUTH_BODY_BYTES:
        raise ValueError("token response too large")
    try:
        payload = json.loads(body)
    except ValueError as exc:
        raise ValueError("token response was not JSON") from exc
    if not isinstance(payload, dict) or not payload.get("access_token"):
        raise ValueError("token response had no access token")
    return payload


def _write_credentials(payload: dict[str, Any]) -> dict[str, Any]:
    """Write the OAuth tokens where the Claude CLI/SDK reads its login
    (``~/.claude/.credentials.json`` by default), mode 0600. Returns the
    non-secret account summary."""
    creds_path = os.environ.get(CREDS_PATH_ENV_VAR, DEFAULT_CREDS_PATH)
    account = payload.get("account") if isinstance(payload.get("account"), dict) else {}
    summary = {
        "emailAddress": account.get("emailAddress"),
        "accountId": account.get("accountId"),
        "subscriptionType": account.get("subscriptionType"),
    }
    document = {
        "claudeAiOauth": {
            "accessToken": payload.get("access_token"),
            "refreshToken": payload.get("refresh_token"),
            "expiresAt": payload.get("expires_at"),
            "scopes": payload.get("scope", "").split() if payload.get("scope") else [],
            "subscriptionType": account.get("subscriptionType"),
            "account": summary,
        }
    }
    path = Path(creds_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(document, handle)
    os.chmod(path, 0o600)
    return summary


def _read_credentials() -> dict[str, Any] | None:
    creds_path = os.environ.get(CREDS_PATH_ENV_VAR, DEFAULT_CREDS_PATH)
    try:
        with open(creds_path, "r", encoding="utf-8") as handle:
            document = json.load(handle)
    except (OSError, ValueError):
        return None
    oauth = document.get("claudeAiOauth") if isinstance(document, dict) else None
    if not isinstance(oauth, dict) or not oauth.get("accessToken"):
        return None
    account = oauth.get("account") if isinstance(oauth.get("account"), dict) else {}
    return {
        "emailAddress": account.get("emailAddress"),
        "subscriptionType": account.get("subscriptionType"),
    }


async def auth_start(request: Request) -> JSONResponse:
    """Begin the first-party browser sign-in: build the claude.ai authorize
    URL (PKCE S256, verifier stays in this process) for the member's desktop
    loopback redirect."""
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error
    try:
        payload = await request.json()
    except Exception:
        return JSONResponse({"error": "invalid JSON body"}, status_code=400)
    if not isinstance(payload, dict):
        return JSONResponse({"error": "JSON body must be an object"}, status_code=400)
    try:
        redirect_uri = _validate_loopback_redirect_uri(str(payload.get("redirect_uri") or ""))
    except ValueError as exc:
        return JSONResponse({"error": str(exc)}, status_code=400)
    state = payload.get("state")
    if state is not None:
        state = str(state)
        if not 16 <= len(state) <= 128 or not all(
            c.isalnum() or c in "_-" for c in state
        ):
            return JSONResponse({"error": "invalid state"}, status_code=400)
    else:
        import secrets

        state = secrets.token_urlsafe(24)
    verifier, challenge = _new_pkce()
    expires = time.time() + AUTH_FLOW_TTL_SECS
    _pending_auth_flows[state] = {
        "verifier": verifier,
        "redirect_uri": redirect_uri,
        "expires": expires,
    }
    from urllib.parse import urlencode

    url = CLAUDE_OAUTH_AUTHORIZE_URL + "?" + urlencode(
        {
            "client_id": CLAUDE_CLI_CLIENT_ID,
            "response_type": "code",
            "redirect_uri": redirect_uri,
            "scope": CLAUDE_OAUTH_SCOPES,
            "code_challenge": challenge,
            "code_challenge_method": "S256",
            "state": state,
        }
    )
    return JSONResponse({"url": url, "state": state, "expires_in": AUTH_FLOW_TTL_SECS})


async def auth_complete(request: Request) -> JSONResponse:
    """Finish the browser sign-in: swap the code at the pinned token endpoint
    and store the login where the SDK reads it."""
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error
    try:
        payload = await request.json()
    except Exception:
        return JSONResponse({"error": "invalid JSON body"}, status_code=400)
    if not isinstance(payload, dict):
        return JSONResponse({"error": "JSON body must be an object"}, status_code=400)
    state = str(payload.get("state") or "")
    code = str(payload.get("code") or "")
    if not state or not code:
        return JSONResponse({"error": "state and code are required"}, status_code=400)
    pending = _pending_auth_flows.pop(state, None)
    if pending is None:
        return JSONResponse({"error": "unknown or used sign-in state"}, status_code=404)
    if time.time() >= pending["expires"]:
        return JSONResponse({"error": "sign-in expired; start again"}, status_code=410)
    try:
        tokens = _exchange_code(code, pending["redirect_uri"], pending["verifier"])
    except ValueError as exc:
        logger.warning("auth complete failed: %s", exc)
        return JSONResponse({"error": "token exchange failed"}, status_code=502)
    account = _write_credentials(tokens)
    logger.info("browser sign-in stored credentials for a Claude account")
    return JSONResponse({"ok": True, "account": account})


async def auth_status(request: Request) -> JSONResponse:
    auth_error = _check_bearer_auth(request)
    if auth_error is not None:
        return auth_error
    account = _read_credentials()
    return JSONResponse({"signed_in": account is not None, "account": account})


@asynccontextmanager
async def lifespan(_app: Starlette):
    env_host = os.environ.get(HOST_ENV_VAR, DEFAULT_HOST)
    validate_loopback_host(env_host)
    yield


app = Starlette(
    routes=[
        Route("/health", health, methods=["GET"]),
        Route("/v1/models", list_models, methods=["GET"]),
        Route("/v1/turn", turn, methods=["POST"]),
        Route("/v1/auth/start", auth_start, methods=["POST"]),
        Route("/v1/auth/complete", auth_complete, methods=["POST"]),
        Route("/v1/auth/status", auth_status, methods=["GET"]),
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
