# Trill CLI

**Trill** is a fork of OpenAI's Codex CLI with local web search capabilities via SearXNG.

## Overview

Trill enables local LLM providers (like LM Studio) to use web search functionality through a local SearXNG instance, without requiring OpenAI's server-side web search.

### Features

- **Local Web Search**: Replaces OpenAI's server-side `web_search` with local SearXNG execution
- **LM Studio Compatible**: Registers web_search as `type: "function"` for LM Studio compatibility
- **Full WebSearchAction Support**: Search, OpenPage, FindInPage actions
- **Configurable SearXNG**: Hardcoded URL with config override option
- **Rich Metadata**: Includes engine, score, category, and publishedDate fields

## Installation

```shell
# Build from source
cd trill-rs
cargo build --release
```

The binary will be at `target/release/trill`.

## Configuration

Create `~/.trill/config.toml`:

```toml
[web_search]
provider = "searxng"
searxng_url = "http://192.168.0.137:8080"
include_metadata = true
timeout_seconds = 30
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         LM Studio                                │
│  (Model sees web_search as type: "function", calls it normally) │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼ Function call: web_search(query)
┌─────────────────────────────────────────────────────────────────┐
│                           Trill                                  │
│                                                                  │
│  1. Receives function call for "web_search"                     │
│  2. Determines WebSearchAction type (Search/OpenPage/FindInPage)│
│  3. Executes via SearXNG or URL fetch                           │
│  4. Formats response as WebSearchCall with WebSearchAction      │
│  5. Returns to model                                            │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    SearXNG (Docker)                              │
│              http://192.168.0.137:8080                          │
│                                                                  │
│  Endpoints:                                                      │
│  - /search?q={query}&format=json  (for Search action)           │
└─────────────────────────────────────────────────────────────────┘
```

## License

This project is licensed under the Apache-2.0 License.

Based on [OpenAI Codex CLI](https://github.com/openai/codex).
