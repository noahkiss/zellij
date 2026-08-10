//! Some general utility functions.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::OnceLock;
use std::{iter, str::from_utf8};

use crate::data::{Palette, PaletteColor, PaletteSource, ThemeHue};
use crate::envs::get_session_name;
use crate::errors::prelude::*;
use crate::input::options::Options;
use colorsys::{Ansi256, Rgb};
use strip_ansi_escapes::strip;
use unicode_width::UnicodeWidthStr;

#[cfg(unix)]
pub use unix_only::*;

#[cfg(unix)]
mod unix_only {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::{fs, io};

    pub fn set_permissions(path: &Path, mode: u32) -> io::Result<()> {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions)
    }
}

#[cfg(not(unix))]
pub fn set_permissions(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

pub fn ansi_len(s: &str) -> usize {
    from_utf8(&strip(s)).unwrap().width()
}

pub fn clean_string_from_control_and_linebreak(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            !c.is_control() &&
            *c != '\n' &&      // line feed
            *c != '\r' &&      // carriage return
            *c != '\u{2028}' && // line separator
            *c != '\u{2029}' // paragraph separator
        })
        .collect()
}

pub fn adjust_to_size(s: &str, rows: usize, columns: usize) -> String {
    s.lines()
        .map(|l| {
            let actual_len = ansi_len(l);
            if actual_len > columns {
                let mut line = String::from(l);
                line.truncate(columns);
                line
            } else {
                [l, &str::repeat(" ", columns - ansi_len(l))].concat()
            }
        })
        .chain(iter::repeat(str::repeat(" ", columns)))
        .take(rows)
        .collect::<Vec<_>>()
        .join("\n\r")
}

/// The TERM a session is given when whatever created it had none worth passing on.
///
/// A terminal type every terminal emulator of the last two decades understands, and the one the
/// generated units name - see [`crate::session_service`], which spells it out where a reader of
/// the unit can see it.
///
/// It lives HERE, not beside the term logic in `session_lifecycle`, because `session_service`
/// reads it and is compiled for wasm while `session_lifecycle` is gated out of wasm. With the
/// const on the far side of that gate the whole crate - and so every default plugin - failed to
/// build for wasm, silently, because the plugin .wasm assets are checked in prebuilt and nothing
/// in the ordinary build recompiles them.
pub const DEFAULT_TERM: &str = "xterm-256color";

/// What the terminal title looks like when `terminal_title_template` is not set - the historical
/// `<session> | <pane>` format.
pub const DEFAULT_TERMINAL_TITLE_TEMPLATE: &str = "{session} | {pane}";

/// The terminal title format of the session running in this process.
///
/// Set once the first client has connected and the config file has been read, because that is the
/// earliest point at which it is known. Panes read it while rendering, which is far away from any
/// place that still has the config in hand.
static TERMINAL_TITLE_FORMAT: OnceLock<TerminalTitleFormat> = OnceLock::new();

/// Fix the terminal title format for the rest of this process. Later calls do nothing.
pub fn set_terminal_title_format(terminal_title_format: TerminalTitleFormat) {
    let _ = TERMINAL_TITLE_FORMAT.set(terminal_title_format);
}

fn terminal_title_format() -> &'static TerminalTitleFormat {
    static DEFAULT_TERMINAL_TITLE_FORMAT: OnceLock<TerminalTitleFormat> = OnceLock::new();
    TERMINAL_TITLE_FORMAT
        .get()
        .unwrap_or_else(|| DEFAULT_TERMINAL_TITLE_FORMAT.get_or_init(TerminalTitleFormat::default))
}

/// The placeholders a terminal title template can contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitlePlaceholder {
    Host,
    Session,
    Pane,
}

impl TitlePlaceholder {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "host" => Some(TitlePlaceholder::Host),
            "session" => Some(TitlePlaceholder::Session),
            "pane" => Some(TitlePlaceholder::Pane),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TitleToken {
    Literal(String),
    Placeholder(TitlePlaceholder),
}

/// A parsed `terminal_title_template` plus the session name aliases it renders with.
///
/// Parsing happens once (at session start), rendering happens per title change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTitleFormat {
    tokens: Vec<TitleToken>,
    session_aliases: BTreeMap<String, String>,
}

impl Default for TerminalTitleFormat {
    fn default() -> Self {
        TerminalTitleFormat::new(None, BTreeMap::new())
    }
}

impl TerminalTitleFormat {
    pub fn new(template: Option<&str>, session_aliases: BTreeMap<String, String>) -> Self {
        let template = template.unwrap_or(DEFAULT_TERMINAL_TITLE_TEMPLATE);
        TerminalTitleFormat {
            tokens: Self::parse_template(template),
            session_aliases,
        }
    }
    pub fn from_options(config_options: &Options) -> Self {
        TerminalTitleFormat::new(
            config_options.terminal_title_template.as_deref(),
            config_options.session_aliases.clone().unwrap_or_default(),
        )
    }
    /// Split the template into literals and placeholders. An unknown placeholder (eg. `{nope}`) is
    /// kept as literal text rather than being an error, so that a template can contain braces.
    fn parse_template(template: &str) -> Vec<TitleToken> {
        let mut tokens = vec![];
        let mut literal = String::new();
        let mut rest = template;
        while let Some(opening_brace) = rest.find('{') {
            let (before_brace, from_brace) = rest.split_at(opening_brace);
            literal.push_str(before_brace);
            match from_brace.find('}') {
                Some(closing_brace) => {
                    let name = &from_brace[1..closing_brace];
                    match TitlePlaceholder::from_name(name) {
                        Some(placeholder) => {
                            if !literal.is_empty() {
                                tokens.push(TitleToken::Literal(std::mem::take(&mut literal)));
                            }
                            tokens.push(TitleToken::Placeholder(placeholder));
                        },
                        None => literal.push_str(&from_brace[..=closing_brace]),
                    }
                    rest = &from_brace[closing_brace + 1..];
                },
                None => {
                    literal.push_str(from_brace);
                    rest = "";
                },
            }
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            tokens.push(TitleToken::Literal(literal));
        }
        tokens
    }
    fn session_alias<'a>(&'a self, session_name: &'a str) -> &'a str {
        self.session_aliases
            .get(session_name)
            .map(|alias| alias.as_str())
            .unwrap_or(session_name)
    }
    /// Render the title, dropping the literals around placeholders that came out empty.
    ///
    /// A literal is only kept if every side of it that has placeholders has at least one non-empty
    /// one - otherwise `{session} | {pane}` would leave a dangling " | " behind whenever a pane has
    /// no title. Leading and trailing whitespace is trimmed off the result.
    pub fn render(&self, session_name: Option<&str>, hostname: &str, pane_title: &str) -> String {
        let values: Vec<Option<&str>> = self
            .tokens
            .iter()
            .map(|token| match token {
                TitleToken::Literal(_) => None,
                TitleToken::Placeholder(TitlePlaceholder::Host) => Some(hostname),
                TitleToken::Placeholder(TitlePlaceholder::Session) => {
                    Some(session_name.map(|n| self.session_alias(n)).unwrap_or(""))
                },
                TitleToken::Placeholder(TitlePlaceholder::Pane) => Some(pane_title),
            })
            .collect();
        let has_value = |values: &[Option<&str>]| values.iter().flatten().any(|v| !v.is_empty());
        let has_placeholder = |values: &[Option<&str>]| values.iter().any(|v| v.is_some());

        let mut rendered = String::new();
        for (i, token) in self.tokens.iter().enumerate() {
            match token {
                TitleToken::Placeholder(_) => rendered.push_str(values[i].unwrap_or("")),
                TitleToken::Literal(literal) => {
                    let (before, after) = (&values[..i], &values[i + 1..]);
                    let keep = (!has_placeholder(before) || has_value(before))
                        && (!has_placeholder(after) || has_value(after));
                    if keep {
                        rendered.push_str(literal);
                    }
                },
            }
        }
        rendered.trim().to_string()
    }
}

/// This machine's hostname without any domain suffix, resolved once.
///
/// Empty if it cannot be resolved, which the title rendering treats like any other empty part.
pub fn short_hostname() -> &'static str {
    static SHORT_HOSTNAME: OnceLock<String> = OnceLock::new();
    SHORT_HOSTNAME.get_or_init(|| {
        resolve_hostname()
            .map(|hostname| hostname.split('.').next().unwrap_or("").to_string())
            .unwrap_or_default()
    })
}

// libc rather than nix::unistd::gethostname, because nix is pulled in here without the feature
// flags that would expose it
#[cfg(unix)]
fn resolve_hostname() -> Option<String> {
    let mut buffer = vec![0u8; 256];
    let resolved =
        unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) } == 0;
    if !resolved {
        return None;
    }
    let hostname_length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    buffer.truncate(hostname_length);
    String::from_utf8(buffer)
        .ok()
        .filter(|hostname| !hostname.is_empty())
}

#[cfg(not(unix))]
fn resolve_hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|hostname| !hostname.is_empty())
}

pub fn make_terminal_title(pane_title: &str) -> String {
    format!(
        "\u{1b}]0;{}\u{07}",
        terminal_title_format().render(
            get_session_name().ok().as_deref(),
            short_hostname(),
            pane_title
        )
    )
}

// Colors
pub mod colors {
    pub const WHITE: u8 = 255;
    pub const GREEN: u8 = 154;
    pub const GRAY: u8 = 238;
    pub const BRIGHT_GRAY: u8 = 245;
    pub const RED: u8 = 124;
    pub const ORANGE: u8 = 166;
    pub const BLACK: u8 = 16;
    pub const MAGENTA: u8 = 201;
    pub const CYAN: u8 = 51;
    pub const YELLOW: u8 = 226;
    pub const BLUE: u8 = 45;
    pub const PURPLE: u8 = 99;
    pub const GOLD: u8 = 136;
    pub const SILVER: u8 = 245;
    pub const PINK: u8 = 207;
    pub const BROWN: u8 = 215;
}

pub fn _hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    Rgb::from_hex_str(hex)
        .expect("The passed argument must be a valid hex color")
        .into()
}

pub fn eightbit_to_rgb(c: u8) -> (u8, u8, u8) {
    Ansi256::new(c).as_rgb().into()
}

pub fn default_palette() -> Palette {
    Palette {
        source: PaletteSource::Default,
        theme_hue: ThemeHue::Dark,
        fg: PaletteColor::EightBit(colors::BRIGHT_GRAY),
        bg: PaletteColor::EightBit(colors::GRAY),
        black: PaletteColor::EightBit(colors::BLACK),
        red: PaletteColor::EightBit(colors::RED),
        green: PaletteColor::EightBit(colors::GREEN),
        yellow: PaletteColor::EightBit(colors::YELLOW),
        blue: PaletteColor::EightBit(colors::BLUE),
        magenta: PaletteColor::EightBit(colors::MAGENTA),
        cyan: PaletteColor::EightBit(colors::CYAN),
        white: PaletteColor::EightBit(colors::WHITE),
        orange: PaletteColor::EightBit(colors::ORANGE),
        gray: PaletteColor::EightBit(colors::GRAY),
        purple: PaletteColor::EightBit(colors::PURPLE),
        gold: PaletteColor::EightBit(colors::GOLD),
        silver: PaletteColor::EightBit(colors::SILVER),
        pink: PaletteColor::EightBit(colors::PINK),
        brown: PaletteColor::EightBit(colors::BROWN),
    }
}

// Dark magic
pub fn detect_theme_hue(bg: PaletteColor) -> ThemeHue {
    match bg {
        PaletteColor::Rgb((r, g, b)) => {
            // HSP, P stands for perceived brightness
            let hsp: f64 = (0.299 * (r as f64 * r as f64)
                + 0.587 * (g as f64 * g as f64)
                + 0.114 * (b as f64 * b as f64))
                .sqrt();
            match hsp > 127.5 {
                true => ThemeHue::Light,
                false => ThemeHue::Dark,
            }
        },
        _ => ThemeHue::Dark,
    }
}

// (this was shamelessly copied from alacritty)
//
// This returns the current terminal version as a unique number based on the
// semver version. The different versions are padded to ensure that a higher semver version will
// always report a higher version number.
pub fn version_number(mut version: &str) -> usize {
    if let Some(separator) = version.rfind('-') {
        version = &version[..separator];
    }

    let mut version_number = 0;

    let semver_versions = version.split('.');
    for (i, semver_version) in semver_versions.rev().enumerate() {
        let semver_number = semver_version.parse::<usize>().unwrap_or(0);
        version_number += usize::pow(100, i as u32) * semver_number;
    }

    version_number
}

pub fn web_server_base_url(
    web_server_ip: IpAddr,
    web_server_port: u16,
    has_certificate: bool,
    enforce_https_for_localhost: bool,
) -> String {
    let is_loopback = match web_server_ip {
        IpAddr::V4(ipv4) => ipv4.is_loopback(),
        IpAddr::V6(ipv6) => ipv6.is_loopback(),
    };

    let url_prefix = if is_loopback && !enforce_https_for_localhost && !has_certificate {
        "http"
    } else {
        "https"
    };
    format!("{}://{}:{}", url_prefix, web_server_ip, web_server_port)
}

pub fn web_server_base_url_from_config(config_options: Options) -> String {
    let web_server_ip = config_options
        .web_server_ip
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
    let web_server_port = config_options.web_server_port.unwrap_or_else(|| 8082);
    let has_certificate =
        config_options.web_server_cert.is_some() && config_options.web_server_key.is_some();
    let enforce_https_for_localhost = config_options.enforce_https_for_localhost.unwrap_or(false);
    web_server_base_url(
        web_server_ip,
        web_server_port,
        has_certificate,
        enforce_https_for_localhost,
    )
}

pub struct ServerAddress {
    pub ip: String,
    pub port: u16,
}

pub fn parse_base_url(url: &str) -> Result<ServerAddress> {
    let url = url::Url::parse(url)?;
    let ip = url
        .host_str()
        .ok_or_else(|| anyhow!("No host in URL"))?
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("No port in URL"))?;

    Ok(ServerAddress { ip, port })
}

#[cfg(test)]
mod terminal_title_tests {
    use super::*;

    /// The formatting `make_terminal_title` did before it was templated - the yardstick for the
    /// default template.
    fn legacy_title(session_name: Option<&str>, pane_title: &str) -> String {
        format!(
            "{}{}",
            session_name
                .map(|n| if pane_title.is_empty() {
                    format!("{}", n)
                } else {
                    format!("{} | ", n)
                })
                .unwrap_or_default(),
            pane_title
        )
    }

    fn aliases(aliases: &[(&str, &str)]) -> BTreeMap<String, String> {
        aliases
            .iter()
            .map(|(session_name, alias)| (session_name.to_string(), alias.to_string()))
            .collect()
    }

    #[test]
    fn default_template_renders_like_the_untemplated_title() {
        let format = TerminalTitleFormat::default();
        for session_name in [Some("my-session"), None] {
            for pane_title in ["", "vim", "some pane"] {
                assert_eq!(
                    format.render(session_name, "example-host", pane_title),
                    legacy_title(session_name, pane_title),
                    "session: {:?}, pane: {:?}",
                    session_name,
                    pane_title
                );
            }
        }
    }

    #[test]
    fn template_renders_all_placeholders() {
        let format = TerminalTitleFormat::new(
            Some("{host} · {session} | {pane}"),
            aliases(&[("my-session", "MS")]),
        );
        assert_eq!(
            format.render(Some("my-session"), "example-host", "vim"),
            "example-host · MS | vim"
        );
    }

    #[test]
    fn session_without_an_alias_renders_its_own_name() {
        let format = TerminalTitleFormat::new(
            Some("{host} · {session} | {pane}"),
            aliases(&[("my-session", "MS")]),
        );
        assert_eq!(
            format.render(Some("other-session"), "example-host", "vim"),
            "example-host · other-session | vim"
        );
    }

    #[test]
    fn an_empty_pane_title_drops_its_separator() {
        let format = TerminalTitleFormat::new(
            Some("{host} · {session} | {pane}"),
            aliases(&[("my-session", "MS")]),
        );
        assert_eq!(
            format.render(Some("my-session"), "example-host", ""),
            "example-host · MS"
        );
    }

    #[test]
    fn an_unresolved_hostname_drops_its_separator() {
        let format = TerminalTitleFormat::new(Some("{host} · {session} | {pane}"), BTreeMap::new());
        assert_eq!(
            format.render(Some("my-session"), "", "vim"),
            "my-session | vim"
        );
        assert_eq!(format.render(None, "", ""), "");
    }

    #[test]
    fn placeholders_render_in_any_order() {
        let format = TerminalTitleFormat::new(
            Some("[{pane}] {session}@{host}"),
            aliases(&[("my-session", "MS")]),
        );
        assert_eq!(
            format.render(Some("my-session"), "example-host", "vim"),
            "[vim] MS@example-host"
        );
    }

    #[test]
    fn unknown_placeholders_are_literal_text() {
        let format = TerminalTitleFormat::new(Some("{nope} {session"), BTreeMap::new());
        assert_eq!(
            format.render(Some("my-session"), "example-host", "vim"),
            "{nope} {session"
        );
    }

    #[test]
    fn a_template_without_placeholders_is_rendered_as_is() {
        let format = TerminalTitleFormat::new(Some("zellij"), BTreeMap::new());
        assert_eq!(format.render(None, "", ""), "zellij");
    }
}
