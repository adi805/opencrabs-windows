# Connecting a GUI client to OpenCrabs over ACP

OpenCrabs ships **no first-party GUI** — the TUI is the whole product, and that is
deliberate. If you want a window with tabs and a file tree, you point a third-party
editor or agent harness at the built-in ACP server.

This guide uses **[MonoCode](https://github.com/hardbeat920/monocode)** (MIT, Tauri
desktop app; macOS, Linux, Windows) because it ships an ACP JSON-RPC client and runs
OpenCrabs in daily use. Any ACP client works the same way: the client provider lists
are **presets, not capability gates**, so a custom agent entry is all you need.

## 1. Check the server is there

```bash
opencrabs acp --help
```

`opencrabs acp` serves the Agent Client Protocol over **stdio** — stdout carries
JSON-RPC frames and nothing else, so the client must be the one to spawn it. It takes
exactly one flag:

| Flag | Meaning |
|---|---|
| `--model <model>` | Default model override for sessions on this server. Omit to use the model from your `config.toml`. |

There is nothing to start by hand and no port to open. One process is spawned per
editor thread; sessions persist in the normal session store, so `session/load`
replays the transcript and restores that session's model.

## 2. Install MonoCode

Follow the install instructions in the [MonoCode repository](https://github.com/hardbeat920/monocode).

## 3. Add OpenCrabs as a custom ACP agent

In MonoCode, open the agent / provider settings and add a **custom ACP agent**. Point
the command at the `opencrabs` binary and pass `acp` as the subcommand:

```json
{
  "name": "OpenCrabs",
  "command": "opencrabs",
  "args": ["acp"]
}
```

With a model override:

```json
{
  "name": "OpenCrabs",
  "command": "opencrabs",
  "args": ["acp", "--model", "combo/workhorse"]
}
```

Two things that bite people:

- **Use the absolute path if `opencrabs` is not on the client's `PATH`.** A GUI app
  launched from a desktop icon inherits a login environment, not your shell's. If the
  client reports the agent failed to start, replace `"command": "opencrabs"` with the
  full path (e.g. `"command": "/home/you/.local/bin/opencrabs"`) and try again.
- **Do not wrap the command in a shell.** The client speaks JSON-RPC over the
  process's stdio; `sh -c "opencrabs acp"` still works, but anything that prints a
  banner to stdout first will corrupt the protocol.

## 4. Verify the session opens

Open a new agent session in MonoCode. A working connection gives you:

- the session opens without an error banner, and the context meter shows a usage
  figure (it is restored from the session store on load, so a resumed session does
  not start at zero);
- a prompt sent from the GUI streams back tool calls and text;
- `/compact` in the GUI pushes through as `session/compact`.

If the session opens but every prompt fails, the ACP layer is fine and the problem is
your provider credentials — check `keys.toml` and run `opencrabs doctor`.

## What stays the same

The ACP server is the *same agent* the TUI runs, with the same brain files, memory,
sessions and database. Approval policy is applied server-side via
`session/set_mode`, so `/approve` in a terminal session governs the GUI session too.
A GUI is a different window onto one crab, not a second installation.

## See also

- README → **ACP Server Mode** for the protocol surface (session load, model
  persistence, mode setting, compaction).
- `opencrabs acp --help` for the current flag list — it is the source of truth.
