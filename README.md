# Comline Language Server

A Language Server Protocol (LSP) implementation for [Comline](https://github.com/Kinflou/comline), providing intelligent code editing features for `.ids` schema files.

## Features

### ✅ Fully Implemented

- **Diagnostics** - Real-time syntax errors plus `comline-core`'s validation pass (undefined types, duplicate declarations, ...) — the same checks `comline build` runs. A type from another file of the package used without a `use` gets a specific error with an **"Add `use ...`" quick fix**; a `use` that reaches one of the package's own schemas by a bare path (`use types::User` instead of `use package::types::User`) is an error with a **"Write `package::…`" quick fix**; a type written against the Rust model (`reference::not-in-scope`, `reference::qualifier-not-in-scope`, `reference::defined-twice`) is core's own error with its **quick fix**
- **Whole-package analysis** - every `.ids` file in a package's `src/` takes part, open or not, kept current through file watching; each package only sees its own files
- **Dependency packages** - the dependencies `config.idp` declares are indexed (path dependencies, and git pins `comline check` has fetched; the editor never fetches), so imports, hover, go-to-definition and completion work into them under the dependency's name. Unresolved imports are errors, worded like `comline check`'s, with a "did you mean" quick fix; imports of a dependency that isn't fetched yet are marked as not checked, and its `config.idp` entry says why. A dependency whose `@unverified` protocols aren't accepted is an error on its entry, with a quick fix that writes the `accept_unverified` line `comline deps accept` would
- **Standard library** - `use std::…` resolves against the std embedded in `comline-core`, like a read-only dependency: hover, go-to-definition (into virtual `comline-std:` documents, whose text clients fetch with the `comline/stdSource` request), completion, and unresolved-import errors worded like `comline check`'s (`std has no schema matching 'std::htp::Request' - did you mean 'std::http'?`)
- **Module docs** - a schema documents itself with `//!` lines at its top, a package with the same at the top of its `config.idp`. Hover any segment of a path (`std`, `validators` in `use std::validators::StringBounds`, or in a qualified type name) for that module's docs, the modules below it and the types it declares; `use` completion shows the same docs beside each module and declaration. `std` and each of its modules are documented this way
- **Document Symbols** - Hierarchical outline view of structs, enums, protocols, and constants
- **Workspace Symbols** - Fuzzy search over every schema in the workspace
- **Hover Information** - Rich tooltips showing full type definitions and signatures
- **Go to Definition / Find References / Rename** - Across files, following each file's `use` statements; rename updates `use` lines and leaves `as` aliases alone
- **Auto-Completion** - Context-aware code suggestions (keywords, primitives, user and imported types, `@annotation` keys, `use` paths); types from other files that aren't imported yet add their `use` line
- **Semantic Tokens** - Comline syntax highlighting from a shared lexer

### 🚧 Rougher / next

- Code Formatting, Signature Help — present but thin
- `std::` imports aren't checked (std isn't part of a build yet either)

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

`comline-lsp --version` prints the server's version and the `comline-core`
commit it was built against:

```
comline-lsp 0.1.0 (core 14ce54658455206d639ddc4fbe6d609d20292587)
```

The `initialize` result's `serverInfo.version` is the same text without the
name, so a client can tell which grammar and checks the running server has.
The commit is read from `Cargo.lock` at build time, else from `Cargo.toml`,
else it is `unknown`.

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

Hover over a struct/enum/protocol/const name to see its definition, a
per-field wire-size breakdown prefixed with each field's wire index, and
the field/variant/function count:

```comline
struct User {
  name: string
  age: i32
  optional email: string
}

wire size: variable

- #0 name: variable
- #1 age: 4 bytes
- #2 email: variable

3 fields
```

Hover over a field's own name for just that field: its type, wire index,
size, `optional`, and any annotations or default value it carries.

```comline
optional email: string

field #2 of `User` (3 fields)

size: variable

optional: yes
```

### Auto-Completion

Typing `@` suggests the keys that are actually known for wherever the
cursor is — a different list on a field, a function, or a protocol/struct's
own leading annotation, since the grammar permits `@key=value` in all four
spots but each reads a different set of keys:

```comline
struct Message {
    @v          // → validators
}

protocol Chat {
    @t          // → timeout_ms, idempotent
    function send(msg: string) -> bool;
}

@f              // → framing (before `protocol`; a no-op before `struct`)
protocol Chat { ... }
```

Each suggestion's detail line explains what the key does and where it's
actually consumed (`@idempotent` is honest that it's advisory-only today —
see [Per-call settings](https://github.com/ComlineProject/docs/blob/main/docs/docs/design/core-target-contract.md#per-call-settings--decided)).
This is a curated, known-good subset, not validation — `@key=value` stays
an open namespace; an unrecognised key still parses, freezes, and is
silently ignored by whatever doesn't act on it.

Hover over any key — typed yourself or picked from completion — for the
same information at a glance: what it does, its default when absent, the
value it expects, and what actually reads it.

```comline
@timeout_ms

How long the client waits for the response before timing out.
Request/response calls only — a one-way call has nothing to wait for.

default: no timeout — waits indefinitely

value: an integer, in milliseconds

consumed by: the generated Rust client (`comline-rust`)
```

A `use` path completes one segment at a time, resolved the way
`comline check` resolves it:

```comline
use                        // → its dependencies, self / parent / package, std
use package::              // → the package's own schemas
use shared::               // → what's under the `shared` dependency
use package::api::common:: // → what `api::common` declares, then `*` and `{…}`
use package::api::common::{Error,   // → what `api::common` declares that isn't listed yet
```

The package's own schemas are reached only as `package::…`, `parent::…` or
`self::…`, so they are offered under those and not at the top. A bare path to
one of them (`use types::User`) is the error `import::bare-own-schema-path`,
the same one `comline check` reports, and its quick fix writes `package::` in
front. The "Add `use …`" fix and a type's auto-import write `package::…` for the
package's own schemas, and a dependency's or `std`'s path as it is.

A dependency that isn't fetched yet is offered, marked as such, with nothing
under it until `comline check` fetches it.

A type is written as in Rust: a name in scope, or a path that starts with one or
with a root. A whole-namespace `use` brings in only its own name, so after
`use package::types` the type is `types::User` and a bare `User` is the error
`reference::not-in-scope` (fix: "Add `use package::types::User`", or write
`types::User`). A full path from a root needs no `use`: `package::types::User`,
`parent::common::Error`, `self::nested::Type`, `std::http::Request`, a
dependency's `greeting::hello::Greeting`; and a path goes down from a name in
scope (`billing::api::Invoice` after `use package::billing`, `b::Invoice` after
`use package::billing::api as b`). These are core's own rule and errors
(`reference::not-in-scope`, `reference::qualifier-not-in-scope`,
`reference::defined-twice`, each with its fix), run over the files the server
has. Completion in a type position follows it:

```comline
use package::types

struct S {
    a: types::     // → what `types` declares; a bare `User` is not offered
    b: package::   // → the package's schemas, then what's under them
    c:             // → the names in scope (declared here, `use`d, `types`), then
                   //   the roots package:: parent:: self:: std:: and the dependencies
}
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
- **Completion Provider** - Trigger characters: `.`, `:`, `(`, `>`, space, `@`
- **Definition Provider** - Go to definition, across files
- **References Provider** - Find all references, across files
- **Document Symbol Provider** - Outline view
- **Workspace Symbol Provider** - Global symbol search
- **Document Formatting** - Auto-formatting
- **Rename Provider** - Symbol renaming across files, with prepare-rename
- **Code Action Provider** - Quick fixes (add a missing `use`, apply an unresolved import's "did you mean", accept a dependency's `@unverified` protocols in `config.idp`)
- **Semantic Tokens** - Enhanced highlighting
- **Workspace Folders** - Multi-root workspaces; `workspace/didChangeWatchedFiles` keeps the index current

### Dependencies

- **tower-lsp** - LSP protocol framework
- **comline-core** - Comline parser and AST
- **rust-sitter** - Parser infrastructure
- **tokio** - Async runtime
- **dashmap** - Concurrent hashmap for document storage
- **tracing** - Structured logging

## Contributing

Contributions are welcome! Areas for improvement:

1. **Signature Help** - Function argument hints
2. **Formatting** - A fuller Comline code formatter
3. **Code Actions** - More quick fixes and refactorings

## License

See the main [Comline project](https://github.com/Kinflou/comline) for licensing information.

## Acknowledgments

Built using:
- [tower-lsp](https://github.com/ebkalderon/tower-lsp) - LSP framework
- [Comline](https://github.com/Kinflou/comline) - The Comline language
