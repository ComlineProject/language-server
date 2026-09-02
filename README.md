# Comline Language Server

A Language Server Protocol (LSP) implementation for [Comline](https://github.com/Kinflou/comline), providing intelligent code editing features for `.ids` schema files.

## Features

### ✅ Fully Implemented

- **Diagnostics** - Real-time syntax errors plus `comline-core`'s validation pass (undefined types, duplicate declarations, ...) — the same checks `comline build` runs
- **Document Symbols** - Hierarchical outline view of structs, enums, protocols, and constants
- **Hover Information** - Rich tooltips showing full type definitions and signatures
- **Go to Definition** - Jump from type references to their declarations
- **Find References** - Locate all usages of a symbol (with include/exclude declaration option)
- **Auto-Completion** - Context-aware code suggestions (keywords, primitives, user types)
- **Semantic Tokens** - Comline syntax highlighting from a shared lexer

### 🚧 Rougher / next

- Rename Symbol, Code Formatting, Signature Help, Code Actions — present but thin
- Cross-file `use` resolution (analysis is single-file today)
- AST-accurate spans (declaration ranges use a text-search heuristic)

## Library

The crate is also an **analysis library**. `--no-default-features` drops the
`server` feature (`tower-lsp`, `tokio`, the doc store, the `comline-lsp` bin),
leaving `parser` + `analysis` + `handlers` over `lsp-types` and `comline-core` —
which **builds for `wasm32-unknown-unknown`**. The Comline playground links it
that way so its browser editor runs the *same* diagnostics, hover, completion
and highlighting as the LSP.

## Installation

### Building from Source

```bash
cd language-server
cargo build --release
```

The LSP server binary will be available at `target/release/comline-lsp`.

## Editor Integration

### Visual Studio Code

1. Install a generic LSP client extension (e.g., `vscode-languageclient`)
2. Configure the server in your settings:

```json
{
  "comline.server": {
    "command": "/path/to/comline-lsp",
    "args": [],
    "transport": "stdio"
  }
}
```

### Neovim

Using `nvim-lspconfig`:

```lua
local lspconfig = require('lspconfig')
local configs = require('lspconfig.configs')

-- Define Comline LSP
if not configs.comline_ls then
  configs.comline_ls = {
    default_config = {
      cmd = {'/path/to/comline-lsp'},
      filetypes = {'comline'},
      root_dir = lspconfig.util.root_pattern('.git', 'config.idp'),
      settings = {},
    },
  }
end

-- Setup Comline LSP
lspconfig.comline_ls.setup{}

-- Associate .ids files with comline filetype
vim.filetype.add({
  extension = {
    ids = 'comline',
  },
})
```

### Helix

Add to your `languages.toml`:

```toml
[[language]]
name = "comline"
scope = "source.comline"
file-types = ["ids"]
roots = ["config.idp", ".git"]
language-servers = ["comline-ls"]

[language-server.comline-ls]
command = "/path/to/comline-lsp"
```

### Emacs (lsp-mode)

```elisp
(add-to-list 'lsp-language-id-configuration '(comline-mode . "comline"))

(lsp-register-client
 (make-lsp-client
  :new-connection (lsp-stdio-connection "/path/to/comline-lsp")
  :major-modes '(comline-mode)
  :server-id 'comline-ls))
```

## Usage

Once integrated with your editor, the LSP server provides:

### Real-time Diagnostics

Syntax errors are highlighted as you type:

```comline
struct User {
    name string  // ← Error: Missing colon
    age: i32
}
```

### Outline View

Navigate your schema using the document symbols panel:

```
📦 User (3 fields)
  ├─ 📄 name
  ├─ 📄 age
  └─ 📄 email
🔌 UserService (2 functions)
  ├─ ⚡ getUser
  └─ ⚡ createUser
```

### Hover Information

Hover over any symbol to see its definition:

```comline
struct User {
  name: string
  age: i32
  optional email: string
}

3 fields
```

### Go to Definition

Ctrl/Cmd + Click on any type reference to jump to its definition:

```comline
struct User { ... }

struct Request {
    user: User  // ← Click jumps to User definition
}
```

## Development

### Project Structure

```
src/
├── main.rs              # Entry point
├── backend.rs           # LSP protocol implementation
├── document.rs          # Document management
├── parser.rs            # Comline parser integration
├── util.rs              # Utility functions
├── analysis/            # Semantic analysis
│   ├── diagnostics.rs   # Error reporting
│   ├── symbols.rs       # Symbol extraction
│   ├── types.rs         # Type resolution
│   └── imports.rs       # Import resolution
└── handlers/            # LSP feature handlers
    ├── completion.rs
    ├── definition.rs
    ├── formatting.rs
    ├── hover.rs
    ├── references.rs
    ├── rename.rs
    └── symbols.rs
```

### Running Tests

```bash
cargo test
```

All tests should pass:
- Parser tests
- Diagnostic tests
- Symbol extraction tests
- Handler tests
- Integration tests

### Logging

Set the `RUST_LOG` environment variable for debugging:

```bash
RUST_LOG=debug /path/to/comline-lsp
```

Logs are written to stderr and won't interfere with the LSP protocol communication over stdio.

## Technical Details

### LSP Capabilities

The server advertises the following capabilities:

- **Text Document Sync** - Full document synchronization
- **Hover Provider** - Type information on hover
- **Completion Provider** - Trigger characters: `.`, `:`
- **Definition Provider** - Go to definition support
- **References Provider** - Find all references (planned)
- **Document Symbol Provider** - Outline view
- **Workspace Symbol Provider** - Global symbol search (planned)
- **Document Formatting** - Auto-formatting (planned)
- **Rename Provider** - Symbol renaming (planned)
- **Semantic Tokens** - Enhanced highlighting (planned)

### Dependencies

- **tower-lsp** - LSP protocol framework
- **comline-core** - Comline parser and AST
- **rust-sitter** - Parser infrastructure
- **tokio** - Async runtime
- **dashmap** - Concurrent hashmap for document storage
- **tracing** - Structured logging

## Contributing

Contributions are welcome! Areas for improvement:

1. **Find References** - Implement reference finding across files
2. **Auto-Completion** - Add intelligent completion suggestions
3. **Import Resolution** - Cross-file type resolution
4. **Code Actions** - Quick fixes and refactorings
5. **Formatting** - Implement Comline code formatter

## License

See the main [Comline project](https://github.com/Kinflou/comline) for licensing information.

## Acknowledgments

Built using:
- [tower-lsp](https://github.com/ebkalderon/tower-lsp) - LSP framework
- [Comline](https://github.com/Kinflou/comline) - The Comline language
