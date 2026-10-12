//! The viewer's terminal: raw mode on the pty it was given, the alternate screen, a read loop for
//! keys and resizes, and putting all of it back on the way out.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use zellij_utils::cli::ImageMode;

use super::decode;
use super::render::{self, CellSize, Key, Pager, Renderer, Rgba};

/// How long the viewer waits for the terminal to answer the kitty probe. A terminal answers the
/// device-attributes half at once, so this bounds only one that answers nothing at all.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// The window, in cells and in pixels.
#[derive(Debug, Clone, Copy)]
struct Window {
    cols: u16,
    rows: u16,
    cell: CellSize,
}

/// Raw mode, held for as long as this lives.
struct RawMode {
    fd: i32,
    saved: libc::termios,
}

impl RawMode {
    fn enter(fd: i32) -> Option<RawMode> {
        // SAFETY: `termios` is plain data, and tcgetattr fills it before it is read
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return None;
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return None;
        }
        Some(RawMode { fd, saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

fn window(fd: i32) -> Window {
    // SAFETY: `winsize` is plain data, and TIOCGWINSZ fills it
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } == 0;
    if !ok || size.ws_col == 0 || size.ws_row == 0 {
        return Window {
            cols: 80,
            rows: 24,
            cell: render::ASSUMED_CELL,
        };
    }
    Window {
        cols: size.ws_col,
        rows: size.ws_row,
        cell: CellSize::from_window(size.ws_col, size.ws_row, size.ws_xpixel, size.ws_ypixel),
    }
}

/// Whether stdin has something to read within `timeout`.
fn readable(fd: i32, timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    unsafe { libc::poll(&mut poll, 1, millis) > 0 && (poll.revents & libc::POLLIN) != 0 }
}

/// An image id that is this viewer's own. Inside zellij ids are per pane anyway; outside it, the
/// process id keeps two viewers in one terminal from replacing each other's image.
fn image_id() -> u32 {
    0x5a00_0000 | (std::process::id() & 0x00ff_ffff)
}

/// Ask the terminal whether it draws kitty graphics. `None` when it never said.
fn probe(stdin: &mut std::io::Stdin, stdout: &mut std::io::Stdout, fd: i32) -> Option<bool> {
    let _ = stdout.write_all(&render::kitty_probe(image_id()));
    let _ = stdout.flush();
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut replies = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !readable(fd, left) {
            return None;
        }
        match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => replies.extend_from_slice(&buf[..n]),
        }
        if let Some(answer) = render::probe_answer(&replies) {
            return Some(answer);
        }
    }
}

/// One file, read and decoded, or why it could not be.
fn load(path: &PathBuf) -> Result<Rgba, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    decode::decode(&bytes)
}

/// The whole screen for the image showing.
fn frame(
    renderer: Renderer,
    window: Window,
    pager: &Pager,
    path: &PathBuf,
    image: &Result<Rgba, String>,
) -> Vec<u8> {
    let cols = window.cols as usize;
    let title = render::title(pager, path);
    let mut out = Vec::new();
    if renderer == Renderer::Kitty {
        out.extend_from_slice(&render::kitty_delete(image_id()));
    }
    // the title on the pane's frame as well as in the top row
    out.extend_from_slice(format!("\x1b]2;{}\x07", title).as_bytes());
    out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    out.extend_from_slice(
        format!(
            "\x1b[1;1H\x1b[1m{}\x1b[0m",
            render::clip(&title, cols.saturating_sub(1))
        )
        .as_bytes(),
    );
    if window.rows > 2 {
        out.extend_from_slice(
            format!(
                "\x1b[{};1H\x1b[2m{}\x1b[0m",
                window.rows,
                render::clip(render::footer(pager.len), cols.saturating_sub(1))
            )
            .as_bytes(),
        );
    }
    // the image area: every row between the title and the footer
    let top = 2u16;
    let area_rows = window.rows.saturating_sub(2);
    match image {
        Err(message) => {
            let message = render::clip(message, cols.saturating_sub(2));
            let row = top + area_rows / 2;
            let col = (cols.saturating_sub(message.chars().count()) / 2 + 1) as u16;
            out.extend_from_slice(format!("\x1b[{};{}H{}", row, col, message).as_bytes());
        },
        Ok(image) => match renderer {
            Renderer::Kitty => {
                if let Some(fit) = render::fit_kitty(
                    image.width,
                    image.height,
                    window.cols,
                    area_rows,
                    window.cell,
                ) {
                    let scaled = render::resample(image, fit.px_width, fit.px_height);
                    out.extend_from_slice(
                        format!("\x1b[{};{}H", top + fit.row, fit.col + 1).as_bytes(),
                    );
                    out.extend_from_slice(&render::kitty_transmit(
                        image_id(),
                        &scaled,
                        fit.cols,
                        fit.rows,
                    ));
                }
            },
            Renderer::Blocks => {
                if let Some(fit) = render::fit_blocks(
                    image.width,
                    image.height,
                    window.cols,
                    area_rows,
                    window.cell,
                ) {
                    let scaled = render::resample(image, fit.px_width, fit.px_height);
                    out.extend_from_slice(
                        render::blocks(&scaled, top + fit.row, fit.col + 1).as_bytes(),
                    );
                }
            },
        },
    }
    out
}

pub fn run(files: Vec<PathBuf>, mode: ImageMode) -> i32 {
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let in_fd = stdin.as_raw_fd();
    let out_fd = stdout.as_raw_fd();
    if unsafe { libc::isatty(in_fd) } != 1 || unsafe { libc::isatty(out_fd) } != 1 {
        eprintln!("view-images draws in a terminal, and stdin or stdout is not one.");
        return 1;
    }
    if files.is_empty() {
        eprintln!("view-images needs at least one file.");
        return 1;
    }
    let Some(raw) = RawMode::enter(in_fd) else {
        eprintln!("view-images could not put the terminal in raw mode.");
        return 1;
    };

    let resized = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(libc::SIGWINCH, resized.clone());
    for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        let _ = signal_hook::flag::register(signal, stop.clone());
    }

    // alternate screen, cursor hidden
    let _ = stdout.write_all(b"\x1b[?1049h\x1b[?25l");
    let _ = stdout.flush();
    let probed = match mode {
        ImageMode::Auto => probe(&mut stdin, &mut stdout, in_fd),
        _ => None,
    };
    let renderer = render::choose_renderer(mode, probed);
    log::info!(
        "view-images: {} file(s), mode {}, probe {:?}, drawing with {:?}",
        files.len(),
        mode.as_str(),
        probed,
        renderer
    );

    let mut pager = Pager::new(files.len());
    let mut loaded: (usize, Result<Rgba, String>) = (0, load(&files[0]));
    let mut dirty = true;
    let mut buf = [0u8; 1024];
    'viewing: while !stop.load(Ordering::Relaxed) {
        if resized.swap(false, Ordering::Relaxed) {
            dirty = true;
        }
        if dirty {
            if loaded.0 != pager.index {
                loaded = (pager.index, load(&files[pager.index]));
            }
            let screen = frame(
                renderer,
                window(out_fd),
                &pager,
                &files[pager.index],
                &loaded.1,
            );
            if stdout
                .write_all(&screen)
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
            dirty = false;
        }
        if !readable(in_fd, Duration::from_millis(100)) {
            continue;
        }
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for key in render::keys(&buf[..n]) {
            let before = pager.index;
            match key {
                Key::Quit => break 'viewing,
                Key::Next => pager.next(),
                Key::Previous => pager.previous(),
                Key::First => pager.first(),
                Key::Last => pager.last(),
                Key::Redraw => dirty = true,
            }
            if pager.index != before {
                dirty = true;
            }
        }
    }

    // nothing of the image is left in the terminal, and the screen is the one the pane had
    if renderer == Renderer::Kitty {
        let _ = stdout.write_all(&render::kitty_delete(image_id()));
    }
    let _ = stdout.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
    let _ = stdout.flush();
    drop(raw);
    0
}
