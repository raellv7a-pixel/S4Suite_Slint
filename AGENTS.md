<!-- prowl-agent -->
## Prowl Agent (code intelligence)

This project is indexed by **prowl-agent**. Query the index from your shell
instead of grepping and reading whole files. Answers are cited (file:line),
ranked, and token-lean (TOON format, ~40% smaller than JSON, and read more
accurately by models). The index refreshes itself on each call, so there is
nothing to start and nothing goes stale.

**Prefer a prowl-agent query before reading files manually.** Open a raw file
only after a query points you to the exact lines.

### Commands

    prowl-agent overview            # project map: docs to read, roles, entrypoints, clusters (start here)
    prowl-agent find <name>         # locate a symbol; returns its signature, file, and line range
    prowl-agent search <text>       # search content; add --smart (rerank) or --compact (files only)
    prowl-agent callers <path>      # what includes / execs / binds to a file
    prowl-agent callees <path>      # what a file includes / execs / binds to
    prowl-agent impact <path>       # blast radius: dependent count, subsystems, direct importers (--all = full list)
    prowl-agent relations <path>    # a file's symbols and include neighbors
    prowl-agent entrypoints <path>  # root files from which this file is reachable
    prowl-agent references <id>     # where a symbol is used: call sites + calling fn, or ref edges (id from 'find')
    prowl-agent clusters [name]     # subsystems (summaries); with a name, that subsystem's files
    prowl-agent hotspots            # structurally central / large files
    prowl-agent violations          # dangling refs, orphan scripts, hardcoded colors
    prowl-agent doctor              # health: cycles, duplicate keybinds, broken commands
    prowl-agent tests <path>        # test files covering a file (or, for a config, what launches it)
    prowl-agent changed             # your git changes mapped to the files they could affect

Every command accepts --json for JSON instead of TOON, and --limit N to cap
results (fewer tokens). Run from anywhere inside the project; prowl-agent finds
the index by walking up to .prowl/.

### When to use which

- New or unfamiliar project: overview for the map, then clusters <name> to pull a subsystem's files.
- After a find: the row carries the signature, line, and end_line, so read the signature for a symbol's interface and open only that line range when you need the body.
- Fuzzy / natural-language question: search "<text>" matches all terms when the exact phrase is absent; --smart reranks semantically (needs AI), --compact lists files first.
- Before changing any symbol (a function, a color, a variable): find it, then references <id> for its usages (cited call sites for code, reference edges for config); check violations.
- Before editing or deleting a file: impact <path> for what breaks, callers <path> for what invokes it.
- Adding a keybind: doctor first, to avoid duplicate-keybind clashes.
- Tracing startup: entrypoints <path> for the entry point and autostart chain.
- After editing, or before committing: changed to see what your edits could affect, then doctor.

The same index is also available over MCP (server: `prowl-agent serve`) for
agents that prefer typed tools, but the shell commands above are the
recommended, lowest-overhead path (no server, no per-call schema cost).
<!-- /prowl-agent -->
