# Trill Implementation Plan

**Trill** is a fork of OpenAI's Codex CLI with local web search capabilities via SearXNG.

## Overview

### Goals
1. Fork Codex → Trill with full rebranding
2. Replace OpenAI's server-side `web_search` with local SearXNG execution
3. Maintain identical response format (`WebSearchCall`, `WebSearchAction`)
4. Support all WebSearchAction types: Search, OpenPage, FindInPage
5. Make SearXNG URL hardcoded with config override option

### Architecture

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

## Phase 1: Branding (Codex → Trill)

### Files to Rename/Modify

| Current | New |
|---------|-----|
| `codex-rs/` | `trill-rs/` |
| `codex-cli/` | `trill-cli/` |
| `~/.codex/` | `~/.trill/` |
| Binary: `codex` | Binary: `trill` |
| Package names in Cargo.toml | `trill-*` |

## Phase 2: Web Search Implementation

### Data Structures

```rust
// WebSearchAction - describes the operation type
pub enum WebSearchAction {
    Search { query: Option<String>, queries: Option<Vec<String>> },
    OpenPage { url: Option<String> },
    FindInPage { url: Option<String>, pattern: Option<String> },
    Other,
}

// WebSearchCall - response item returned to model
WebSearchCall {
    id: Option<String>,
    status: Option<String>,  // "completed", "in_progress"
    action: Option<WebSearchAction>,
}
```

### Tool Registration

Register `web_search` as `type: "function"` (not `type: "web_search"`) so LM Studio accepts it.

The model calls it like any function, Trill handles it locally via SearXNG.

## Phase 3: Configuration

```toml
# ~/.trill/config.toml

[web_search]
provider = "searxng"
searxng_url = "http://192.168.0.137:8080"
include_metadata = true
timeout_seconds = 30
```

## Phase 4: SearXNG Response Mapping

SearXNG provides extra fields:
- `engine`: Primary source engine
- `engines`: All engines that returned this result  
- `score`: Cross-engine relevance score
- `category`: Content type
- `publishedDate`: For time-sensitive queries

These are mapped to the WebSearchCall response with optional metadata inclusion.

## Implementation Order

1. **Branding**: Rename Codex → Trill throughout codebase
2. **Config**: Add web search configuration options
3. **Handler**: Create web_search handler with SearXNG integration
4. **Tool Registration**: Register as function tool
5. **Build & Test**: Verify functionality
