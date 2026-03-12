# LinkedIn Post Drafts

## Post 1: nab

A typical web page: ~12,500 tokens of raw HTML. The same page through nab: ~500 tokens. That's 25x savings before your LLM even starts thinking.

I built this because every existing tool was either too slow, too fat, or required a cloud API key for something that should run locally.

nab is a 15MB Rust binary. 50ms fetch times. Your actual browser cookies for authenticated content. TLS fingerprint impersonation for sites that block non-browser clients. HTTP/3, QuickJS for JS-heavy pages, PDF extraction. 11 site-specific providers that use APIs instead of scraping.

If you're building anything with LLMs that touches the web, every token wasted on HTML boilerplate is money burned.

785 tests. MIT licensed.

https://github.com/MikkoParkkola/nab

#rust #llm #opensource


## Post 2: mcp-gateway

I gave Claude Code access to 100+ tools. It became unusable.

Not because the tools were bad. The tool definitions alone consumed ~15,000 tokens of context window. Every single request. Before any work happened.

mcp-gateway replaces all tool definitions with 4 meta-tools (~400 tokens). Your AI discovers tools on demand instead of loading everything upfront. 97% context savings.

Any REST API becomes an MCP tool by dropping a YAML file. 42 built-in capabilities work out of the box. Circuit breakers, rate limiting, hot-reload, OpenAPI auto-import.

The alternative isn't "use fewer tools." It's losing capabilities. This keeps everything accessible without the context tax.

1,369 tests. 7.1 MB binary. MIT licensed.

https://github.com/MikkoParkkola/mcp-gateway

#mcp #llm #rust #opensource
