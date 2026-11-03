// revue: git change viewer that runs in the browser. Argument parsing, port fallback, opening the browser.
mod definitions;
mod discovery;
mod folder;
mod git;
mod http;
mod image;
mod mcp;
mod paths;
mod repo;
mod server;
mod watcher;

use std::path::PathBuf;

const HELP: &str = "revue: git change viewer that runs in the browser

Usage: revue [dir] [--port N] [--no-open] [--no-git] [--notes memory|file] [--idle-timeout MIN]
       revue mcp [--lifetime session|detached] [--notes memory|file] [--idle-timeout MIN]
       revue --version

  dir                any folder (default: current directory); inside a git repo, the repo's
                     changes, history and branches are shown, and any other folder's files
  --no-git           show dir's files only, as for a folder that isn't in a repo
  --port N           port to listen on, 0 for any free port (default: 4777, or a random
                     free port if 4777 is in use; an explicit port that is in use is an error)
  --no-open          don't open the browser
  --notes STORAGE    where agent notes are kept: memory (default, lost on restart) or
                     file (.git/revue/notes.json, or for a folder outside a repo, under
                     $XDG_DATA_HOME/revue/notes)                        [env REVUE_NOTES]
  --idle-timeout MIN exit after MIN minutes with no open tabs and no requests
                     (default: never)                            [env REVUE_IDLE_TIMEOUT]

  mcp                run as an MCP server over stdio, so agents can open revue and jump to lines
  --lifetime L       servers the MCP starts stop with it (session, default) or keep running
                     (detached, with a 60 minute idle timeout)          [env REVUE_LIFETIME]

  -V, --version      print the version and the commit it was built from
  -h, --help         print this help";

// "revue 0.2.0 (1a2b3c4)", with ", modified" when the build had changes not in that commit, and no commit at all when
// it was built outside a git checkout (build.rs).
fn version() -> String {
    let v = env!("CARGO_PKG_VERSION");
    match (env!("REVUE_COMMIT"), env!("REVUE_MODIFIED").is_empty()) {
        ("", _) => format!("revue {v}"),
        (c, true) => format!("revue {v} ({c})"),
        (c, false) => format!("revue {v} ({c}, modified)"),
    }
}

pub struct Opts {
    pub port: u16,
    pub port_set: bool,
    pub open: bool,
    pub notes: String,
    pub lifetime: String,
    pub idle_timeout: f64,
    pub no_git: bool,
}

fn show(v: Option<&String>) -> String {
    v.cloned().unwrap_or_else(|| "undefined".into())
}

// Parse --flag value pairs shared by both commands; returns the leftover positional args.
fn parse_flags(args: &[String], opts: &mut Opts) -> Result<Vec<String>, String> {
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--port" => {
                i += 1;
                let v = args.get(i);
                match v.filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).and_then(|v| v.parse::<u32>().ok()) {
                    Some(n) if n <= 65535 => {
                        opts.port = n as u16;
                        opts.port_set = true;
                    }
                    _ => {
                        let not = v.map(|v| format!(", not {v}")).unwrap_or_default();
                        return Err(format!("--port needs a port number from 0 to 65535{not}"));
                    }
                }
            }
            "--no-open" => opts.open = false,
            "--no-git" => opts.no_git = true,
            "--notes" => {
                i += 1;
                opts.notes = show(args.get(i));
            }
            "--idle-timeout" => {
                i += 1;
                opts.idle_timeout = server::js_number(args.get(i).map(|s| s.as_str()).or(Some("x"))).unwrap_or(f64::NAN);
            }
            "--lifetime" => {
                i += 1;
                opts.lifetime = show(args.get(i));
            }
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }
    if !["memory", "file"].contains(&opts.notes.as_str()) {
        return Err(format!("--notes must be memory or file, not {}", opts.notes));
    }
    if !["session", "detached"].contains(&opts.lifetime.as_str()) {
        return Err(format!("--lifetime must be session or detached, not {}", opts.lifetime));
    }
    if opts.idle_timeout.is_nan() || opts.idle_timeout < 0.0 {
        return Err("--idle-timeout must be a number of minutes".into());
    }
    Ok(rest)
}

// Exit normally on signals so discovery files are removed. The handler only writes a byte to a pipe (write is
// async-signal-safe) and a thread does the exit. Nothing is blocked: a blocked mask would be inherited by every process
// we start (git, the browser), which then couldn't be interrupted or terminated.
#[cfg(unix)]
static SIGNAL_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    let fd = SIGNAL_PIPE.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 {
        unsafe { libc::write(fd, b"x".as_ptr() as *const libc::c_void, 1) };
    }
}

#[cfg(unix)]
fn handle_signals() {
    unsafe {
        let mut fds = [0 as libc::c_int; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        // A full pipe must never block the handler; one byte is enough to wake the thread.
        libc::fcntl(write_fd, libc::F_SETFL, libc::fcntl(write_fd, libc::F_GETFL) | libc::O_NONBLOCK);
        let reader = git::spawn(move || {
            let mut b = [0u8; 1];
            loop {
                let n = libc::read(read_fd, b.as_mut_ptr() as *mut libc::c_void, 1);
                if n > 0 {
                    discovery::exit(0);
                }
                if n == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    return;
                }
            }
        });
        // With no thread to wake, signals keep their default action: they end the process, only without removing
        // its discovery entry, which list_servers drops once the pid is gone.
        if let Err(e) = reader {
            eprintln!("revue: signals not handled: could not start a thread: {e}");
            libc::close(read_fd);
            libc::close(write_fd);
            return;
        }
        SIGNAL_PIPE.store(write_fd, std::sync::atomic::Ordering::Relaxed);
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        act.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut act.sa_mask);
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaction(sig, &act, std::ptr::null_mut());
        }
    }
}
// Windows: Ctrl+C, Ctrl+Break and closing the console. The system runs the handler on a thread of its own, so it can
// exit through discovery::exit directly; there is no async-signal-safety rule to respect, unlike a Unix handler.
#[cfg(windows)]
fn handle_signals() {
    unsafe extern "system" fn on_ctrl(_kind: u32) -> windows_sys::core::BOOL {
        discovery::exit(0)
    }
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}
#[cfg(not(any(unix, windows)))]
fn handle_signals() {}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("{}", version());
        return;
    }
    let mut opts = Opts {
        port: 4777,
        port_set: false,
        open: true,
        notes: env("REVUE_NOTES").unwrap_or_else(|| "memory".into()),
        lifetime: env("REVUE_LIFETIME").unwrap_or_else(|| "session".into()),
        idle_timeout: server::js_number(Some(&env("REVUE_IDLE_TIMEOUT").unwrap_or_else(|| "0".into()))).unwrap_or(f64::NAN),
        no_git: false,
    };
    let rest = match parse_flags(&args, &mut opts) {
        Ok(r) => r,
        Err(e) => {
            let usage = HELP.split("\n\n").nth(1).unwrap_or("");
            eprintln!("revue: {e}\n{usage}\nRun revue --help for details.");
            std::process::exit(1);
        }
    };
    handle_signals();

    if rest.first().map(|s| s.as_str()) == Some("mcp") {
        mcp::run(opts);
    }

    let dir = std::path::absolute(PathBuf::from(rest.first().map(|s| s.as_str()).unwrap_or("."))).unwrap_or_else(|_| PathBuf::from("."));
    // Inside a repo, its work tree's top level; anywhere else (or with --no-git), the folder itself, as a plain folder.
    let repo_root = if opts.no_git { None } else { repo::toplevel(&dir) };
    let git = repo_root.is_some();
    let root = match repo_root {
        Some(r) => r,
        None => match std::fs::canonicalize(&dir) {
            Ok(r) if r.is_dir() => r,
            Ok(_) => {
                eprintln!("revue: {} is not a folder", dir.display());
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("revue: {}: {e}", dir.display());
                std::process::exit(1);
            }
        },
    };
    // Servers that the MCP starts in detached mode label themselves so `revue_status` can tell them apart.
    let lifetime = if std::env::var("REVUE_SPAWNED_BY").as_deref() == Ok("mcp") { "detached" } else { "external" };
    let make = |port| server::Options {
        port,
        notes: opts.notes.clone(),
        idle_timeout: opts.idle_timeout,
        lifetime: lifetime.into(),
        git,
        on_idle: Some(Box::new(|| {
            println!("revue: idle, exiting");
            discovery::exit(0);
        })),
    };
    let srv = match server::start(&root, make(opts.port)) {
        Ok(s) => s,
        // Only the default port falls back to a free one; a port you asked for must be the one you get.
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && !opts.port_set => match server::start(&root, make(0)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("revue: {e}");
                std::process::exit(1);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("revue: port {} is already in use", opts.port);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("revue: {e}");
            std::process::exit(1);
        }
    };
    let url = format!("http://127.0.0.1:{}/", srv.port());
    let kind = if git { "" } else { " (folder)" };
    println!("revue: {}{kind}\n       {url}  (Ctrl+C to stop)", root.display());
    if opts.open {
        server::open_browser(&url);
    }
    loop {
        std::thread::park();
    }
}
