"""LlamaIndex document reader powered by nab."""

from __future__ import annotations

from typing import List, Optional

from llama_index.core import Document
from llama_index.core.readers.base import BaseReader

from nab_loader.core import NabLoader


class NabWebReader(BaseReader):
    """Read web pages as LlamaIndex Documents using nab.

    Each URL becomes a Document with text set to the markdown
    conversion and metadata containing url, status, and size.

    Example::

        reader = NabWebReader()
        docs = reader.load_data(["https://example.com"])
        print(docs[0].text)
    """

    def __init__(
        self,
        *,
        cookies: str = "auto",
        binary: Optional[str] = None,
    ) -> None:
        super().__init__()
        self._loader = NabLoader(binary=binary, cookies=cookies)

    def load_data(self, urls: List[str]) -> List[Document]:
        """Fetch URLs and return LlamaIndex Documents."""
        results = self._loader.fetch_batch(urls)
        return [
            Document(
                text=r.markdown,
                metadata={
                    "source": r.url,
                    "status": r.status,
                    "size": r.size,
                    "time_ms": r.time_ms,
                },
            )
            for r in results
        ]
