# Trill CLI

**Trill** is a fork of OpenAI's Codex CLI designed for local LLM usage with LM Studio and local web search via SearXNG.

## Overview

Trill enables local LLM providers (like LM Studio) to use web search functionality through a local SearXNG instance, without requiring OpenAI's server-side web search. All tools are registered as `type: "function"` for full LM Studio compatibility.

### Features

- **Local LLM Support**: Works with LM Studio, Ollama, and other OpenAI-compatible APIs
- **Local Web Search**: Replaces OpenAI's server-side `web_search` with local SearXNG execution
- **LM Studio Compatible**: All tools registered as `type: "function"` (not `web_search`, `local_shell`, or `custom`)
- **Full WebSearchAction Support**: Search, OpenPage, FindInPage actions
- **Configurable SearXNG**: URL configurable via `~/.trill/config.toml`
- **Rich Metadata**: Includes engine, score, category, and publishedDate fields

## Quick Start

### Prerequisites

- Rust toolchain (for building)
- LM Studio running at `http://192.168.0.137:1234` (or configure your endpoint)
- SearXNG Docker container at `http://127.0.0.1:8080`

### Installation

```bash
# Clone the repo
git clone https://github.com/yourusername/trill.git
cd trill/trill-rs

# Build release binary
cargo build --release

# Install
sudo cp target/release/trill /usr/local/bin/

# Verify
trill --version
```

### Configuration

Create `~/.trill/config.toml`:

```toml
model = "qwen2.5-coder-32b-instruct"
model_provider = "lmstudio"
searxng_url = "http://127.0.0.1:8080"
web_search_mode = "live"
approval_policy = "on-failure"

[model_providers.lmstudio]
base_url = "http://192.168.0.137:1234/v1"
```

### SearXNG Setup

```bash
# Create config directory
mkdir -p ~/searxng-config

# Create settings.yml
cat > ~/searxng-config/settings.yml << 'EOF'
use_default_settings: true

server:
  secret_key: "your-secret-key-here"

search:
  formats:
    - html
    - json
EOF

# Start SearXNG
docker run -d --name searxng \
  -p 8080:8080 \
  -v ~/searxng-config:/etc/searxng \
  searxng/searxng:latest
```

## Usage

```bash
# Interactive mode
trill

# Single prompt
trill "explain this codebase"

# With specific model
trill -m qwen2.5-coder-32b-instruct "your prompt"

# OSS mode (local models)
trill --oss "your prompt"
```

## Architecture

```
+-------------------+       +-------------------+       +-------------------+
|     User CLI      |       |    LM Studio      |       |     SearXNG       |
|                   |       |                   |       |                   |
|  trill [prompt]   +------>+  Local LLM API    |       |  Docker container |
|                   |       |  192.168.0.137    |       |  127.0.0.1:8080   |
+--------+----------+       |  :1234/v1         |       +--------+----------+
         |                  +-------------------+                ^
         |                                                       |
         +-------------------------------------------------------+
         |  web_search tool calls route to SearXNG locally       |
         +-------------------------------------------------------+
```

### Key Differences from Codex

| Feature | Codex | Trill |
| --- | --- | --- |
| Web Search | OpenAI server-side | Local SearXNG |
| Tool Types | `web_search`, `local_shell`, `custom` | All `function` type |
| LLM Backend | OpenAI API | LM Studio, Ollama, etc. |
| Dependencies | Internet required | Fully local capable |

## Configuration Options

| Option | Default | Description |
| --- | --- | --- |
| `model` | (none) | Model name to use |
| `model_provider` | (none) | Provider from model_providers |
| `searxng_url` | `http://127.0.0.1:8080` | SearXNG endpoint |
| `web_search_mode` | (none) | `live` or `cached` |
| `approval_policy` | `on-request` | Tool approval mode |

### Approval Policies

- `untrusted`: Ask for most actions
- `on-failure`: Auto-approve in sandbox, ask on failure
- `on-request`: Model decides when to ask
- `never`: Auto-approve all (use carefully)

## Documentation

- [Agent Rules](agent_rules.md) - Operational guidelines and architecture
- [Commands Reference](commands.md) - Full command documentation
- [Stack Runbook](stack-runbook.md) - Deployment and troubleshooting

## Development

```bash
# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug trill "your prompt"

# Check code
cargo check
cargo clippy
cargo fmt
```

## Troubleshooting

### SearXNG not responding
```bash
docker ps -a | grep searxng
docker logs searxng
docker restart searxng
```

### LM Studio connection refused
- Check if LM Studio server is running
- Verify firewall allows port 1234
- Test: `curl http://192.168.0.137:1234/v1/models`

### Web search returns empty results
- Verify `json` format is enabled in SearXNG settings.yml
- Test: `curl "http://127.0.0.1:8080/search?q=test&format=json"`

## License

This project is licensed under the Apache-2.0 License.

Based on [OpenAI Codex CLI](https://github.com/openai/codex).
