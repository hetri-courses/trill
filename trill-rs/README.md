# Trill CLI (Rust Implementation)

We provide Trill CLI as a standalone, native executable to ensure a zero-dependency install.

## Installing Trill

Today, the easiest way to install Trill is via `npm`:

```shell
npm i -g @hetri-courses/trill
codex
```

You can also install via Homebrew (`brew install --cask codex`) or download a platform-specific release directly from our [GitHub Releases](https://github.com/hetri-courses/trill/releases).

## Documentation quickstart

- First run with Trill? Start with [`docs/getting-started.md`](../docs/getting-started.md) (links to the walkthrough for prompts, keyboard shortcuts, and session management).
- Want deeper control? See [`docs/config.md`](../docs/config.md) and [`docs/install.md`](../docs/install.md).

## What's new in the Rust CLI

The Rust implementation is now the maintained Trill CLI and serves as the default experience. It includes a number of features that the legacy TypeScript CLI never supported.

### Config

Trill supports a rich set of configuration options. Note that the Rust CLI uses `config.toml` instead of `config.json`. See [`docs/config.md`](../docs/config.md) for details.

### Model Context Protocol Support

#### MCP client

Trill CLI functions as an MCP client that allows the Trill CLI and IDE extension to connect to MCP servers on startup. See the [`configuration documentation`](../docs/config.md#connecting-to-mcp-servers) for details.

#### MCP server (experimental)

Trill can be launched as an MCP _server_ by running `trill mcp-server`. This allows _other_ MCP clients to use Trill as a tool for another agent.

Use the [`@modelcontextprotocol/inspector`](https://github.com/modelcontextprotocol/inspector) to try it out:

```shell
npx @modelcontextprotocol/inspector trill mcp-server
```

Use `trill mcp` to add/list/get/remove MCP server launchers defined in `config.toml`, and `trill mcp-server` to run the MCP server directly.

### Notifications

You can enable notifications by configuring a script that is run whenever the agent finishes a turn. The [notify documentation](../docs/config.md#notify) includes a detailed example that explains how to get desktop notifications via [terminal-notifier](https://github.com/julienXX/terminal-notifier) on macOS. When Trill detects that it is running under WSL 2 inside Windows Terminal (`WT_SESSION` is set), the TUI automatically falls back to native Windows toast notifications so approval prompts and completed turns surface even though Windows Terminal does not implement OSC 9.

### `trill exec` to run Trill programmatically/non-interactively

To run Trill non-interactively, run `trill exec PROMPT` (you can also pass the prompt via `stdin`) and Trill will work on your task until it decides that it is done and exits. Output is printed to the terminal directly. You can set the `RUST_LOG` environment variable to see more about what's going on.

### Experimenting with the Trill Sandbox

To test to see what happens when a command is run under the sandbox provided by Trill, we provide the following subcommands in Trill CLI:

```
# macOS
trill sandbox macos [--full-auto] [--log-denials] [COMMAND]...

# Linux
trill sandbox linux [--full-auto] [COMMAND]...

# Windows
trill sandbox windows [--full-auto] [COMMAND]...

# Legacy aliases
trill debug seatbelt [--full-auto] [--log-denials] [COMMAND]...
trill debug landlock [--full-auto] [COMMAND]...
```

### Selecting a sandbox policy via `--sandbox`

The Rust CLI exposes a dedicated `--sandbox` (`-s`) flag that lets you pick the sandbox policy **without** having to reach for the generic `-c/--config` option:

```shell
# Run Trill with the default, read-only sandbox
trill --sandbox read-only

# Allow the agent to write within the current workspace while still blocking network access
trill --sandbox workspace-write

# Danger! Disable sandboxing entirely (only do this if you are already running in a container or other isolated env)
trill --sandbox danger-full-access
```

The same setting can be persisted in `~/.trill/config.toml` via the top-level `sandbox_mode = "MODE"` key, e.g. `sandbox_mode = "workspace-write"`.

## Code Organization

This folder is the root of a Cargo workspace. It contains quite a bit of experimental code, but here are the key crates:

- [`core/`](./core) contains the business logic for Trill. Ultimately, we hope this to be a library crate that is generally useful for building other Rust/native applications that use Trill.
- [`exec/`](./exec) "headless" CLI for use in automation.
- [`tui/`](./tui) CLI that launches a fullscreen TUI built with [Ratatui](https://ratatui.rs/).
- [`cli/`](./cli) CLI multitool that provides the aforementioned CLIs via subcommands.
