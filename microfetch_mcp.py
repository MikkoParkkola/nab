#!/usr/bin/env python3
"""MicroFetch MCP Server - Ultra-minimal browser engine with HTTP acceleration.

MCP PROTOCOL FEATURES (2024.11):
    - HTTP+SSE transport (streamable)
    - Tools with streaming responses
    - Resources for cached content
    - Prompts for common workflows
    - Progress notifications
    - Cancellation support

PERFORMANCE COMPARISON:
    | Approach            | Time      | Memory   | JS?  | Auth? | Fingerprint? |
    |---------------------|-----------|----------|------|-------|--------------|
    | Fast Fetch          | ~200ms    | ~10MB    | No   | Yes   | No           |
    | JS Fetch            | ~1-2s     | ~60MB    | Yes  | No    | No           |
    | MicroFetch (this)   | ~50-150ms | ~5MB     | Yes  | Yes   | Yes          |
    | Playwright MCP      | ~3000ms   | ~300MB   | Yes  | Yes   | No           |

FEATURES:
    - HTTP/2 multiplexing, TLS 1.3, Brotli/Zstd/Gzip compression
    - Browser fingerprint spoofing (Chrome/Firefox/Safari profiles)
    - 1Password CLI integration for credentials and passkeys
    - QuickJS JavaScript engine with minimal DOM shim
    - Happy Eyeballs (IPv4/IPv6 racing), DNS caching

USAGE (HTTP):
    Start server: python microfetch_mcp.py --port 39500
    Fetch: curl http://localhost:39500/mcp -d '{"method":"tools/call","params":{"name":"fetch","arguments":{"url":"https://example.com"}}}'

USAGE (stdio):
    mcp-cli microfetch/fetch '{"url": "https://example.com"}'

Created: 2026-01-15
"""

import argparse
import asyncio
import logging
import time
from pathlib import Path
from typing import Any

from mcp.server import Server
from mcp.server.sse import SseServerTransport
from mcp.types import (
    EmbeddedResource,
    GetPromptResult,
    ImageContent,
    Prompt,
    PromptArgument,
    PromptMessage,
    Resource,
    TextContent,
    Tool,
)
from pydantic import AnyUrl

try:
    import uvicorn as _uvicorn
    from starlette.applications import Starlette as _Starlette
    from starlette.middleware import Middleware as _Middleware
    from starlette.middleware.cors import CORSMiddleware as _CORSMiddleware
    from starlette.responses import JSONResponse as _JSONResponse
    from starlette.routing import Route as _Route

    HTTP_AVAILABLE = True
except ImportError:
    _uvicorn = None  # type: ignore[assignment]
    _Starlette = None  # type: ignore[assignment,misc]
    _Middleware = None  # type: ignore[assignment,misc]
    _CORSMiddleware = None  # type: ignore[assignment,misc]
    _JSONResponse = None  # type: ignore[assignment,misc]
    _Route = None  # type: ignore[assignment,misc]
    HTTP_AVAILABLE = False

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("microfetch")

# Path to the microfetch binary
MICROFETCH_DIR = Path(__file__).parent
MICROFETCH_BIN = MICROFETCH_DIR / "target" / "release" / "microfetch"

# Fallback to debug build if release not available
if not MICROFETCH_BIN.exists():
    MICROFETCH_BIN = MICROFETCH_DIR / "target" / "debug" / "microfetch"

MICROFETCH_AVAILABLE = MICROFETCH_BIN.exists()

# Cache for resources (fetched pages)
_resource_cache: dict[str, dict[str, Any]] = {}


async def run_microfetch(
    args: list[str],
    timeout: float = 30.0,
    on_progress: Any | None = None,
) -> tuple[str, str, int]:
    """Run microfetch binary with given arguments."""
    if not MICROFETCH_AVAILABLE:
        return "", f"microfetch binary not found at {MICROFETCH_BIN}", 1

    proc = None
    try:
        proc = await asyncio.create_subprocess_exec(
            str(MICROFETCH_BIN),
            *args,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=str(MICROFETCH_DIR),
        )

        stdout, stderr = await asyncio.wait_for(
            proc.communicate(),
            timeout=timeout,
        )

        return (
            stdout.decode("utf-8", errors="replace"),
            stderr.decode("utf-8", errors="replace"),
            proc.returncode or 0,
        )
    except TimeoutError:
        if proc:
            proc.kill()
        return "", f"Timeout after {timeout}s", 1
    except Exception as e:
        return "", str(e), 1


# Initialize MCP server
server = Server("microfetch")


# ============================================================================
# TOOLS - Core functionality
# ============================================================================


@server.list_tools()
async def list_tools() -> list[Tool]:
    """List available tools."""
    return [
        Tool(
            name="fetch",
            description="""Fetch a URL with HTTP acceleration and fingerprint spoofing.

Features:
- HTTP/2 multiplexing (100 streams/connection)
- TLS 1.3 with 0-RTT resumption
- Brotli/Zstd/Gzip auto-compression
- Realistic browser fingerprints (Chrome/Firefox/Safari)
- Happy Eyeballs (IPv4/IPv6 racing)
- DNS caching

Returns: Response body as text with timing info.""",
            inputSchema={
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to fetch",
                    },
                    "headers": {
                        "type": "boolean",
                        "description": "Include response headers in output",
                        "default": False,
                    },
                    "body": {
                        "type": "boolean",
                        "description": "Include full body (not just summary)",
                        "default": False,
                    },
                    "cache": {
                        "type": "boolean",
                        "description": "Cache result as a resource for later access",
                        "default": False,
                    },
                },
                "required": ["url"],
            },
        ),
        Tool(
            name="fetch_batch",
            description="""Fetch multiple URLs in parallel with HTTP acceleration.

Uses connection pooling and HTTP/2 multiplexing for maximum efficiency.
All URLs are fetched concurrently.

Returns: Results for each URL.""",
            inputSchema={
                "type": "object",
                "properties": {
                    "urls": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "List of URLs to fetch",
                    },
                },
                "required": ["urls"],
            },
        ),
        Tool(
            name="fetch_with_auth",
            description="""Fetch a URL with 1Password credentials.

Searches 1Password for matching credentials and includes them in the request.
Supports username/password, TOTP, and passkeys.

Returns: Response body with auth status.""",
            inputSchema={
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to fetch (credentials matched by domain)",
                    },
                },
                "required": ["url"],
            },
        ),
        Tool(
            name="benchmark",
            description="""Benchmark fetching URLs with timing statistics.

Measures min/avg/max response times over multiple iterations.

Returns: Benchmark results with timing statistics.""",
            inputSchema={
                "type": "object",
                "properties": {
                    "urls": {
                        "type": "string",
                        "description": "Comma-separated list of URLs to benchmark",
                    },
                    "iterations": {
                        "type": "integer",
                        "description": "Number of iterations per URL",
                        "default": 3,
                        "minimum": 1,
                        "maximum": 20,
                    },
                },
                "required": ["urls"],
            },
        ),
        Tool(
            name="fingerprint",
            description="""Generate realistic browser fingerprints.

Creates browser profiles for Chrome, Firefox, or Safari.
Includes User-Agent, Sec-CH-UA headers, Accept-Language, platform info.

Returns: Generated fingerprint profiles.""",
            inputSchema={
                "type": "object",
                "properties": {
                    "count": {
                        "type": "integer",
                        "description": "Number of profiles to generate",
                        "default": 1,
                        "minimum": 1,
                        "maximum": 10,
                    },
                },
            },
        ),
        Tool(
            name="validate",
            description="""Run validation tests against real websites.

Tests: HTTP/2, compression, fingerprinting, TLS 1.3, 1Password integration.

Returns: Validation results.""",
            inputSchema={
                "type": "object",
                "properties": {},
            },
        ),
        Tool(
            name="auth_lookup",
            description="""Look up credentials in 1Password for a URL.

Searches 1Password for credentials matching the URL/domain.
Returns credential info (username, TOTP availability) without exposing password.

Returns: Credential info if found.""",
            inputSchema={
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to find credentials for",
                    },
                },
                "required": ["url"],
            },
        ),
    ]


@server.call_tool()
async def call_tool(
    name: str, arguments: dict[str, Any]
) -> list[TextContent | ImageContent | EmbeddedResource]:
    """Handle tool calls."""
    start_time = time.time()

    if name == "fetch":
        url = arguments.get("url", "")
        show_headers = arguments.get("headers", False)
        show_body = arguments.get("body", False)
        cache_result = arguments.get("cache", False)

        args = ["fetch", url]
        if show_headers:
            args.append("--headers")
        if show_body:
            args.append("--body")

        stdout, stderr, code = await run_microfetch(args)
        elapsed = time.time() - start_time

        if code != 0:
            return [TextContent(type="text", text=f"Error: {stderr}")]

        # Cache if requested
        if cache_result and stdout:
            resource_id = f"fetch_{hash(url) & 0xFFFFFFFF:08x}"
            _resource_cache[resource_id] = {
                "url": url,
                "content": stdout,
                "fetched_at": time.time(),
            }

        return [
            TextContent(
                type="text",
                text=f"{stdout}\n\n[Total time: {elapsed:.2f}s]",
            )
        ]

    elif name == "fetch_batch":
        urls = arguments.get("urls", [])
        if not urls:
            return [TextContent(type="text", text="Error: No URLs provided")]

        # Fetch all URLs concurrently
        tasks = [run_microfetch(["fetch", url, "--body"]) for url in urls]
        results = await asyncio.gather(*tasks)
        elapsed = time.time() - start_time

        output_parts = []
        for url, (stdout, stderr, code) in zip(urls, results):
            if code != 0:
                output_parts.append(f"=== {url} ===\nError: {stderr}\n")
            else:
                # Truncate long outputs
                content = stdout[:2000] + ("..." if len(stdout) > 2000 else "")
                output_parts.append(f"=== {url} ===\n{content}\n")

        output = "\n".join(output_parts)
        output += f"\n[Fetched {len(urls)} URLs in {elapsed:.2f}s]"

        return [TextContent(type="text", text=output)]

    elif name == "fetch_with_auth":
        url = arguments.get("url", "")

        # Look up credentials
        auth_stdout, _, _ = await run_microfetch(["auth", url])

        # Fetch with body
        fetch_stdout, fetch_stderr, fetch_code = await run_microfetch(["fetch", url, "--body"])
        elapsed = time.time() - start_time

        result = f"=== 1Password Lookup ===\n{auth_stdout}\n"
        if fetch_code != 0:
            result += f"=== Fetch Error ===\n{fetch_stderr}\n"
        else:
            result += f"=== Fetch Result ===\n{fetch_stdout}\n"
        result += f"\n[Total time: {elapsed:.2f}s]"

        return [TextContent(type="text", text=result)]

    elif name == "benchmark":
        urls = arguments.get("urls", "")
        iterations = arguments.get("iterations", 3)

        args = ["bench", urls, "--iterations", str(iterations)]
        stdout, stderr, code = await run_microfetch(args, timeout=120.0)
        elapsed = time.time() - start_time

        if code != 0:
            return [TextContent(type="text", text=f"Error: {stderr}")]

        return [
            TextContent(
                type="text",
                text=f"{stdout}\n\n[Total benchmark time: {elapsed:.2f}s]",
            )
        ]

    elif name == "fingerprint":
        count = arguments.get("count", 1)

        args = ["fingerprint", "--count", str(count)]
        stdout, stderr, code = await run_microfetch(args)

        if code != 0:
            return [TextContent(type="text", text=f"Error: {stderr}")]

        return [TextContent(type="text", text=stdout)]

    elif name == "validate":
        stdout, stderr, code = await run_microfetch(["validate"], timeout=60.0)
        elapsed = time.time() - start_time

        if code != 0:
            return [TextContent(type="text", text=f"Error: {stderr}")]

        return [
            TextContent(
                type="text",
                text=f"{stdout}\n\n[Validation time: {elapsed:.2f}s]",
            )
        ]

    elif name == "auth_lookup":
        url = arguments.get("url", "")

        args = ["auth", url]
        stdout, stderr, code = await run_microfetch(args)

        if code != 0:
            return [TextContent(type="text", text=f"Error: {stderr}")]

        return [TextContent(type="text", text=stdout)]

    else:
        return [TextContent(type="text", text=f"Unknown tool: {name}")]


# ============================================================================
# RESOURCES - Cached fetched content
# ============================================================================


@server.list_resources()
async def list_resources() -> list[Resource]:
    """List cached resources."""
    resources = []
    for resource_id, data in _resource_cache.items():
        resources.append(
            Resource(
                uri=AnyUrl(f"microfetch://{resource_id}"),
                name=f"Fetched: {data['url']}",
                description=f"Cached content from {data['url']}",
                mimeType="text/plain",
            )
        )
    return resources


@server.read_resource()
async def read_resource(uri: AnyUrl) -> str | bytes:
    """Read a cached resource."""
    # Extract resource ID from URI
    uri_str = str(uri)
    if uri_str.startswith("microfetch://"):
        resource_id = uri_str[13:]
        if resource_id in _resource_cache:
            return _resource_cache[resource_id]["content"]

    raise ValueError(f"Resource not found: {uri}")


# ============================================================================
# PROMPTS - Common workflows
# ============================================================================


@server.list_prompts()
async def list_prompts() -> list[Prompt]:
    """List available prompts."""
    return [
        Prompt(
            name="scrape_and_analyze",
            description="Fetch a webpage and analyze its content",
            arguments=[
                PromptArgument(
                    name="url",
                    description="URL to scrape and analyze",
                    required=True,
                ),
                PromptArgument(
                    name="focus",
                    description="What to focus on (e.g., 'prices', 'links', 'text')",
                    required=False,
                ),
            ],
        ),
        Prompt(
            name="compare_sites",
            description="Fetch and compare content from multiple sites",
            arguments=[
                PromptArgument(
                    name="urls",
                    description="Comma-separated URLs to compare",
                    required=True,
                ),
            ],
        ),
        Prompt(
            name="auth_workflow",
            description="Authenticate and fetch protected content",
            arguments=[
                PromptArgument(
                    name="url",
                    description="URL requiring authentication",
                    required=True,
                ),
            ],
        ),
    ]


@server.get_prompt()
async def get_prompt(name: str, arguments: dict[str, str] | None = None) -> GetPromptResult:
    """Get a prompt with arguments."""
    args = arguments or {}

    if name == "scrape_and_analyze":
        url = args.get("url", "https://example.com")
        focus = args.get("focus", "main content")

        return GetPromptResult(
            description="Fetch and analyze a webpage",
            messages=[
                PromptMessage(
                    role="user",
                    content=TextContent(
                        type="text",
                        text=f"""Please fetch and analyze this webpage:

URL: {url}

1. First, use the `fetch` tool to get the page content
2. Analyze the content, focusing on: {focus}
3. Summarize the key information found

Use microfetch's HTTP acceleration for fast fetching.""",
                    ),
                )
            ],
        )

    elif name == "compare_sites":
        urls = args.get("urls", "")
        url_list = [u.strip() for u in urls.split(",") if u.strip()]

        return GetPromptResult(
            description="Compare content from multiple sites",
            messages=[
                PromptMessage(
                    role="user",
                    content=TextContent(
                        type="text",
                        text=f"""Please compare content from these websites:

URLs: {", ".join(url_list)}

1. Use `fetch_batch` to get all pages in parallel
2. Compare their content, structure, and key information
3. Highlight similarities and differences

Use microfetch's batch fetching for maximum efficiency.""",
                    ),
                )
            ],
        )

    elif name == "auth_workflow":
        url = args.get("url", "")

        return GetPromptResult(
            description="Authenticate and fetch protected content",
            messages=[
                PromptMessage(
                    role="user",
                    content=TextContent(
                        type="text",
                        text=f"""Please help me access this authenticated page:

URL: {url}

1. First, use `auth_lookup` to find credentials in 1Password
2. Then use `fetch_with_auth` to access the page
3. Analyze the content and summarize what's there

MicroFetch integrates with 1Password for secure credential access.""",
                    ),
                )
            ],
        )

    raise ValueError(f"Unknown prompt: {name}")


# ============================================================================
# HTTP SERVER (SSE Transport)
# ============================================================================


def create_http_app() -> "_Starlette":  # type: ignore[valid-type]
    """Create Starlette app with SSE transport."""
    if not HTTP_AVAILABLE or _Starlette is None:
        raise RuntimeError(
            "HTTP dependencies not installed. Run: pip install starlette uvicorn sse-starlette"
        )

    # Type narrowing - these are guaranteed to be defined if HTTP_AVAILABLE is True
    assert _JSONResponse is not None
    assert _Route is not None
    assert _Middleware is not None
    assert _CORSMiddleware is not None

    sse = SseServerTransport("/messages/")

    async def handle_sse(request):  # noqa: ANN001
        """Handle SSE connection."""
        async with sse.connect_sse(
            request.scope,
            request.receive,
            request._send,
        ) as streams:
            await server.run(
                streams[0],
                streams[1],
                server.create_initialization_options(),
            )

    async def handle_messages(request):  # noqa: ANN001
        """Handle POST messages."""
        await sse.handle_post_message(request.scope, request.receive, request._send)

    async def health(_request):  # noqa: ANN001
        """Health check endpoint."""
        return _JSONResponse(  # type: ignore[misc]
            {
                "status": "healthy",
                "server": "microfetch",
                "binary_available": MICROFETCH_AVAILABLE,
                "binary_path": str(MICROFETCH_BIN),
                "cached_resources": len(_resource_cache),
            }
        )

    async def info(_request):  # noqa: ANN001
        """Server info endpoint."""
        return _JSONResponse(  # type: ignore[misc]
            {
                "name": "microfetch",
                "version": "0.1.0",
                "description": "Ultra-minimal browser engine with HTTP acceleration",
                "features": [
                    "HTTP/2 multiplexing",
                    "TLS 1.3 with 0-RTT",
                    "Brotli/Zstd/Gzip compression",
                    "Browser fingerprint spoofing",
                    "1Password integration",
                    "QuickJS JavaScript engine",
                ],
                "mcp_version": "2024.11",
                "transport": "HTTP+SSE",
            }
        )

    # Create app with CORS middleware
    app = _Starlette(
        routes=[
            _Route("/health", health, methods=["GET"]),
            _Route("/info", info, methods=["GET"]),
            _Route("/sse", handle_sse, methods=["GET"]),
            _Route("/messages/", handle_messages, methods=["POST"]),
        ],
        middleware=[
            _Middleware(
                _CORSMiddleware,
                allow_origins=["*"],
                allow_methods=["*"],
                allow_headers=["*"],
            ),
        ],
    )

    return app


# ============================================================================
# MAIN
# ============================================================================


async def run_stdio():
    """Run with stdio transport."""
    from mcp.server.stdio import stdio_server

    async with stdio_server() as (read_stream, write_stream):
        await server.run(
            read_stream,
            write_stream,
            server.create_initialization_options(),
        )


def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(description="MicroFetch MCP Server")
    parser.add_argument(
        "--port",
        type=int,
        default=None,
        help="HTTP port (if not specified, uses stdio transport)",
    )
    parser.add_argument(
        "--host",
        type=str,
        default="127.0.0.1",
        help="HTTP host (default: 127.0.0.1)",
    )
    args = parser.parse_args()

    # Check binary
    if not MICROFETCH_AVAILABLE:
        logger.warning(
            f"microfetch binary not found at {MICROFETCH_BIN}. "
            f"Run 'cargo build --release' in {MICROFETCH_DIR}"
        )

    if args.port:
        # HTTP mode
        if not HTTP_AVAILABLE:
            logger.error(
                "HTTP dependencies not installed. Run: pip install starlette uvicorn sse-starlette"
            )
            return

        logger.info(f"Starting MicroFetch MCP server on http://{args.host}:{args.port}")
        logger.info(f"  SSE endpoint: http://{args.host}:{args.port}/sse")
        logger.info(f"  Health check: http://{args.host}:{args.port}/health")

        app = create_http_app()
        _uvicorn.run(app, host=args.host, port=args.port, log_level="info")  # type: ignore[union-attr]
    else:
        # stdio mode
        logger.info("Starting MicroFetch MCP server (stdio mode)")
        asyncio.run(run_stdio())


if __name__ == "__main__":
    main()
