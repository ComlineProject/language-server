# Comline Language Server

A complete Language Server Protocol (LSP) implementation for [Comline](https://github.com/ComlineProject), an Interface Definition Language (IDL) for cross-language RPC and code generation.

## Features

🚧 **Work in Progress** 🚧

This LSP server is currently under active development. Planned features include:

- ✅ **Real-time Diagnostics** - Syntax and semantic error detection
- 🚧 **Code Intelligence**
  - Auto-completion for keywords, types, and fields
  - Hover information with type signatures
  - Go-to-definition for types and symbols
  - Find all references
- 🚧 **Refactoring**
  - Rename symbol across files
  - Code actions and quick fixes
- 🚧 **Navigation**
  - Document symbols (outline view)
  - Workspace symbol search
- 🚧 **Formatting** - Automatic code formatting

## Building

```bash
cargo build --release
```

The LSP server binary will be available at `target/release/comline-lsp`.

## Running

The language server communicates via standard input/output:

```bash
./target/release/comline-lsp
```

## Editor Integration

### VS Code

Create a VS Code extension or use the generic LSP client extension with this configuration:

```json
{
  "languageServerExample.server": {
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

if not configs.comline then
  configs.comline = {
    default_config = {
      cmd = {'/path/to/comline-lsp'},
      filetypes = {'comline', 'ids'},
      root_dir = lspconfig.util.root_pattern('.git', 'config.idp'),
    },
  }
end

lspconfig.comline.setup{}
```

## Development

### Project Structure

```
src/
├── main.rs           # Entry point
├── backend.rs        # LSP protocol implementation
├── document.rs       # Document management
├── parser.rs         # Comline parser integration
├── util.rs           # Utilities
├── analysis/         # Semantic analysis
│   ├── diagnostics.rs
│   ├── imports.rs
│   ├── symbols.rs
│   └── types.rs
└── handlers/         # LSP feature handlers
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

### Logging

Set the `RUST_LOG` environment variable to control logging verbosity:

```bash
RUST_LOG=debug ./target/release/comline-lsp
```

Logs are written to stderr, so they won't interfere with LSP communication.

## Comline Language

Comline (`.ids` files) supports:

- **Structs**: Data structures with fields
- **Enums**: Enumerated types  
- **Protocols**: Service definitions with functions
- **Constants**: Named constant values
- **Imports**: Modern `use` statements with glob and multi-import support

Example schema:

```comline
use std::validators::*

struct User {
    name: string
    age: i32
    optional email: string
}

enum UserRole {
    Admin
    User
    Guest
}

protocol UserService {
    function getUser(i64) -> User;
    function createUser(User) -> i64;
}
```

## Contributing

Contributions are welcome! Please check the [implementation plan](../brain/.../implementation_plan.md) for current progress and planned work.

## License

See the main Comline project for licensing information.
