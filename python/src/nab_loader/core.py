"""Core nab integration: shells out to the nab CLI and parses results."""

from __future__ import annotations

import json
import shutil
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from typing import List, Optional


class NabNotFoundError(RuntimeError):
    """Raised when the nab binary is not found on PATH."""

    def __init__(self) -> None:
        super().__init__(
            "nab binary not found. Install it first:\n"
            "  brew install mikkoparkkola/tap/nab   # macOS\n"
            "  cargo install nab                     # from source\n"
            "See https://github.com/MikkoParkkola/nab for details."
        )


class NabFetchError(RuntimeError):
    """Raised when nab fetch fails for a URL."""

    def __init__(self, url: str, reason: str) -> None:
        self.url = url
        super().__init__(f"nab fetch failed for {url}: {reason}")


@dataclass
class NabResult:
    """Result of a single nab fetch."""

    url: str
    markdown: str
    status: int
    size: int
    time_ms: float
    metadata: dict = field(default_factory=dict)


class NabLoader:
    """Fetch web content via the nab CLI and return structured results.

    Args:
        binary: Path to the nab binary. Defaults to finding it on PATH.
        cookies: Cookie source (auto, brave, chrome, firefox, none).
        timeout: Subprocess timeout in seconds per URL.
    """

    def __init__(
        self,
        binary: Optional[str] = None,
        cookies: str = "auto",
        timeout: int = 30,
    ) -> None:
        self.binary = binary or shutil.which("nab")
        if self.binary is None:
            raise NabNotFoundError()
        self.cookies = cookies
        self.timeout = timeout

    def fetch(self, url: str) -> NabResult:
        """Fetch a single URL and return structured result."""
        cmd = [
            self.binary,
            "fetch",
            url,
            "--format",
            "json",
            "--cookies",
            self.cookies,
            "--body",
        ]
        try:
            proc = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=self.timeout,
            )
        except FileNotFoundError:
            raise NabNotFoundError()
        except subprocess.TimeoutExpired:
            raise NabFetchError(url, f"timed out after {self.timeout}s")

        if proc.returncode != 0:
            raise NabFetchError(url, proc.stderr.strip() or f"exit code {proc.returncode}")

        # nab --format json outputs JSON on first line, body follows
        lines = proc.stdout.split("\n", 1)
        try:
            meta = json.loads(lines[0])
        except (json.JSONDecodeError, IndexError):
            raise NabFetchError(url, "could not parse nab JSON output")

        markdown = lines[1] if len(lines) > 1 else ""

        return NabResult(
            url=meta.get("url", url),
            markdown=markdown,
            status=meta.get("status", 0),
            size=meta.get("size", len(markdown)),
            time_ms=meta.get("time_ms", 0.0),
            metadata=meta,
        )

    def fetch_batch(self, urls: List[str], parallel: int = 5) -> List[NabResult]:
        """Fetch multiple URLs in parallel.

        Args:
            urls: List of URLs to fetch.
            parallel: Maximum concurrent fetches.

        Returns:
            List of NabResult in the same order as input URLs.
            Failed fetches are included with empty markdown and status=0.
        """
        results: dict[int, NabResult] = {}

        with ThreadPoolExecutor(max_workers=parallel) as pool:
            future_to_idx = {pool.submit(self._safe_fetch, url): i for i, url in enumerate(urls)}
            for future in as_completed(future_to_idx):
                idx = future_to_idx[future]
                results[idx] = future.result()

        return [results[i] for i in range(len(urls))]

    def _safe_fetch(self, url: str) -> NabResult:
        """Fetch a URL, returning an error result instead of raising."""
        try:
            return self.fetch(url)
        except (NabFetchError, NabNotFoundError):
            return NabResult(url=url, markdown="", status=0, size=0, time_ms=0.0)
