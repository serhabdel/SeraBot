from __future__ import annotations

from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import sys
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


def _build_mock_sdk(query_impl):
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
        ClaudeAgentOptions=_FakeClaudeAgentOptions,
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


class TestClaudeGateway(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        self._orig_token = os.environ.get(server.TOKEN_ENV_VAR)
        self._orig_host = os.environ.get(server.HOST_ENV_VAR)
        os.environ[server.TOKEN_ENV_VAR] = "test-secret-token-123"
        os.environ.pop(server.HOST_ENV_VAR, None)

    def tearDown(self) -> None:
        if self._orig_token is None:
            os.environ.pop(server.TOKEN_ENV_VAR, None)
        else:
            os.environ[server.TOKEN_ENV_VAR] = self._orig_token

        if self._orig_host is None:
            os.environ.pop(server.HOST_ENV_VAR, None)
        else:
            os.environ[server.HOST_ENV_VAR] = self._orig_host

    def test_loopback_host_validation(self) -> None:
        self.assertEqual(server.validate_loopback_host("127.0.0.1"), "127.0.0.1")
        self.assertEqual(server.validate_loopback_host("::1"), "::1")
        self.assertEqual(server.validate_loopback_host("[::1]"), "::1")

        for bad_host in ("0.0.0.0", "::", "192.168.1.50", "example.com", ""):
            with self.assertRaises(ValueError):
                server.validate_loopback_host(bad_host)

        with self.assertRaises(ValueError):
            server.main(["--host", "0.0.0.0", "--port", "8791"])

    async def test_health_and_models(self) -> None:
        status, body = await _asgi_request("GET", "/health")
        self.assertEqual(status, 200)
        self.assertEqual(
            body,
            {
                "status": "ok",
                "sdk": "claude-agent-sdk",
                "auth_configured": True,
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
        # Missing Authorization header -> 401
        status, _ = await _asgi_request("GET", "/v1/models")
        self.assertEqual(status, 401)

        # Wrong token -> 401
        status, _ = await _asgi_request(
            "POST",
            "/v1/turn",
            headers={"Authorization": "Bearer wrong-token"},
            json_body={"messages": [{"role": "user", "content": "hi"}]},
        )
        self.assertEqual(status, 401)

        # Unset SERABOT_CLAUDE_GATEWAY_TOKEN -> 503 fail-closed
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

    async def test_turn_text_only_enforces_isolated_options(self) -> None:
        seen_options: list[_FakeClaudeAgentOptions] = []

        async def fake_query(*, prompt: str, options: _FakeClaudeAgentOptions):
            seen_options.append(options)
            yield _FakeAssistantMessage(content=[_FakeTextBlock(text="Hello from Claude")])
            yield _FakeResultMessage(
                subtype="success",
                usage={"input_tokens": 12, "output_tokens": 5},
            )

        mock_sdk = _build_mock_sdk(fake_query)
        with patch.dict(sys.modules, {"claude_agent_sdk": mock_sdk}):
            status, body = await _asgi_request(
                "POST",
                "/v1/turn",
                headers={"Authorization": "Bearer test-secret-token-123"},
                json_body={
                    "model": "claude-sonnet-4-6",
                    "system": "Be concise.",
                    "messages": [{"role": "user", "content": "Say hi"}],
                    "tools": [],
                    "max_tokens": 256,
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

    async def test_turn_captures_mcp_tool_calls(self) -> None:
        seen_options: list[_FakeClaudeAgentOptions] = []

        async def fake_query(*, prompt: str, options: _FakeClaudeAgentOptions):
            seen_options.append(options)
            # Simulate invoking the registered MCP tool handler directly as well
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


if __name__ == "__main__":
    unittest.main()
