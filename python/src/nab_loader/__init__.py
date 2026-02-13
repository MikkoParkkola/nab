"""nab-loader: LangChain and LlamaIndex document loaders powered by nab."""

from nab_loader.core import NabLoader

__all__ = ["NabLoader"]

try:
    from nab_loader.langchain import NabWebLoader

    __all__.append("NabWebLoader")
except ImportError:
    pass

try:
    from nab_loader.llamaindex import NabWebReader

    __all__.append("NabWebReader")
except ImportError:
    pass
