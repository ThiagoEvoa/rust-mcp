# rust-docs-mcp

Give your AI coding assistant access to official Rust documentation, including
Rust’s API and style guides, Cargo, Clippy, and other Rust tools.

It connects through MCP (Model Context Protocol), which lets AI apps use extra
tools. Documentation is included, so it works offline after installation. No API
key or account is needed for this server.

## Install

You’ll need an internet connection for installation and an AI app that supports
local MCP servers.

### 1. Install Rust

Follow the [official Rust installer](https://rust-lang.org/tools/install/) and
choose the default installation. If Rust is already installed, skip this step.

Rust also needs your computer’s build tools:

- **macOS:** open **Terminal**, run `xcode-select --install`, and follow the prompts.
- **Ubuntu/Debian:** open **Terminal** and run `sudo apt install build-essential`.
  For other Linux systems, follow the
  [Rust setup guide](https://doc.rust-lang.org/book/ch01-01-installation.html).
- **Windows:** accept the Rust installer’s offer to install Visual Studio build tools.

Close and reopen Terminal (or **PowerShell** on Windows) before continuing.

### 2. Install rust-docs-mcp

Copy and paste each line below into that window, pressing **Enter** after each:

```sh
rustup update stable
cargo +stable install --locked --git https://github.com/ThiagoEvoa/rust-mcp rust-docs-mcp
rust-docs-mcp --version
```

Installation may take a few minutes. When the last command prints
`rust-docs-mcp` followed by a version number, it’s installed.

### 3. Connect your AI app

Find the installed program’s full path by running:

- **macOS/Linux:** `command -v rust-docs-mcp`
- **Windows (PowerShell):** `(Get-Command rust-docs-mcp).Source`

In your AI app’s MCP settings, add a local server named **rust-docs**. Use the
path printed above as the **command** and `serve` as its **argument**.

If your app uses a JSON configuration, add this server to its `mcpServers`
section, replacing the example path with yours. On Windows, use forward slashes
in the path, for example `C:/Users/you/.cargo/bin/rust-docs-mcp.exe`.

```json
{
  "mcpServers": {
    "rust-docs": {
      "command": "/Users/you/.cargo/bin/rust-docs-mcp",
      "args": ["serve"]
    }
  }
}
```

Restart your AI app, then ask it: **“Use rust-docs to find the Rust API guidelines.”**
Your app starts the server for you; no separate launch or documentation download
is needed.

For connection help, see the [client setup guide](docs/CLIENTS.md). For updates
and troubleshooting, see the [operations guide](docs/OPERATIONS.md).
