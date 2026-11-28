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

    route_path, _, query_str = path.partition("?")
    scope = {
        "type": "http",
        "asgi": {"version": "3.0"},
        "http_version": "1.1",
        "method": method,
        "scheme": "http",
        "path": route_path,
        "raw_path": route_path.encode("ascii"),
        "query_string": query_str.encode("ascii"),
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

    def store(email):
        config_dir.mkdir(parents=True, exist_ok=True)
        state_file.write_text(json.dumps({
            "loggedIn": True,
            "authMethod": "claudeai",
            "email": email,
            "orgId": "org_1",
            "orgName": "Test Org",
            "subscriptionType": "pro",
        }), encoding="utf-8")

    listener = None
    browser = os.environ.get("BROWSER", "")
    if browser and browser != "true":
        import socket
        import subprocess
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.bind(("127.0.0.1", 0))
        listener.listen(4)
        port = listener.getsockname()[1]
        auto = (
            "https://claude.com/cai/oauth/authorize?code=true&client_id=test"
            f"&redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2Fcallback&state=abc123"
        )
        subprocess.run([browser, auto], check=False)
    print("\x1b[36mOpen this URL:\x1b[0m https://claude.ai/oauth/authorize?client_id=test&state=abc123", flush=True)
    if mode == "fail":
        print("Authentication denied by user", flush=True)
        sys.exit(1)
    if mode == "auto":
        time.sleep(0.15)
        store("auto@example.com")
        sys.exit(0)
    if mode == "callback" and listener is not None:
        from urllib.parse import parse_qs, urlsplit
        conn, _ = listener.accept()
        head = conn.recv(65536).decode("latin-1")
        target = head.split(" ")[1]
        query = parse_qs(urlsplit(target).query)
        code = query.get("code", [""])[0]
        state = query.get("state", [""])[0]
        if state != "abc123" or code == "bad-code":
            conn.sendall(b"HTTP/1.1 200 OK\\r\\nContent-Length: 6\\r\\n\\r\\nfailed")
            conn.close()
            print("Couldn't log in: Request failed with status code 400", flush=True)
            sys.exit(1)
        conn.sendall(b"HTTP/1.1 200 OK\\r\\nContent-Length: 2\\r\\n\\r\\nok")
        conn.close()
        store(f"{code}@example.com")
        sys.exit(0)
    while True:
        print("Paste code here if prompted > ", end="", flush=True)
        code = sys.stdin.readline().strip()
        if "#" not in code:
            print("That doesn't look like the full code. Copy all of it, including the # in the middle.", flush=True)
            continue
        break
    if code.startswith("bad-code"):
        print(f"Couldn't log in: That code wasn't accepted ({code}).", flush=True)
        sys.exit(1)
    store(f"{code}@example.com")
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
        server._last_rate_limits.clear()

    def tearDown(self) -> None:
        server._last_rate_limits.clear()
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
            ("GET", "/v1/auth/usage"),
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
        self.assertIn(started["mode"], ("callback", "paste"))
        self.assertTrue(started["profile"].startswith("acct_"))
        self.assertEqual(
            started["manual_url"],
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

    @unittest.skipIf(os.name == "nt", "the $BROWSER capture is POSIX only")
    async def test_cli_auth_browser_callback_is_relayed_to_the_cli(self) -> None:
        headers = {"Authorization": "Bearer test-secret-token-123"}
        cli_path = _write_mock_claude_cli(Path(self._tmp_dir.name))
        os.environ[server.CLI_PATH_ENV_VAR] = str(cli_path)

        with patch.dict(os.environ, {"MOCK_LOGIN_MODE": "callback"}):
            status, started = await _asgi_request(
                "POST", "/v1/auth/start", headers=headers, json_body={"profile": "cb"}
            )
        self.assertEqual(status, 200)
        # The automatic URL (the CLI's localhost redirect) is what opens.
        self.assertEqual(started["mode"], "callback")
        self.assertTrue(started["url"].startswith("https://claude.com/cai/oauth/authorize"))
        self.assertIn("redirect_uri=http%3A%2F%2Flocalhost", started["url"])
        self.assertEqual(
            started["manual_url"],
            "https://claude.ai/oauth/authorize?client_id=test&state=abc123",
        )
        self.assertEqual(started["callback"]["path"], "/callback")
        self.assertGreaterEqual(started["callback"]["port"], 1024)
        self.assertEqual(started["state"], "abc123")
        flow_id = started["flow_id"]

        # A wrong state never reaches the CLI.
        status, body = await _asgi_request(
            "POST",
            "/v1/auth/complete",
            headers=headers,
            json_body={"flow_id": flow_id, "code": "alice", "state": "other"},
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "state_mismatch")

        status, done = await _asgi_request(
            "POST",
            "/v1/auth/complete",
            headers=headers,
            json_body={"flow_id": flow_id, "code": "alice", "state": "abc123"},
        )
        self.assertEqual(status, 200, done)
        self.assertTrue(done["ok"])
        self.assertEqual(done["profile"], "cb")
        self.assertEqual(done["account"]["email"], "alice@example.com")

        # A poll that races the completion still sees how it ended.
        status, flow_state = await _asgi_request(
            "GET", f"/v1/auth/flow/{flow_id}", headers=headers
        )
        self.assertEqual(status, 200)
        self.assertEqual(flow_state["status"], "ok")
        self.assertEqual(flow_state["account"]["email"], "alice@example.com")
        # Completing again is idempotent for a finished, successful flow.
        status, again = await _asgi_request(
            "POST",
            "/v1/auth/complete",
            headers=headers,
            json_body={"flow_id": flow_id, "code": "alice", "state": "abc123"},
        )
        self.assertEqual(status, 200)
        self.assertTrue(again["ok"])

        # A pasted localhost URL from the address bar is relayed the same way;
        # a refused exchange reports Claude's reason, not a generic failure.
        with patch.dict(os.environ, {"MOCK_LOGIN_MODE": "callback"}):
            status, started = await _asgi_request(
                "POST", "/v1/auth/start", headers=headers, json_body={"profile": "cb2"}
            )
        pasted = (
            f"http://localhost:{started['callback']['port']}/callback"
            "?code=bad-code&state=abc123"
        )
        status, body = await _asgi_request(
            "POST",
            "/v1/auth/complete",
            headers=headers,
            json_body={"flow_id": started["flow_id"], "code": pasted},
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "auth_code_rejected")
        self.assertIn("did not accept", body["error"])
        status, flow_state = await _asgi_request(
            "GET", f"/v1/auth/flow/{started['flow_id']}", headers=headers
        )
        self.assertEqual(flow_state["status"], "denied")

    async def test_cli_auth_paste_reprompt_keeps_the_flow_open(self) -> None:
        headers = {"Authorization": "Bearer test-secret-token-123"}
        cli_path = _write_mock_claude_cli(Path(self._tmp_dir.name))
        os.environ[server.CLI_PATH_ENV_VAR] = str(cli_path)

        status, started = await _asgi_request(
            "POST", "/v1/auth/start", headers=headers, json_body={"profile": "paste"}
        )
        self.assertEqual(status, 200)
        flow_id = started["flow_id"]

        # Control characters are refused before anything reaches the CLI.
        status, body = await _asgi_request(
            "POST",
            "/v1/auth/complete",
            headers=headers,
            json_body={"flow_id": flow_id, "code": "abc\x1b[2Jdef"},
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "invalid_code")

        if server.pty is not None and os.name != "nt":
            status, body = await _asgi_request(
                "POST",
                "/v1/auth/complete",
                headers=headers,
                json_body={"flow_id": flow_id, "code": "half-a-code"},
            )
            self.assertEqual(status, 400)
            self.assertEqual(body["code"], "code_incomplete")
            self.assertIn("#", body["error"])
            status, flow_state = await _asgi_request(
                "GET", f"/v1/auth/flow/{flow_id}", headers=headers
            )
            self.assertEqual(flow_state["status"], "pending")

        status, body = await _asgi_request(
            "POST",
            "/v1/auth/complete",
            headers=headers,
            json_body={"flow_id": flow_id, "code": "bad-code#secret-state"},
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "auth_code_rejected")
        # The echoed code never comes back in the message.
        self.assertNotIn("secret-state", body["error"])
        self.assertIn("wasn't accepted", body["error"])

    def test_cli_failure_message_and_callback_parsing(self) -> None:
        out = (
            "Opening browser to sign in\u2026\r\nIf the browser didn't open, visit:\r\n"
            "https://claude.ai/oauth/authorize?z-abc\r\n"
            "Paste code here if prompted > Couldn't log in: Request failed with status code 400\r\n"
        )
        self.assertIn("did not accept", server._cli_failure_message(out, 1, None))
        echoed = "Paste code here if prompted > tok#st\r\r\nBoom tok#st here\r\n"
        self.assertEqual(server._cli_failure_message(echoed, 1, "tok#st"), "Boom … here")

        good = (
            "https://claude.com/cai/oauth/authorize?code=true&client_id=x"
            "&redirect_uri=http%3A%2F%2Flocalhost%3A40123%2Fcallback&state=s1"
        )
        self.assertEqual(
            server._parse_cli_callback(good),
            {"port": 40123, "path": "/callback", "state": "s1"},
        )
        for bad in (
            good.replace("http%3A%2F%2Flocalhost", "http%3A%2F%2Fevil.example"),
            good.replace("40123", "80"),
            good.replace("https://", "http://"),
            good.replace("&state=s1", ""),
            None,
        ):
            self.assertIsNone(server._parse_cli_callback(bad), bad)

        # A URL split across two reads is not taken until it is complete.
        self.assertIsNone(server._extract_https_url("visit: https://claude.ai/oauth/auth"))
        self.assertEqual(
            server._extract_https_url("visit: https://claude.ai/oauth/x?y=1\r\n"),
            "https://claude.ai/oauth/x?y=1",
        )
        self.assertEqual(
            server._parse_pasted_callback("http://localhost:1234/callback?code=c&state=s"),
            ("c", "s"),
        )
        self.assertIsNone(server._parse_pasted_callback("c#s"))

    async def test_auth_usage_live_oauth_and_validation(self) -> None:
        headers = {"Authorization": "Bearer test-secret-token-123"}

        # Rejects invalid profile id
        status, body = await _asgi_request(
            "GET", "/v1/auth/usage?profile=../bad", headers=headers
        )
        self.assertEqual(status, 400)
        self.assertEqual(body["code"], "invalid_profile")

        # Write OAuth credentials for named profile
        secret_oauth_token = "sk-ant-oat01-very-secret-token-xyz"
        profile_dir = Path(self._tmp_dir.name) / "profiles" / "work_1"
        profile_dir.mkdir(parents=True)
        (profile_dir / ".credentials.json").write_text(
            json.dumps({"claudeAiOauth": {"accessToken": secret_oauth_token}}),
            encoding="utf-8",
        )

        seen_requests: list[tuple[Any, float]] = []

        class _FakeResp:
            status = 200

            def __init__(self, raw: bytes) -> None:
                self._raw = raw

            def read(self, n: int = -1) -> bytes:
                return self._raw[:n] if n >= 0 else self._raw

            def __enter__(self) -> "_FakeResp":
                return self

            def __exit__(self, *args: Any) -> bool:
                return False

        class _FakeOpener:
            def open(self, req: Any, timeout: float = 0.0) -> _FakeResp:
                seen_requests.append((req, timeout))
                payload = json.dumps(
                    {
                        "five_hour": {
                            "utilization": 37.5,
                            "resets_at": "2026-11-28T05:00:00Z",
                        },
                        "seven_day": {
                            "utilization": 12.0,
                            "resets_at": "2026-12-04T00:00:00Z",
                        },
                        "seven_day_sonnet": None,
                        "seven_day_opus": {
                            "utilization": 5.0,
                            "resets_at": "2026-12-04T00:00:00Z",
                        },
                    }
                ).encode("utf-8")
                return _FakeResp(payload)

        with patch.object(
            server.urllib.request, "build_opener", return_value=_FakeOpener()
        ) as mock_build:
            status, body = await _asgi_request(
                "GET", "/v1/auth/usage?profile=work_1", headers=headers
            )

        self.assertEqual(status, 200)
        mock_build.assert_called_once_with(server._NoRedirect)
        self.assertEqual(len(seen_requests), 1)
        req, timeout = seen_requests[0]
        self.assertEqual(req.full_url, "https://api.anthropic.com/api/oauth/usage")
        self.assertEqual(timeout, 15.0)
        req_headers = {k.lower(): v for k, v in req.header_items()}
        self.assertEqual(req_headers["authorization"], f"Bearer {secret_oauth_token}")
        self.assertEqual(req_headers["anthropic-beta"], "oauth-2025-04-20")
        self.assertEqual(req_headers["user-agent"], "claude-code/2.0.0")

        self.assertTrue(body["available"])
        quota = body["quota"]
        self.assertEqual(quota["source"], "claude")
        self.assertEqual(quota["state"], "ok")
        self.assertTrue(quota["sampled_at"].endswith("Z"))
        self.assertEqual(
            quota["windows"],
            [
                {
                    "scope": "subscription",
                    "label": "5h",
                    "used_percent": 37.5,
                    "remaining": 0.625,
                    "resets_at": "2026-11-28T05:00:00Z",
                },
                {
                    "scope": "subscription",
                    "label": "7d",
                    "used_percent": 12.0,
                    "remaining": 0.88,
                    "resets_at": "2026-12-04T00:00:00Z",
                },
                {
                    "scope": "subscription",
                    "label": "7d_opus",
                    "used_percent": 5.0,
                    "remaining": 0.95,
                    "resets_at": "2026-12-04T00:00:00Z",
                },
            ],
        )
        # Never leak the OAuth token
        self.assertNotIn(secret_oauth_token, json.dumps(body))

    async def test_auth_usage_falls_back_to_cached_rate_limits(self) -> None:
        headers = {"Authorization": "Bearer test-secret-token-123"}

        # No credentials and no cache -> unavailable
        with patch.object(Path, "home", return_value=Path(self._tmp_dir.name)):
            status, body = await _asgi_request("GET", "/v1/auth/usage", headers=headers)
        self.assertEqual(status, 200)
        self.assertEqual(body, {"available": False, "quota": None})

        # Run a turn on profile "work_2" that emits rate_limit_info on ResultMessage
        async def fake_query(*, prompt: str, options: _FakeClaudeAgentOptions):
            yield _FakeAssistantMessage(content=[_FakeTextBlock(text="ok")])
            yield SimpleNamespace(
                subtype="success",
                usage={"input_tokens": 5, "output_tokens": 2},
                result=None,
                rate_limit_info={
                    "status": "allowed_warning",
                    "rateLimitType": "five_hour",
                    "utilization": 0.42,
                    "resetsAt": 1795824000,
                },
            )

        mock_sdk = _build_mock_sdk(fake_query)
        with patch.dict(sys.modules, {"claude_agent_sdk": mock_sdk}):
            status, _ = await _asgi_request(
                "POST",
                "/v1/turn",
                headers=headers,
                json_body={
                    "profile": "work_2",
                    "messages": [{"role": "user", "content": "hi"}],
                },
            )
        self.assertEqual(status, 200)

        # Profile "work_2" now falls back to its cached rate_limit_info
        status, body = await _asgi_request(
            "GET", "/v1/auth/usage?profile=work_2", headers=headers
        )
        self.assertEqual(status, 200)
        self.assertTrue(body["available"])
        quota = body["quota"]
        self.assertEqual(quota["source"], "claude")
        self.assertEqual(quota["state"], "ok")
        self.assertEqual(len(quota["windows"]), 1)
        win = quota["windows"][0]
        self.assertEqual(win["scope"], "subscription")
        self.assertEqual(win["label"], "5h")
        self.assertAlmostEqual(win["used_percent"], 42.0)
        self.assertAlmostEqual(win["remaining"], 0.58)
        self.assertEqual(win["resets_at"], "2026-11-28T00:00:00Z")

        # Default profile remains unaffected by work_2's cache
        with patch.object(Path, "home", return_value=Path(self._tmp_dir.name)):
            status, body = await _asgi_request("GET", "/v1/auth/usage", headers=headers)
        self.assertEqual(status, 200)
        self.assertEqual(body, {"available": False, "quota": None})


if __name__ == "__main__":
    unittest.main()
