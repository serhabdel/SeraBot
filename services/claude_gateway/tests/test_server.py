from __future__ import annotations

from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

GATEWAY_DIR = Path(__file__).resolve().parents[1]
if str(GATEWAY_DIR) not in sys.path:
    sys.path.insert(0, str(GATEWAY_DIR))

import server  # noqa: E402


async def _asgi_request(
    method: str,
    path: str,
    *,
    headers: dict[str, str] | None = None,
    json_body: dict | None = None,
) -> tuple[int, dict]:
    raw_headers = [
        (k.lower().encode("latin-1"), v.encode("latin-1"))
        for k, v in (headers or {}).items()
    ]
    body_bytes = b""
    if json_body is not None:
        body_bytes = json.dumps(json_body).encode("utf-8")
        raw_headers.append((b"content-type", b"application/json"))

    scope = {
        "type": "http",
        "asgi": {"version": "3.0"},
        "http_version": "1.1",
        "method": method,
        "scheme": "http",
        "path": path,
        "raw_path": path.encode("ascii"),
        "query_string": b"",
        "headers": raw_headers,
        "client": ("127.0.0.1", 50000),
        "server": ("127.0.0.1", 8791),
    }

    sent_body = False

    async def receive() -> dict:
        nonlocal sent_body
        if not sent_body:
            sent_body = True
            return {"type": "http.request", "body": body_bytes, "more_body": False}
        return {"type": "http.disconnect"}

    status_code = 500
    response_chunks: list[bytes] = []

    async def send(message: dict) -> None:
        nonlocal status_code
        if message["type"] == "http.response.start":
            status_code = message["status"]
        elif message["type"] == "http.response.body":
            response_chunks.append(message.get("body", b""))

    await server.app(scope, receive, send)
    payload = json.loads(b"".join(response_chunks).decode("utf-8"))
    return status_code, payload


@dataclass
class _FakeClaudeAgentOptions:
    model: str = ""
    system_prompt: str = ""
    tools: list = field(default_factory=list)
    setting_sources: list = field(default_factory=list)
    permission_mode: str = "default"
    mcp_servers: dict = field(default_factory=dict)
    allowed_tools: list = field(default_factory=list)
    max_turns: int = 1
    env: dict = field(default_factory=dict)
    effort: str | None = None


@dataclass
class _LegacyClaudeAgentOptions:
    model: str = ""
    system_prompt: str = ""
    tools: list = field(default_factory=list)
    setting_sources: list = field(default_factory=list)
    permission_mode: str = "default"
    mcp_servers: dict = field(default_factory=dict)
    allowed_tools: list = field(default_factory=list)
    max_turns: int = 1


@dataclass
class _FakeTextBlock:
    text: str


@dataclass
class _FakeToolUseBlock:
    id: str
    name: str
    input: dict


@dataclass
class _FakeAssistantMessage:
    content: list


@dataclass
class _FakeResultMessage:
    subtype: str = "success"
    usage: dict = field(default_factory=dict)
    result: str | None = None


def _build_mock_sdk(query_impl, options_cls=_FakeClaudeAgentOptions):
    created_servers: list[dict] = []

    def tool_decorator(name: str, description: str, schema: dict):
        def _wrap(fn):
            fn._tool_meta = {
                "name": name,
                "description": description,
                "schema": schema,
            }
            return fn

        return _wrap

    def create_sdk_mcp_server(*, name: str, version: str = "1.0.0", tools=None):
        srv = {"name": name, "version": version, "tools": list(tools or [])}
        created_servers.append(srv)
        return srv

    sdk = SimpleNamespace(
        ClaudeAgentOptions=options_cls,
        AssistantMessage=_FakeAssistantMessage,
        ResultMessage=_FakeResultMessage,
        TextBlock=_FakeTextBlock,
        ToolUseBlock=_FakeToolUseBlock,
        tool=tool_decorator,
        create_sdk_mcp_server=create_sdk_mcp_server,
        query=query_impl,
        _created_servers=created_servers,
    )
    return sdk


def _write_mock_claude_cli(directory: Path) -> Path:
    script = directory / "claude"
    script.write_text(
        """#!/usr/bin/env python3
import json
import os
import sys
import time
from pathlib import Path

args = sys.argv[1:]
config_dir = Path(os.environ.get("CLAUDE_CONFIG_DIR") or (Path.home() / ".claude"))
state_file = config_dir / "mock_login.json"

if args == ["--version"]:
    print("2.1.0 (Claude Code)")
    sys.exit(0)

if args[:3] == ["auth", "status", "--json"]:
    if state_file.exists():
        print(state_file.read_text(encoding="utf-8"))
    else:
        print(json.dumps({"loggedIn": False}))
    sys.exit(0)

if args[:2] == ["auth", "login"]:
    mode = os.environ.get("MOCK_LOGIN_MODE", "interactive")
    print("\\x1b[36mOpen this URL:\\x1b[0m https://claude.ai/oauth/authorize?client_id=test&state=abc123", flush=True)
    if mode == "fail":
        print("Authentication denied by user", flush=True)
        sys.exit(1)
    if mode == "auto":
        time.sleep(0.15)
        config_dir.mkdir(parents=True, exist_ok=True)
        state_file.write_text(json.dumps({
            "loggedIn": True,
            "authMethod": "claudeai",
            "email": "auto@example.com",
            "orgId": "org_1",
            "orgName": "Auto Org",
            "subscriptionType": "max",
        }), encoding="utf-8")
        sys.exit(0)
    code = sys.stdin.readline().strip()
    if code == "bad-code":
        print("Invalid code", flush=True)
        sys.exit(1)
    config_dir.mkdir(parents=True, exist_ok=True)
    state_file.write_text(json.dumps({
        "loggedIn": True,
        "authMethod": "claudeai",
        "email": f"{code}@example.com",
        "orgId": "org_1",
        "orgName": "Test Org",
        "subscriptionType": "pro",
    }), encoding="utf-8")
    sys.exit(0)

sys.exit(2)
""",
        encoding="utf-8",
    )
    os.chmod(script, 0o755)
    return script


class TestClaudeGateway(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        self._orig_token = os.environ.get(server.TOKEN_ENV_VAR)
        self._orig_host = os.environ.get(server.HOST_ENV_VAR)
        self._orig_state_dir = os.environ.get(server.STATE_DIR_ENV_VAR)
        self._orig_cli_path = os.environ.get(server.CLI_PATH_ENV_VAR)
        os.environ[server.TOKEN_ENV_VAR] = "test-secret-token-123"
        os.environ.pop(server.HOST_ENV_VAR, None)
        self._tmp_dir = tempfile.TemporaryDirectory()
        os.environ[server.STATE_DIR_ENV_VAR] = self._tmp_dir.name
        os.environ[server.CLI_PATH_ENV_VAR] = ""
        server._pending_auth_flows.clear()

    def tearDown(self) -> None:
        for fid in list(server._pending_auth_flows):
            fl = server._pending_auth_flows.pop(fid, None)
            if fl is not None:
                server._cleanup_flow(fl)
        self._tmp_dir.cleanup()

        for env_var, orig in (
            (server.TOKEN_ENV_VAR, self._orig_token),
            (server.HOST_ENV_VAR, self._orig_host),
            (server.STATE_DIR_ENV_VAR, self._orig_state_dir),
            (server.CLI_PATH_ENV_VAR, self._orig_cli_path),
        ):
            if orig is None:
                os.environ.pop(env_var, None)
            else:
                os.environ[env_var] = orig

    def test_loopback_host_validation(self) -> None:
        self.assertEqual(server.validate_loopback_host("127.0.0.1"), "127.0.0.1")
        self.assertEqual(server.validate_loopback_host("::1"), "::1")
        self.assertEqual(server.validate_loopback_host("[::1]"), "::1")

        for bad_host in ("0.0.0.0", "::", "192.168.1.50", "example.com", ""):
            with self.assertRaises(ValueError):
                server.validate_loopback_host(bad_host)

        with self.assertRaises(ValueError):
            server.main(["--host", "0.0.0.0", "--port", "8791"])

    def test_clean_claude_env_strips_nested_vars_and_sets_client_app(self) -> None:
        dirty = {
            "PATH": "/usr/bin",
            "CLAUDECODE": "1",
            "CLAUDE_CODE_ENTRYPOINT": "cli",
            "CLAUDE_CODE_SESSION_ID": "sess_1",
            "CLAUDE_CODE_SESSION_ACCESS_TOKEN": "tok",
            "CLAUDE_EFFORT": "high",
            "CLAUDE_PID": "999",
            "CLAUDE_BG_TASK": "1",
            "CLAUDE_CODE_HOST_FOO": "drop",
            "CLAUDE_CODE_HOST_AUTH_ENV_VAR": "keep_me",
            "CLAUDE_CODE_HOST_AUTH_REFRESH_TIMEOUT_MS": "5000",
            "CLAUDE_CODE_HOST_CREDS_FILE": "/tmp/creds",
            "AI_AGENT": "claude-code-2",
            "ANTHROPIC_API_KEY": "sk-ant-123",
            "CLAUDE_CONFIG_DIR": "/some/other/dir",
        }
        with patch.dict(os.environ, dirty, clear=True):
            env_default = server._clean_claude_env(None, strip_api_key=False)
            self.assertNotIn("CLAUDECODE", env_default)
            self.assertNotIn("CLAUDE_CODE_ENTRYPOINT", env_default)
            self.assertNotIn("CLAUDE_CODE_SESSION_ID", env_default)
            self.assertNotIn("CLAUDE_CODE_SESSION_ACCESS_TOKEN", env_default)
            self.assertNotIn("CLAUDE_EFFORT", env_default)
            self.assertNotIn("CLAUDE_PID", env_default)
            self.assertNotIn("CLAUDE_BG_TASK", env_default)
            self.assertNotIn("CLAUDE_CODE_HOST_FOO", env_default)
            self.assertNotIn("AI_AGENT", env_default)
            self.assertNotIn("CLAUDE_CONFIG_DIR", env_default)
            self.assertEqual(env_default["CLAUDE_CODE_HOST_AUTH_ENV_VAR"], "keep_me")
            self.assertEqual(
                env_default["CLAUDE_CODE_HOST_AUTH_REFRESH_TIMEOUT_MS"], "5000"
            )
            self.assertEqual(env_default["CLAUDE_CODE_HOST_CREDS_FILE"], "/tmp/creds")
            self.assertEqual(
                env_default["CLAUDE_AGENT_SDK_CLIENT_APP"],
                f"serabot-gateway/{server.GATEWAY_VERSION}",
            )
            self.assertEqual(env_default["DISABLE_AUTOUPDATER"], "1")
            self.assertEqual(env_default["ANTHROPIC_API_KEY"], "sk-ant-123")

            profile_path = Path(self._tmp_dir.name) / "profiles" / "acct_1"
            env_profile = server._clean_claude_env(profile_path, strip_api_key=True)
            self.assertEqual(env_profile["CLAUDE_CONFIG_DIR"], str(profile_path))
            self.assertNotIn("ANTHROPIC_API_KEY", env_profile)

        with patch.dict(os.environ, {"AI_AGENT": "serabot"}, clear=True):
            env_other = server._clean_claude_env(None)
            self.assertEqual(env_other["AI_AGENT"], "serabot")

    def test_profile_validation_and_directory_creation(self) -> None:
        for bad in ("", "..", "../etc", "a/b", "Upper", "-dash", "_under", "a" * 49):
            with self.assertRaises(ValueError):
                server._validate_profile_id(bad)

        name, pdir = server._resolve_profile_dir("default", create=True)
        self.assertEqual(name, "default")
        self.assertIsNone(pdir)

        name, pdir = server._resolve_profile_dir("acct_01-ab", create=True)
        self.assertEqual(name, "acct_01-ab")
        self.assertIsNotNone(pdir)
        assert pdir is not None
        self.assertTrue(pdir.is_dir())
        if os.name != "nt":
            self.assertEqual(stat.S_IMODE(pdir.stat().st_mode), 0o700)

    def test_read_cli_status_fallback_credentials(self) -> None:
        profile_dir = Path(self._tmp_dir.name) / "profiles" / "fallback1"
        profile_dir.mkdir(parents=True)
        (profile_dir / ".credentials.json").write_text(
            json.dumps(
                {
                    "claudeAiOauth": {
                        "accessToken": "tok-123",
                        "subscriptionType": "pro",
                        "account": {"emailAddress": "fb@example.com"},
                    }
                }
            ),
            encoding="utf-8",
        )
        acct = server._read_cli_status(profile_dir)
        self.assertIsNotNone(acct)
        assert acct is not None
        self.assertTrue(acct["loggedIn"])
        self.assertEqual(acct["email"], "fb@example.com")
        self.assertEqual(acct["subscriptionType"], "pro")

    async def test_health_and_models(self) -> None:
        mock_sdk = _build_mock_sdk(lambda **_: None)
        with patch.dict(sys.modules, {"claude_agent_sdk": mock_sdk}):
            status, body = await _asgi_request("GET", "/health")
        self.assertEqual(status, 200)
        self.assertEqual(
            body,
            {
                "status": "ok",
                "sdk": "claude-agent-sdk",
                "version": server.GATEWAY_VERSION,
                "auth_configured": True,
                "cli_installed": False,
                "sdk_installed": True,
            },
        )

        status, body = await _asgi_request(
            "GET",
            "/v1/models",
            headers={"Authorization": "Bearer test-secret-token-123"},
        )
        self.assertEqual(status, 200)
        model_ids = [m["id"] for m in body["data"]]
        self.assertEqual(
            model_ids,
            [
                "claude-sonnet-4-5",
                "claude-opus-4-5",
                "claude-haiku-4-5",
                "claude-sonnet-4-6",
                "claude-opus-4-6",
            ],
        )

    async def test_auth_missing_wrong_and_unconfigured(self) -> None:
        status, _ = await _asgi_request("GET", "/v1/models")
        self.assertEqual(status, 401)

        status, _ = await _asgi_request(
            "POST",
            "/v1/turn",
            headers={"Authorization": "Bearer wrong-token"},
            json_body={"messages": [{"role": "user", "content": "hi"}]},
        )
        self.assertEqual(status, 401)

        os.environ.pop(server.TOKEN_ENV_VAR, None)
        status, health_body = await _asgi_request("GET", "/health")
        self.assertEqual(status, 200)
        self.assertFalse(health_body["auth_configured"])

        status, _ = await _asgi_request(
            "GET",
            "/v1/models",
            headers={"Authorization": "Bearer anything"},
        )
        self.assertEqual(status, 503)

    async def test_turn_text_only_enforces_isolated_options_and_effort(self) -> None:
        seen_options: list[_FakeClaudeAgentOptions] = []

        async def fake_query(*, prompt: str, options: _FakeClaudeAgentOptions):
            seen_options.append(options)
            yield _FakeAssistantMessage(
                content=[_FakeTextBlock(text="Hello from Claude")]
            )
            yield _FakeResultMessage(
                subtype="success",
                usage={"input_tokens": 12, "output_tokens": 5},
            )

        mock_sdk = _build_mock_sdk(fake_query)
        with patch.dict(sys.modules, {"claude_agent_sdk": mock_sdk}):
            status, body = await _asgi_request(
                "POST",
                "/v1/turn",
                headers={
                    "Authorization": "Bearer test-secret-token-123",
                    "X-Claude-Profile": "acct_work",
                },
                json_body={
                    "model": "claude-sonnet-4-6",
                    "system": "Be concise.",
                    "messages": [{"role": "user", "content": "Say hi"}],
                    "tools": [],
                    "effort": "high",
                },
            )

        self.assertEqual(status, 200)
        self.assertEqual(body["content"], "Hello from Claude")
        self.assertEqual(body["tool_calls"], [])
        self.assertEqual(body["usage"], {"input_tokens": 12, "output_tokens": 5})
        self.assertEqual(body["finish_reason"], "stop")

        self.assertEqual(len(seen_options), 1)
        opts = seen_options[0]
        self.assertEqual(opts.model, "claude-sonnet-4-6")
        self.assertEqual(opts.system_prompt, "Be concise.")
        self.assertEqual(opts.tools, [])
        self.assertEqual(opts.setting_sources, [])
        self.assertEqual(opts.permission_mode, "default")
        self.assertEqual(opts.effort, "high")
        self.assertEqual(
            opts.env["CLAUDE_CONFIG_DIR"],
            str(Path(self._tmp_dir.name) / "profiles" / "acct_work"),
        )

    async def test_turn_falls_back_when_options_lack_env_and_effort(self) -> None:
        seen_options: list[_LegacyClaudeAgentOptions] = []

        async def fake_query(*, prompt: str, options: _LegacyClaudeAgentOptions):
            seen_options.append(options)
            yield _FakeAssistantMessage(content=[_FakeTextBlock(text="Legacy ok")])

        mock_sdk = _build_mock_sdk(fake_query, options_cls=_LegacyClaudeAgentOptions)
        with patch.dict(sys.modules, {"claude_agent_sdk": mock_sdk}):
            status, body = await _asgi_request(
                "POST",
                "/v1/turn",
                headers={"Authorization": "Bearer test-secret-token-123"},
                json_body={
                    "messages": [{"role": "user", "content": "hi"}],
                    "effort": "low",
                },
            )

        self.assertEqual(status, 200)
        self.assertEqual(body["content"], "Legacy ok")
        self.assertEqual(len(seen_options), 1)

    async def test_turn_rejects_invalid_profile_and_effort_and_classifies_errors(
        self,
    ) -> None:
        headers = {"Authorization": "Bearer test-secret-token-123"}

        status, body = await _asgi_request(
            "POST",
            "/v1/turn",
            headers=headers,
            json_body={
                "profile": "../bad",
                "messages": [{"role": "user", "content": "hi"}],
            },
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "invalid_profile")

        status, body = await _asgi_request(
            "POST",
            "/v1/turn",
            headers=headers,
            json_body={
                "effort": "ultra",
                "messages": [{"role": "user", "content": "hi"}],
            },
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "invalid_effort")

        cases = [
            (RuntimeError("Unauthorized: invalid api key sk-ant-secret123"), 401, "auth_failed"),
            (RuntimeError("429 rate limit exceeded"), 429, "rate_limited"),
            (RuntimeError("402 insufficient credit balance"), 402, "billing_failed"),
            (RuntimeError("404 model not found: claude-x"), 404, "model_not_found"),
            (FileNotFoundError("claude not found"), 503, "cli_missing"),
        ]
        for exc, expected_status, expected_code in cases:
            def _make_failing_query(err):
                async def failing_query(*, prompt: str, options):
                    raise err
                    yield  # pragma: no cover

                return failing_query

            mock_sdk = _build_mock_sdk(_make_failing_query(exc))
            with patch.dict(sys.modules, {"claude_agent_sdk": mock_sdk}):
                status, body = await _asgi_request(
                    "POST",
                    "/v1/turn",
                    headers=headers,
                    json_body={"messages": [{"role": "user", "content": "hi"}]},
                )
            self.assertEqual(status, expected_status)
            self.assertEqual(body["code"], expected_code)
            self.assertNotIn("sk-ant-secret123", json.dumps(body))

    async def test_turn_captures_mcp_tool_calls(self) -> None:
        seen_options: list[_FakeClaudeAgentOptions] = []

        async def fake_query(*, prompt: str, options: _FakeClaudeAgentOptions):
            seen_options.append(options)
            mcp_srv = options.mcp_servers["serabot"]
            await mcp_srv["tools"][0]({"city": "Casablanca"})
            yield _FakeAssistantMessage(
                content=[
                    _FakeTextBlock(text="Checking weather."),
                    _FakeToolUseBlock(
                        id="toolu_01abc",
                        name="mcp__serabot__get_weather",
                        input={"city": "Casablanca"},
                    ),
                ]
            )
            yield _FakeResultMessage(
                subtype="success",
                usage={"input_tokens": 30, "output_tokens": 15},
            )

        mock_sdk = _build_mock_sdk(fake_query)
        with patch.dict(sys.modules, {"claude_agent_sdk": mock_sdk}):
            status, body = await _asgi_request(
                "POST",
                "/v1/turn",
                headers={"Authorization": "Bearer test-secret-token-123"},
                json_body={
                    "model": "claude-opus-4-6",
                    "system": "Use tools.",
                    "messages": [{"role": "user", "content": "Weather in Casablanca?"}],
                    "tools": [
                        {
                            "name": "get_weather",
                            "description": "Get weather for a city",
                            "input_schema": {
                                "type": "object",
                                "properties": {"city": {"type": "string"}},
                                "required": ["city"],
                            },
                        }
                    ],
                },
            )

        self.assertEqual(status, 200)
        self.assertEqual(body["finish_reason"], "tool_use")
        self.assertEqual(body["content"], "Checking weather.")
        self.assertEqual(
            body["tool_calls"],
            [
                {
                    "id": "toolu_01abc",
                    "name": "get_weather",
                    "arguments": {"city": "Casablanca"},
                }
            ],
        )

        opts = seen_options[0]
        self.assertEqual(opts.tools, [])
        self.assertEqual(opts.setting_sources, [])
        self.assertEqual(opts.allowed_tools, ["mcp__serabot__get_weather"])
        self.assertEqual(len(mock_sdk._created_servers), 1)
        self.assertEqual(mock_sdk._created_servers[0]["name"], "serabot")

    async def test_auth_endpoints_require_the_bearer_token(self) -> None:
        for method, path in (
            ("GET", "/v1/auth/status"),
            ("POST", "/v1/auth/start"),
            ("GET", "/v1/auth/flow/abc"),
            ("POST", "/v1/auth/complete"),
            ("POST", "/v1/auth/import-cli"),
            ("POST", "/v1/auth/cancel"),
        ):
            status, _ = await _asgi_request(method, path)
            self.assertEqual(status, 401, path)

    async def test_auth_start_returns_503_when_cli_missing(self) -> None:
        headers = {"Authorization": "Bearer test-secret-token-123"}
        status, body = await _asgi_request("POST", "/v1/auth/start", headers=headers)
        self.assertEqual(status, 503)
        self.assertEqual(body["code"], "claude_cli_missing")

    async def test_cli_auth_start_complete_status_and_cancel(self) -> None:
        headers = {"Authorization": "Bearer test-secret-token-123"}
        cli_path = _write_mock_claude_cli(Path(self._tmp_dir.name))
        os.environ[server.CLI_PATH_ENV_VAR] = str(cli_path)

        # Invalid profile id rejected
        status, body = await _asgi_request(
            "POST",
            "/v1/auth/start",
            headers=headers,
            json_body={"profile": "../escape"},
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "invalid_profile")

        # Start flow with auto-generated profile
        status, started = await _asgi_request(
            "POST", "/v1/auth/start", headers=headers, json_body={}
        )
        self.assertEqual(status, 200)
        self.assertEqual(started["mode"], "cli_browser_or_paste")
        self.assertTrue(started["profile"].startswith("acct_"))
        self.assertEqual(
            started["url"],
            "https://claude.ai/oauth/authorize?client_id=test&state=abc123",
        )
        flow_id = started["flow_id"]

        # Poll flow while pending
        status, flow_state = await _asgi_request(
            "GET", f"/v1/auth/flow/{flow_id}", headers=headers
        )
        self.assertEqual(status, 200)
        self.assertEqual(flow_state["status"], "pending")

        # Complete flow by pasting code
        status, done = await _asgi_request(
            "POST",
            "/v1/auth/complete",
            headers=headers,
            json_body={"flow_id": flow_id, "code": "alice-code#state"},
        )
        self.assertEqual(status, 200)
        self.assertTrue(done["ok"])
        self.assertEqual(done["profile"], started["profile"])
        self.assertEqual(done["account"]["email"], "alice-code#state@example.com")

        # Status now lists the signed-in profile and CLI version
        status, info = await _asgi_request("GET", "/v1/auth/status", headers=headers)
        self.assertEqual(status, 200)
        self.assertTrue(info["cli_installed"])
        self.assertEqual(info["cli_version"], "2.1.0 (Claude Code)")
        self.assertTrue(info["signed_in"])
        profile_names = [p["profile"] for p in info["profiles"]]
        self.assertIn("default", profile_names)
        self.assertIn(started["profile"], profile_names)

        # Cancel flow terminates process cleanly
        status, started2 = await _asgi_request(
            "POST",
            "/v1/auth/start",
            headers=headers,
            json_body={"profile": "work_2"},
        )
        self.assertEqual(status, 200)
        status, cancelled = await _asgi_request(
            "POST",
            "/v1/auth/cancel",
            headers=headers,
            json_body={"flow_id": started2["flow_id"]},
        )
        self.assertEqual(status, 200)
        self.assertTrue(cancelled["ok"])
        status, _ = await _asgi_request(
            "GET", f"/v1/auth/flow/{started2['flow_id']}", headers=headers
        )
        self.assertEqual(status, 404)

    async def test_cli_auth_flow_polling_auto_complete_and_import_cli(self) -> None:
        headers = {"Authorization": "Bearer test-secret-token-123"}
        cli_path = _write_mock_claude_cli(Path(self._tmp_dir.name))
        os.environ[server.CLI_PATH_ENV_VAR] = str(cli_path)

        # Mock auto-completing browser callback
        with patch.dict(os.environ, {"MOCK_LOGIN_MODE": "auto"}):
            status, started = await _asgi_request(
                "POST",
                "/v1/auth/start",
                headers=headers,
                json_body={"profile": "auto_acct"},
            )
            self.assertEqual(status, 200)

        flow_id = started["flow_id"]
        flow_state = {"status": "pending"}
        for _ in range(20):
            status, flow_state = await _asgi_request(
                "GET", f"/v1/auth/flow/{flow_id}", headers=headers
            )
            self.assertEqual(status, 200)
            if flow_state["status"] != "pending":
                break
            time.sleep(0.05)

        self.assertEqual(flow_state["status"], "ok")
        self.assertEqual(flow_state["profile"], "auto_acct")
        self.assertEqual(flow_state["account"]["email"], "auto@example.com")

        # import-cli: 404 when default not logged in, 200 when logged in
        with patch.object(server, "_read_cli_status", return_value=None):
            status, body = await _asgi_request(
                "POST", "/v1/auth/import-cli", headers=headers
            )
        self.assertEqual(status, 404)
        self.assertEqual(body["code"], "not_signed_in")

        fake_acct = {
            "loggedIn": True,
            "authMethod": "claudeai",
            "email": "default@example.com",
            "orgId": None,
            "orgName": None,
            "subscriptionType": "pro",
        }
        with patch.object(server, "_read_cli_status", return_value=fake_acct):
            status, body = await _asgi_request(
                "POST", "/v1/auth/import-cli", headers=headers
            )
        self.assertEqual(status, 200)
        self.assertEqual(body["profile"], "default")
        self.assertEqual(body["account"]["email"], "default@example.com")


if __name__ == "__main__":
    unittest.main()
