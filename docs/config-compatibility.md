# Adding a config key that older builds will read

What an older build does with a config key, action or value it does not know, and what that means
when one config is shared by installs on different releases.

**An unknown TOP-LEVEL key in `config.kdl` is ignored, not an error.** Verified by running a config
carrying three new keys through the previous release's binary: `setup --check` still reported
`CONFIG FILE: Well defined`.

**A block that parses its own children used to reject an unknown one, failing the WHOLE config,
not just the block.** `session_service` was the known example:

```
× Failed to parse Zellij configuration
╰── Unknown session_service entry: "manage_my_session" (expected systemd, launchd, pin_exe, managed_session or restart_via_launchd)
```

(`managed_session` taught that lesson in 2026-08; `launchd { env }` paid for it again.) From
nkmk.18 on, all three blocks — `session_service`, `pane_privacy` and `resurrect_command_hints` —
warn and ignore an unknown name at every depth; see FORK.md's "An unknown key in
`session_service` warns instead of failing the config" and "`pane_privacy` and
`resurrect_command_hints` warn on a key they do not know". But the old rule is not dead: a
build at or before nkmk.17 still rejects an unknown nested key in any of them.

**From the release that carries FORK.md's "A setting this build does not know warns and is
skipped, on every config surface", the whole config is tolerant.** An unknown name or enum value
anywhere — a bind's action, a key spelling, a mode, an option value, a child of `ui`, `web_client`
or a theme, a permission — warns with its file and line and is skipped; `setup --check` still
reports the config as well defined. A malformed KNOWN thing (wrong type, bad syntax, a known action
with bad arguments) still fails. An unknown `pane_privacy` value fails closed for that one rule: it
takes the strictest known value, not the default. That entry lists the exceptions.

Tolerance only helps an install that runs it. **An install still on an older build keeps that
build's strict behaviour**: it rejects a new action, mode or option value exactly as before. So the
oldest build that reads a shared config still gates a change to it.

So before seeding a new key, action or value ahead of the binary, check both sides: the surface
must be a warn-and-ignore one in the build that ships it, and the OLDEST binary that reads the
config must tolerate it too. Do not generalise a top-level probe to a nested key: test the key you actually
intend to add, in the position you intend to add it, against the oldest release still running,
and re-run `zellij setup --check` **before committing**. Getting this wrong breaks every install
that pulls the config, not just the one that is behind.

That decides the rollout order for a config-only feature. The key can be added to a shared config
first and the binaries upgraded afterwards, in any order — installs still on the old build ignore it
until they catch up. There is no need to hold the config back, and no need to gate it per install.

The reverse is not true and is the trap: a shell alias, a service unit, or a script that calls a
**new subcommand** breaks immediately on any install that has not upgraded. Config is forgiving;
the command surface is not. When a change spans both, land the config early and the callers last.

Worth re-verifying rather than assuming if a future release changes config parsing — the check is
two minutes: fetch the previous release's binary and run `setup --check` against the new config.

<!-- Optional local rules, untracked. The import is skipped when the file is absent. -->
@AGENTS.local.md
