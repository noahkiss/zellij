//! fork addition: an agent message for an OpenCode agent goes in through OpenCode's own server,
//! not typed into its prompt.
//!
//! A plain `opencode` TUI starts a private server child, `opencode serve --stdio --port 0`. The
//! TUI talks to it over pipes, but the child also listens on a random loopback TCP port, and its
//! environment holds the password, `OPENCODE_PASSWORD` (basic auth, user `opencode`). Nothing on
//! disk names either. So the session finds them from the pane:
//!
//! 1. the pane's process tree, for the descendant whose argv is `opencode serve --stdio ...`;
//! 2. that process's listening TCP port: on Linux its `/proc/<pid>/fd` socket inodes matched
//!    against the LISTEN rows of `/proc/<pid>/net/tcp` and `tcp6`, in-process; on macOS
//!    `/usr/sbin/lsof`, spawned by absolute path;
//! 3. its `OPENCODE_PASSWORD`, read as `resurrect_hints` reads any variable (`/proc/<pid>/environ`,
//!    or `KERN_PROCARGS2` on macOS).
//!
//! It then posts `POST /api/session/<id>/synthetic` with the body
//! `zellij_utils::agent_mail::opencode_synthetic` builds. The session id is the one the pane's
//! agent reported over `OSC 1337` (the agent-session plugin); stock OpenCode reports none.
//!
//! Everything here runs off the screen thread. Every failure is an `Err` with the reason, and
//! the caller types the message in instead.

use std::time::Duration;

use crate::resurrect_hints::ProcessTree;

/// The whole post, connection included.
pub const POST_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// How long `lsof` may take on macOS before the attempt gives up.
#[cfg(target_os = "macos")]
const LSOF_TIMEOUT: Duration = Duration::from_secs(2);

/// What the post needs: where the agent's server listens, and its password.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The `opencode serve` process.
    pub pid: u32,
    /// `127.0.0.1` or `[::1]`, ready for a URL.
    pub host: String,
    pub port: u16,
    pub password: String,
}

impl std::fmt::Debug for Endpoint {
    // the password never reaches a log
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("pid", &self.pid)
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// Finds the pane's OpenCode server, then posts `body` to session `session_id` on it. `Ok` with
/// the port only on HTTP 200.
pub fn deliver(pane_pid: u32, session_id: &str, body: &serde_json::Value) -> Result<u16, String> {
    if !valid_session_id(session_id) {
        return Err(format!(
            "session id {:?} is not one OpenCode gives",
            session_id
        ));
    }
    let endpoint = discover(pane_pid)?;
    post(&endpoint, session_id, body)?;
    Ok(endpoint.port)
}

/// The OpenCode server under the pane whose shell is `pane_pid`.
pub fn discover(pane_pid: u32) -> Result<Endpoint, String> {
    let tree = ProcessTree::read();
    let pids = tree.descendants(pane_pid);
    let serve = argv_of(&pids)
        .into_iter()
        .find(|(_, argv)| is_serve_stdio(argv))
        .map(|(pid, _)| pid)
        .ok_or_else(|| {
            format!(
                "no `opencode serve --stdio` under pane pid {} (a TUI attached with --server, or \
                 no TUI)",
                pane_pid
            )
        })?;
    let (host, port) = listening_port(serve).ok_or_else(|| {
        format!(
            "opencode serve (pid {}) has no loopback TCP listener",
            serve
        )
    })?;
    let password = crate::resurrect_hints::process_env(serve, "OPENCODE_PASSWORD")
        .filter(|password| !password.is_empty())
        .ok_or_else(|| {
            format!(
                "opencode serve (pid {}) carries no OPENCODE_PASSWORD",
                serve
            )
        })?;
    Ok(Endpoint {
        pid: serve,
        host,
        port,
        password,
    })
}

/// Whether `argv` is OpenCode's private server: `opencode serve --stdio ...`. The program may be
/// named by path (`/opt/homebrew/Cellar/opencode/2.3.2/bin/opencode`), and a script runtime may
/// stand before it (`bun .../opencode serve --stdio`).
pub fn is_serve_stdio(argv: &[String]) -> bool {
    let Some(at) = argv.iter().position(|arg| is_opencode_program(arg)) else {
        return false;
    };
    let rest = &argv[at + 1..];
    rest.first().map(String::as_str) == Some("serve") && rest.iter().any(|arg| arg == "--stdio")
}

fn is_opencode_program(arg: &str) -> bool {
    let name = arg.rsplit('/').next().unwrap_or(arg);
    name == "opencode" || name == "opencode.js" || name == "opencode.exe"
}

/// OpenCode session ids are `ses_` and letters and digits; anything else would be a path
/// segment it never gave.
fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The argv of each of `pids` that can be read, in the order given.
fn argv_of(pids: &[u32]) -> Vec<(u32, Vec<String>)> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let wanted: Vec<Pid> = pids.iter().map(|pid| Pid::from_u32(*pid)).collect();
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&wanted),
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    pids.iter()
        .filter_map(|pid| {
            let process = system.process(Pid::from_u32(*pid))?;
            let argv: Vec<String> = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            (!argv.is_empty()).then_some((*pid, argv))
        })
        .collect()
}

/// Where `pid` listens on TCP, as a host for a URL and a port.
#[cfg(target_os = "linux")]
fn listening_port(pid: u32) -> Option<(String, u16)> {
    let inodes = socket_inodes(pid);
    if inodes.is_empty() {
        return None;
    }
    ["tcp", "tcp6"].iter().find_map(|table| {
        let rows = std::fs::read_to_string(format!("/proc/{}/net/{}", pid, table)).ok()?;
        listen_row(&rows, &inodes)
    })
}

#[cfg(target_os = "macos")]
fn listening_port(pid: u32) -> Option<(String, u16)> {
    let pid_arg = pid.to_string();
    let output = run_with_timeout(
        "/usr/sbin/lsof",
        &["-nP", "-a", "-p", &pid_arg, "-iTCP", "-sTCP:LISTEN", "-Fn"],
        LSOF_TIMEOUT,
    )?;
    lsof_listen(&output)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn listening_port(_pid: u32) -> Option<(String, u16)> {
    None
}

/// The socket inodes `pid` holds open, from its `/proc/<pid>/fd` links (`socket:[12345]`).
#[cfg(target_os = "linux")]
fn socket_inodes(pid: u32) -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{}/fd", pid)) else {
        return vec![];
    };
    entries
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .filter_map(|link| socket_inode(&link.to_string_lossy()))
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// The first LISTEN row of a `/proc/net/tcp` or `tcp6` table whose inode is one of `inodes` and
/// whose address can be reached over loopback.
///
/// A row reads `sl local_address rem_address st tx:rx tr:when retrnsmt uid timeout inode ...`;
/// `local_address` is the address in hex as the kernel stores it, a colon, and the port in hex.
/// `st` `0A` is LISTEN.
#[cfg(any(target_os = "linux", test))]
fn listen_row(rows: &str, inodes: &[u64]) -> Option<(String, u16)> {
    rows.lines().skip(1).find_map(|row| {
        let fields: Vec<&str> = row.split_whitespace().collect();
        if fields.len() < 10 || fields[3] != "0A" {
            return None;
        }
        let inode: u64 = fields[9].parse().ok()?;
        if !inodes.contains(&inode) {
            return None;
        }
        let (address, port) = fields[1].split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        Some((loopback_host_from_hex(address)?, port))
    })
}

/// A `/proc/net/tcp*` address as a loopback host for a URL: a loopback or wildcard IPv4 address
/// is `127.0.0.1`, IPv6 `::1` or `::` is `[::1]`, and a v4-mapped one is its IPv4 answer. The
/// kernel prints each 32-bit word in host byte order.
#[cfg(any(target_os = "linux", test))]
fn loopback_host_from_hex(hex: &str) -> Option<String> {
    let word = |at: usize| -> Option<[u8; 4]> {
        let value = u32::from_str_radix(hex.get(at..at + 8)?, 16).ok()?;
        Some(if cfg!(target_endian = "little") {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        })
    };
    match hex.len() {
        8 => {
            let ip = std::net::Ipv4Addr::from(word(0)?);
            loopback_v4(ip)
        },
        32 => {
            let mut bytes = [0u8; 16];
            for at in 0..4 {
                bytes[at * 4..at * 4 + 4].copy_from_slice(&word(at * 8)?);
            }
            let ip = std::net::Ipv6Addr::from(bytes);
            if let Some(v4) = ip.to_ipv4_mapped() {
                return loopback_v4(v4);
            }
            (ip.is_loopback() || ip.is_unspecified()).then(|| "[::1]".to_owned())
        },
        _ => None,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn loopback_v4(ip: std::net::Ipv4Addr) -> Option<String> {
    (ip.is_loopback() || ip.is_unspecified()).then(|| "127.0.0.1".to_owned())
}

/// The first loopback listener in `lsof -Fn` output, whose name lines read `n127.0.0.1:55121`,
/// `n[::1]:55121` or `n*:55121`.
#[cfg(any(target_os = "macos", test))]
fn lsof_listen(output: &str) -> Option<(String, u16)> {
    output.lines().find_map(|line| {
        let name = line.strip_prefix('n')?;
        let (address, port) = name.rsplit_once(':')?;
        let port: u16 = port.parse().ok()?;
        let host = match address {
            "*" => "127.0.0.1".to_owned(),
            "[::1]" | "[::]" => "[::1]".to_owned(),
            v4 => loopback_v4(v4.parse().ok()?)?,
        };
        Some((host, port))
    })
}

/// Runs `program` and returns its stdout, or `None` when it fails or outlives `timeout`, in
/// which case it is killed.
#[cfg(target_os = "macos")]
fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| log::debug!("opencode delivery: cannot run {}: {}", program, e))
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            },
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                log::debug!("opencode delivery: {} took too long", program);
                return None;
            },
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

/// The URL the post goes to.
fn synthetic_url(endpoint: &Endpoint, session_id: &str) -> String {
    format!(
        "http://{}:{}/api/session/{}/synthetic",
        endpoint.host, endpoint.port, session_id
    )
}

/// `Authorization: Basic` for user `opencode`.
fn basic_auth(password: &str) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{}", password))
    )
}

/// Posts `body` to the synthetic route. `Ok` only on HTTP 200. Never through a proxy: the
/// request carries the password, and the server is on loopback.
fn post(endpoint: &Endpoint, session_id: &str, body: &serde_json::Value) -> Result<(), String> {
    use isahc::config::Configurable;
    let url = synthetic_url(endpoint, session_id);
    let request = isahc::Request::post(&url)
        .header("content-type", "application/json")
        .header("authorization", basic_auth(&endpoint.password))
        .timeout(POST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .proxy(None)
        .body(body.to_string())
        .map_err(|e| format!("cannot build the request: {}", e))?;
    let response = isahc::send(request).map_err(|e| format!("POST {}: {}", url, e))?;
    match response.status().as_u16() {
        200 => Ok(()),
        status => Err(format!("POST {} answered HTTP {}", url, status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn only_opencode_s_private_server_matches() {
        for line in [
            "opencode serve --stdio --port 0",
            "/opt/homebrew/Cellar/opencode/2.3.2/bin/opencode serve --stdio --port 0",
            "/home/u/.bun/bin/bun /home/u/.opencode/opencode.js serve --port 0 --stdio",
        ] {
            assert!(is_serve_stdio(&argv(line)), "{line}");
        }
        for line in [
            "opencode",
            "opencode serve --hostname 127.0.0.1 --port 4799",
            "opencode --server http://127.0.0.1:4798 -s ses_x",
            "/usr/bin/notopencode serve --stdio",
            "vim serve --stdio",
            "opencode run serve --stdio",
        ] {
            assert!(!is_serve_stdio(&argv(line)), "{line}");
        }
        assert!(!is_serve_stdio(&[]));
    }

    #[test]
    fn a_socket_link_gives_its_inode() {
        assert_eq!(socket_inode("socket:[12345]"), Some(12345));
        assert_eq!(socket_inode("pipe:[12345]"), None);
        assert_eq!(socket_inode("/dev/pts/3"), None);
        assert_eq!(socket_inode("socket:[x]"), None);
    }

    /// A `/proc/net/tcp` address word for `ip`, as this machine's kernel prints it.
    fn hex_v4(ip: [u8; 4]) -> String {
        let value = if cfg!(target_endian = "little") {
            u32::from_le_bytes(ip)
        } else {
            u32::from_be_bytes(ip)
        };
        format!("{:08X}", value)
    }

    #[test]
    fn the_listen_row_with_one_of_the_process_s_inodes_gives_the_port() {
        let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when \
                      retrnsmt   uid  timeout inode";
        let row = |sl: u32, ip: [u8; 4], port: u16, st: &str, inode: u64| {
            format!(
                "  {}: {}:{:04X} 00000000:0000 {} 00000000:00000000 00:00000000 00000000  \
                 1000        0 {} 1 0000000000000000 100 0 0 10 0",
                sl,
                hex_v4(ip),
                port,
                st,
                inode
            )
        };
        let table = [
            header.to_owned(),
            // another process's listener
            row(0, [127, 0, 0, 1], 4000, "0A", 111),
            // the process's own connection, not a listener
            row(1, [127, 0, 0, 1], 5000, "01", 222),
            // the process's listener on a LAN address: not reachable over loopback
            row(2, [203, 0, 113, 5], 6000, "0A", 333),
            row(3, [127, 0, 0, 1], 55121, "0A", 444),
        ]
        .join("\n");
        assert_eq!(
            listen_row(&table, &[222, 333, 444]),
            Some(("127.0.0.1".to_owned(), 55121))
        );
        assert_eq!(listen_row(&table, &[222, 333]), None);
        assert_eq!(listen_row(&table, &[]), None);
        // a wildcard listener is reached over loopback
        let any = [header.to_owned(), row(0, [0, 0, 0, 0], 7000, "0A", 9)].join("\n");
        assert_eq!(listen_row(&any, &[9]), Some(("127.0.0.1".to_owned(), 7000)));
    }

    #[test]
    fn an_ipv6_listener_on_loopback_is_reached_at_its_own_address() {
        let word = |bytes: [u8; 4]| hex_v4(bytes);
        let loopback6 = format!(
            "{}{}{}{}",
            word([0, 0, 0, 0]),
            word([0, 0, 0, 0]),
            word([0, 0, 0, 0]),
            word([0, 0, 0, 1])
        );
        assert_eq!(loopback_host_from_hex(&loopback6).as_deref(), Some("[::1]"));
        let mapped = format!(
            "{}{}{}{}",
            word([0, 0, 0, 0]),
            word([0, 0, 0, 0]),
            word([0, 0, 0xff, 0xff]),
            word([127, 0, 0, 1])
        );
        assert_eq!(
            loopback_host_from_hex(&mapped).as_deref(),
            Some("127.0.0.1")
        );
        let lan6 = format!(
            "{}{}{}{}",
            word([0xfe, 0x80, 0, 0]),
            word([0, 0, 0, 0]),
            word([0, 0, 0, 0]),
            word([0, 0, 0, 1])
        );
        assert_eq!(loopback_host_from_hex(&lan6), None);
        assert_eq!(loopback_host_from_hex("zz"), None);
    }

    #[test]
    fn lsof_s_name_lines_give_the_loopback_port() {
        let output = "p98669\nf23\nn127.0.0.1:55121\n";
        assert_eq!(lsof_listen(output), Some(("127.0.0.1".to_owned(), 55121)));
        assert_eq!(
            lsof_listen("p1\nf3\nn[::1]:4000\n"),
            Some(("[::1]".to_owned(), 4000))
        );
        assert_eq!(
            lsof_listen("p1\nf3\nn*:4001\n"),
            Some(("127.0.0.1".to_owned(), 4001))
        );
        assert_eq!(lsof_listen("p1\nf3\nn203.0.113.5:4002\n"), None);
        assert_eq!(lsof_listen(""), None);
    }

    #[test]
    fn the_request_goes_to_the_session_s_synthetic_route_with_basic_auth() {
        let endpoint = Endpoint {
            pid: 1,
            host: "127.0.0.1".to_owned(),
            port: 55121,
            password: "pw".to_owned(),
        };
        assert_eq!(
            synthetic_url(&endpoint, "ses_e048ac785ffeXSk4IVGCSPqsoF"),
            "http://127.0.0.1:55121/api/session/ses_e048ac785ffeXSk4IVGCSPqsoF/synthetic"
        );
        // base64("opencode:pw")
        assert_eq!(basic_auth("pw"), "Basic b3BlbmNvZGU6cHc=");
        assert!(!format!("{:?}", endpoint).contains("pw"));
    }

    #[test]
    fn a_session_id_must_look_like_one_before_it_becomes_a_path() {
        assert!(valid_session_id("ses_e048ac785ffeXSk4IVGCSPqsoF"));
        for bad in ["", "../x", "ses/x", "ses x", "ses?x"] {
            assert!(!valid_session_id(bad), "{bad}");
            assert!(deliver(1, bad, &serde_json::json!({})).is_err());
        }
    }
}
