//! Subcommands for building.
//!
//! Currently has the following functions:
//!
//! - [`build`]: Builds general cargo projects (i.e. zellij components) with `cargo build`
use crate::{flags, metadata, WorkspaceMember};
use anyhow::Context;
use std::path::{Path, PathBuf};
use xshell::{cmd, Shell};

/// Build members of the zellij workspace.
///
/// Build behavior is controlled by the [`flags`](flags::Build). Calls some variation of `cargo
/// build` under the hood.
pub fn build(sh: &Shell, flags: flags::Build) -> anyhow::Result<()> {
    let _pd = sh.push_dir(crate::project_root());

    let cargo = crate::cargo()?;
    if flags.no_plugins && flags.plugins_only {
        eprintln!("Cannot use both '--no-plugins' and '--plugins-only'");
        std::process::exit(1);
    }

    // zellij-utils requires protobuf definition files to be present. Usually these are
    // auto-generated with `build.rs`-files, but this is currently broken for us.
    // See [this PR][1] for details.
    //
    // [1]: https://github.com/zellij-org/zellij/pull/2711#issuecomment-1695015818
    run_proto_codegen(sh, false);

    // Build all plugins in a single invocation so Cargo can unify transitive dependency
    // features across all of them and compile shared crates (e.g. zellij-utils) only once.
    let build_plugins =
        !flags.no_plugins && (flags.release || plugins_force() || plugin_sources_changed());
    if build_plugins {
        let plugin_members: Vec<&WorkspaceMember> = crate::workspace_members()
            .iter()
            .filter(|m| m.build && m.crate_name.contains("plugins"))
            .collect();

        if !plugin_members.is_empty() {
            eprintln!();
            let msg = ">> Building plugins";
            crate::status(msg);
            eprintln!("{}", msg);

            if flags.release {
                build_plugins_release_into_assets(sh, &plugin_members)?;
            } else {
                let remap = wasm_path_remap_config();
                let mut base_cmd =
                    cmd!(sh, "{cargo} build --target wasm32-wasip1 --config {remap}");
                for member in &plugin_members {
                    base_cmd = base_cmd.args(["-p", plugin_name_of(member)?]);
                }
                base_cmd.run().context("failed to build plugins")?;
                write_plugin_stamp(sh);
            }
        }
    } else if !flags.no_plugins {
        let msg = ">> Plugins unchanged since last build, skipping (set ZELLIJ_FORCE_PLUGINS=1 to rebuild)";
        crate::status(msg);
        eprintln!("{}", msg);
    }

    // Build non-plugin crates (native target).
    if !flags.plugins_only {
        for WorkspaceMember { crate_name, .. } in crate::workspace_members()
            .iter()
            .filter(|member| member.build && !member.crate_name.contains("plugins"))
        {
            let err_context = || format!("failed to build '{crate_name}'");

            let _pd = sh.push_dir(Path::new(crate_name));
            println!();
            let msg = format!(">> Building '{crate_name}'");
            crate::status(&msg);
            println!("{}", msg);

            let mut base_cmd = cmd!(sh, "{cargo} build");
            if flags.release {
                base_cmd = base_cmd.arg("--release");
            } else {
                base_cmd = base_cmd.args(["--profile", "dev-opt"]);
            }
            if flags.no_web {
                // Check if this crate has web features that need modification
                match metadata::get_no_web_features(sh, crate_name)
                    .context("Failed to check web features")?
                {
                    Some(features) => {
                        base_cmd = base_cmd.arg("--no-default-features");
                        if !features.is_empty() {
                            base_cmd = base_cmd.arg("--features");
                            base_cmd = base_cmd.arg(features);
                        }
                    },
                    None => {
                        // Crate doesn't have web features, build normally
                    },
                }
            }
            base_cmd = base_cmd.args(&flags.args);
            base_cmd.run().with_context(err_context)?;
        }
    }

    Ok(())
}

fn plugins_force() -> bool {
    std::env::var_os("ZELLIJ_FORCE_PLUGINS").is_some()
}

fn plugin_asset_path(plugin_name: &str) -> PathBuf {
    crate::asset_dir()
        .join("plugins")
        .join(plugin_name)
        .with_extension("wasm")
}

fn newest_plugin_source_time() -> Option<std::time::SystemTime> {
    let root = crate::project_root();
    ["default-plugins", "zellij-tile", "zellij-tile-utils"]
        .iter()
        .filter_map(|dir| newest_file_time(&root.join(dir)))
        .max()
}

fn newest_file_time(dir: &Path) -> Option<std::time::SystemTime> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut newest: Option<std::time::SystemTime> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            if path
                .file_name()
                .map(|name| name == "target")
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(child) = newest_file_time(&path) {
                newest = Some(newest.map_or(child, |current| current.max(child)));
            }
        } else if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            newest = Some(newest.map_or(modified, |current| current.max(modified)));
        }
    }
    newest
}

pub fn ensure_plugin_assets(sh: &Shell) -> anyhow::Result<()> {
    let plugin_members: Vec<&WorkspaceMember> = crate::workspace_members()
        .iter()
        .filter(|m| m.build && m.crate_name.contains("plugins"))
        .collect();
    if plugin_members.is_empty() {
        return Ok(());
    }

    let newest_source = newest_plugin_source_time();
    let stale = plugins_force()
        || plugin_members.iter().any(|member| {
            let plugin_name = match member.crate_name.rsplit_once('/') {
                Some((_, name)) => name,
                None => return true,
            };
            let asset_time = std::fs::metadata(plugin_asset_path(plugin_name))
                .and_then(|m| m.modified())
                .ok();
            match (asset_time, newest_source) {
                (Some(asset_time), Some(newest_source)) => asset_time < newest_source,
                _ => true,
            }
        });

    if !stale {
        let msg = ">> Plugin assets up to date, skipping plugin build";
        crate::status(msg);
        eprintln!("{}", msg);
        return Ok(());
    }

    let msg = ">> Building plugin assets (release)";
    crate::status(msg);
    eprintln!("{}", msg);

    build_plugins_release_into_assets(sh, &plugin_members)
}

fn build_plugins_release_into_assets(
    sh: &Shell,
    plugin_members: &[&WorkspaceMember],
) -> anyhow::Result<()> {
    let cargo = crate::cargo()?;
    let remap = wasm_path_remap_config();
    let mut base_cmd = cmd!(
        sh,
        "{cargo} build --target wasm32-wasip1 --release --config {remap}"
    );
    for member in plugin_members {
        let plugin_name = plugin_name_of(member)?;
        base_cmd = base_cmd.args(["-p", plugin_name]);
    }
    base_cmd.run().context("failed to build plugin assets")?;

    for member in plugin_members {
        move_plugin_to_assets(sh, plugin_name_of(member)?)?;
    }
    Ok(())
}

/// A cargo `--config` argument that strips machine-specific absolute paths from the plugins.
///
/// The built `.wasm` files are checked in under `zellij-utils/assets/plugins/`, and rustc bakes an
/// absolute path into them for every `panic!` location and every `include!`d file. Without a remap
/// those paths publish whoever built the assets: their home directory, their cargo registry, and
/// where they keep the checkout.
///
/// Scoped to the wasm target so it never disturbs the host flags — a linker or a `-C` setting a
/// machine configures for its native triple still applies. Cargo appends this to any
/// `target.wasm32-wasip1.rustflags` a config file already sets rather than replacing them.
fn wasm_path_remap_config() -> String {
    // Generic first, specific last: rustc applies the last prefix that matches.
    let mut prefixes: Vec<(PathBuf, &str)> = Vec::new();
    if let Some(home) = home_dir() {
        prefixes.push((home, "~"));
    }
    if let Some(cargo_home) = cargo_home() {
        prefixes.push((cargo_home, "/cargo"));
    }
    prefixes.push((crate::project_root(), "/zellij"));

    let flags = prefixes
        .into_iter()
        .map(|(from, to)| toml_string(&format!("--remap-path-prefix={}={}", from.display(), to)))
        .collect::<Vec<_>>()
        .join(",");
    format!("target.wasm32-wasip1.rustflags=[{flags}]")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

fn cargo_home() -> Option<PathBuf> {
    match std::env::var_os("CARGO_HOME") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => home_dir().map(|home| home.join(".cargo")),
    }
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn plugin_name_of(member: &WorkspaceMember) -> anyhow::Result<&'static str> {
    Ok(member
        .crate_name
        .rsplit_once('/')
        .context("Cannot determine plugin name from crate path")?
        .1)
}

fn plugin_stamp_path() -> PathBuf {
    crate::target_dir().join(".xtask-plugins-stamp")
}

fn write_plugin_stamp(sh: &Shell) {
    let _ = sh.write_file(plugin_stamp_path(), b"");
}

fn plugin_sources_changed() -> bool {
    let stamp_time = match std::fs::metadata(plugin_stamp_path()).and_then(|m| m.modified()) {
        Ok(stamp_time) => stamp_time,
        Err(_) => return true,
    };
    match newest_plugin_source_time() {
        Some(newest_source) => newest_source > stamp_time,
        None => true,
    }
}

pub fn proto(sh: &Shell) -> anyhow::Result<()> {
    let msg = ">> Generating protobuffer code";
    crate::status(msg);
    println!("{}", msg);

    run_proto_codegen(sh, true);
    Ok(())
}

fn run_proto_codegen(sh: &Shell, force: bool) {
    let zellij_utils_basedir = crate::project_root().join("zellij-utils");
    let _pd = sh.push_dir(&zellij_utils_basedir);

    let specs: &[(&str, &str, &str)] = &[
        ("assets/prost", "src/plugin_api", "generated_plugin_api.rs"),
        (
            "assets/prost_ipc",
            "src/client_server_contract",
            "generated_client_server_api.rs",
        ),
        (
            "assets/prost_web_server",
            "src/web_server_contract",
            "generated_web_server_api.rs",
        ),
        (
            "assets/prost_nested_session",
            "src/nested_session_contract",
            "generated_nested_session_api.rs",
        ),
    ];

    for (out_subdir, src_subdir, include_file) in specs {
        let out_dir = sh.current_dir().join(out_subdir);
        let src_dir = sh.current_dir().join(src_subdir);
        std::fs::create_dir_all(&out_dir).unwrap();

        let last_generated = out_dir
            .join(include_file)
            .metadata()
            .and_then(|m| m.modified());
        let mut proto_files = vec![];
        let mut needs_regeneration = force;

        for entry in std::fs::read_dir(&src_dir).unwrap() {
            let entry_path = entry.unwrap().path();
            if entry_path.is_file()
                && entry_path
                    .extension()
                    .map(|e| e == "proto")
                    .unwrap_or(false)
            {
                let modified = entry_path.metadata().and_then(|m| m.modified());
                needs_regeneration |= match (&last_generated, modified) {
                    (Ok(last_generated), Ok(modified)) => modified > *last_generated,
                    // Couldn't read some metadata, assume needs update
                    _ => true,
                };
                proto_files.push(entry_path.display().to_string());
            }
        }
        proto_files.sort();

        if needs_regeneration {
            let mut prost = prost_build::Config::new();
            prost.out_dir(&out_dir);
            prost.include_file(include_file);
            prost.compile_protos(&proto_files, &[src_dir]).unwrap();
        }
    }
}

fn move_plugin_to_assets(sh: &Shell, plugin_name: &str) -> anyhow::Result<()> {
    let err_context = || format!("failed to move plugin '{plugin_name}' to assets folder");

    // Get asset path
    let asset_name = crate::asset_dir()
        .join("plugins")
        .join(plugin_name)
        .with_extension("wasm");

    // Get plugin path
    let plugin = crate::target_dir()
        .join("wasm32-wasip1")
        .join("release")
        .join(plugin_name)
        .with_extension("wasm");

    if !plugin.is_file() {
        return Err(anyhow::anyhow!("No plugin found at '{}'", plugin.display()))
            .with_context(err_context);
    }

    // This is a plugin we want to move
    let from = plugin.as_path();
    let to = asset_name.as_path();
    sh.copy_file(from, to).with_context(err_context)
}
