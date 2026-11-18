<p align="center">
  <img src="assets/flint-logo.svg" alt="Flint Logo" width="96" height="96" />
</p>

# Flint

**Flint** — a GPU-accelerated IDE built with Rust and **GPUI** (`gpui-kit`) for **AI Fill-In-the-Middle (FIM) Auto Complete**, **Streaming ReAct Repository Exploration**, **LSP**, and **Vim Code Navigation**.

## Features

### AI FIM Auto Complete
- **Native inline & multi-line ghost text** rendered inside the GPUI code editor (`Tab` accepts full suggestion, `Esc` dismisses, `⌥→` accepts next word, `⌥↓` accepts next line, `⌥\` manually triggers, `⌥]` / `⌥[` cycles candidates).
- **Cross-file repository context injection**: queries the live Tree-sitter `WorkspaceIndex` around the cursor and injects referenced definitions, signatures, and struct/enum members from other workspace files into the FIM prompt.
- **Multiple FIM backends** (configured via `⌘,` or `~/.config/kestrel/config.json`):
  - **Auto**: uses configured/env API keys (`DEEPSEEK_API_KEY`, `CODESTRAL_API_KEY`, `OPENAI_API_KEY`, `OLLAMA_HOST`, `KESTREL_FIM_ENDPOINT`) and seamlessly falls back to the offline AST engine.
  - **Local AST & Symbols**: built-in offline FIM generator using Tree-sitter syntax + `WorkspaceIndex` (struct literal field scaffolding, enum `match` arm generation, dot/scope members, call argument hints, doc comments, block continuations).
  - **Ollama** (`/api/generate` with `prompt` + `suffix`)
  - **OpenAI-Compatible FIM** (`/completions`)
  - **DeepSeek FIM** (`https://api.deepseek.com/beta/completions`)
  - **Mistral Codestral FIM** (`https://codestral.mistral.ai/v1/fim/completions`)
  - **Chat FIM** (`/chat/completions`)
- **AI FIM Workbench** (Right Dock → `AI FIM`): live telemetry, latency, cache hits, active suggestion controls, cross-file context snippets, prefix/suffix window preview, and trace history.

### Ask AI, Code Links & ReAct Repo Explorer
- **Dedicated Ask AI Panel** (Right Dock → `Ask AI`, toggle via `Ask AI ⌘L` or `Explain Sel ⌘⇧E`): interactive code Q&A and Markdown explanations with syntax-highlighted code blocks (`TextView::markdown`).
- **Clickable Code Links**: citations (`path:L<line>` / `[path:L<line>](kestrel://open?...)`) inside AI answers and ReAct trace steps jump straight to the exact file and line in the editor (and record Vim/`Ctrl+o` jump history).
- **Streaming ReAct Repository Exploration**: streams thoughts (`reasoning_content`), autonomous read-only repository tool calls (`list_files`, `search_symbols`, `grep_text`, `read_lines`, `find_references`), tool observations, and final answer tokens incrementally via OpenAI/DeepSeek SSE, Ollama NDJSON, or the offline Local AST ReAct explorer (`ReAct ON/OFF` toggle).
- **Conversation Isolation Management**: maintain independent parallel threads (`+ New`, `Fork`, close `×`, rename) where each conversation owns its own isolated message history, attached code contexts, ReAct toggle, and background streaming task without cross-thread leakage.
- **Selection-Driven Code Context**:
  - Automatically tracks your active editor selection (mouse drag, `Shift+arrows`, or Vim `v`/`V` Visual mode) with exact file path, 1-based line range, enclosing symbol, and cross-file workspace definitions/diagnostics.
  - Pin selections from one or multiple files with `⌘L` / `+ Add Selection`, expand/inspect attached snippets inline, or jump back to referenced code lines on click.
  - Trigger quick actions (**Explain** `⌘⇧E`, **Review**, **Tests**, **Fix**) or type custom questions (`Enter` to ask, `⇧Enter` for newline), plus Vim `:ask [question]`, `:explain`, and `g?`.
- **Configurable Backends**: automatically adapts your configured FIM provider/API key (for example DeepSeek `https://api.deepseek.com/chat/completions` + `deepseek-chat`, OpenAI Chat, Mistral, or Ollama `/api/chat`), with offline Local AST ReAct Explorer fallback when no API key is set.

### Language Server Protocol (LSP)
- **Configurable stdio LSP client** (`~/.config/kestrel/config.json`, per-workspace `.kestrel/config.json`, or **Settings (`⌘,`) → LSP Servers**) supporting multiple simultaneous servers per language.
- Built-in server presets for `rust-analyzer` (Rust), `typescript-language-server` (TypeScript/TSX/JavaScript), **`ty` (`ty server`) + `ruff` (`ruff server`) + `pyright-langserver` (Python)**, `gopls` (Go), and `clangd` (C/C++), plus adding/editing/restarting custom language servers.
- **Live Capabilities**: `initialize` / `didOpen` / `didChange` / `didSave` / `didClose`, both push (`textDocument/publishDiagnostics`) and LSP 3.17 pull (`textDocument/diagnostic`) diagnostics merged across active servers, `textDocument/definition`, `textDocument/declaration`, `textDocument/typeDefinition`, `textDocument/implementation`, `textDocument/references`, `textDocument/hover`, `textDocument/completion`, and `textDocument/codeAction`, with seamless fallback to the Tree-sitter `WorkspaceIndex` when a server is starting or unavailable.

### Built-in Vim Mode
- **Modal editing** (`Normal`, `Insert`, `Visual`, `VisualLine`, `Replace`, `Command`) integrated directly into the GPUI editor with block-frame Normal/Replace cursor decorations, inline search match highlights, and live status bar mode/pending/command indicators.
- **Motions & Operators**: `h/j/k/l`, `w/b/e/W/B/E`, `0/^/$/g_`, `gg/G`, `%`, `{/}`, `[{`/`]}`, `[(`/`])`, `f/F/t/T/;/,`, `/` & `?` search (`n/N/*/#`), `zz/zt/zb`, counts, `i/I/a/A/o/O`, `d/c/y/>/<` + text objects (`iw/aw`, `i"/a"`, `i'/a'`, ``i`/a` ``, `i(/i[/i{`), `x/X/s/S/D/C/Y/r/R/J/~/p/P`, `.` dot-repeat, `u`/`Ctrl+r` undo/redo, and system clipboard sync.
- **Code & Jump Navigation**: `gd` (Definition), `gD` (Declaration), `gy` (Type Definition), `gI` (Implementation), `gr` (References), `gf` (Go to File/Module), `gi` (Last Insert), `ga` (Code Actions), `gO` (Outline), `K` (Hover Info), `Ctrl+o`/`Ctrl+i` (Jump Back/Forward), `]d`/`[d` (Diagnostics), `]e`/`[e` (Errors), `]s`/`[s`/`]]`/`[[`/`]c`/`[c` (Symbols), `gt`/`gT`/`{n}gt`/`]b`/`[b` (Tabs), and Ex commands (`:w`, `:q`, `:q!`, `:wq`, `:<line>`, `:e [file]`, `:fim`, `:def`, `:decl`, `:type`, `:impl`, `:refs`, `:hover`, `:outline`, `:jumps`, `:tabs`, `:bn`, `:bp`, `:noh`, `:set vim` / `:set novim`), plus configurable options in **Settings (`⌘,`) → Vim Mode**.

### Code Navigation, Hints & Ergonomics
- **Tree-sitter Workspace Index**: indexes source files across Rust, TypeScript/TSX, JavaScript, Python, Go, C/C++, TOML, JSON, YAML, Markdown, Bash, CSS, and HTML, re-indexing live on every edit.
- **Global Regex Symbol Search**: search workspace symbols in **Go to Symbol (`⌘⇧O` / `⌘T`)** via fuzzy matching or regex (`re:<pattern>`, `/<pattern>/`, or toggle `.*` via `⌥R`/`⌘R`).
- **Inlay Hints & Inline Hover Docstrings**: displays LSP `textDocument/inlayHint` (plus offline AST type/parameter hints, toggle `:set [no]inlay`), inline cursor-line docstring summaries for JSDoc/Python/Rust symbols (`:set [no]inlinedoc`), and inline Markdown hover popovers (`K`).
- **Scroll Padding & Smooth Navigation**: configurable scroll margin (`scroll_padding` / `:set scrolloff=<n>`, plus `zz`/`zt`/`zb`) in the editor, and arrow-key auto-scrolling with top/bottom item padding inside Quick Open & Symbol palettes.
- **Go to Definition** (`F12` / `⌘Click`): resolves definitions across workspace files, opening target files or showing a multi-definition picker when multiple targets match.
- **Find All References** (`⇧F12`): lists definitions, call sites, and identifier references across all workspace files in the bottom Navigation Results dock.
- **Hover Info**: Markdown hover popovers showing symbol kind, signature, source location, reference count, members, and doc comments.
- **Code Map** (Right Dock → `Code Map`): live structural view of the enclosing scope, cursor symbol definition, outgoing referenced symbols, incoming callers/references, and file imports.
- **Document Outline & Interactive Breadcrumbs**: hierarchical symbol outline tracking the enclosing symbol at the cursor + clickable breadcrumbs bar.
- **Navigation History & Tab Switching**: Jump Back (`⌘⌥←` / Vim `Ctrl+o`) / Forward (`⌘⌥→` / Vim `Ctrl+i`) across file and symbol jumps, plus `⌘1`–`⌘9` (tabs 1–9) and `⌘<` / `⌘>` (previous / next tab).
- **Quick Open (`⌘P`) & Go to Symbol (`⌘⇧O` / `⌘T`)**: fuzzy and regex file/workspace symbol palettes with padded arrow-key scrolling.

## Running

```bash
cargo run -- /path/to/project
```

## Testing

```bash
cargo test
```
