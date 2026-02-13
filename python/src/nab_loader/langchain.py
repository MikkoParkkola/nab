"""LangChain document loader powered by nab."""

from __future__ import annotations

from typing import Iterator, List, Optional

from langchain_core.document_loaders import BaseLoader
from langchain_core.documents import Document

from nab_loader.core import NabLoader


class NabWebLoader(BaseLoader):
    """Load web pages as LangChain Documents using nab.

    Each URL becomes a Document with page_content set to the markdown
    conversion and metadata containing url, status, and size.

    Example::

        loader = NabWebLoader(["https://example.com"])
        docs = loader.load()
        print(docs[0].page_content)
    """

    def __init__(
        self,
        urls: List[str],
        *,
        cookies: str = "auto",
        binary: Optional[str] = None,
    ) -> None:
        self.urls = urls
        self._loader = NabLoader(binary=binary, cookies=cookies)

    def lazy_load(self) -> Iterator[Document]:
        """Yield Documents one at a time."""
        for url in self.urls:
            result = self._loader.fetch(url)
            yield Document(
                page_content=result.markdown,
                metadata={
                    "source": result.url,
                    "status": result.status,
                    "size": result.size,
                    "time_ms": result.time_ms,
                },
            )

    def load(self) -> List[Document]:
        """Load all URLs in parallel and return Documents."""
        results = self._loader.fetch_batch(self.urls)
        return [
            Document(
                page_content=r.markdown,
                metadata={
                    "source": r.url,
                    "status": r.status,
                    "size": r.size,
                    "time_ms": r.time_ms,
                },
            )
            for r in results
        ]
