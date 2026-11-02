mod ctl;

use std::env;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode};

use mightty_pty::{Pty, WindowSize};
use mightty_term::Term;

const USAGE: &str = "\
usage: mightty [--setting=value...] [-e <program> [args...]]
                              open a terminal window; with -e it runs
                              <program> in the current directory and closes
                              when the program exits (see `mightty --help`)
       mightty ctl <command>   control a running mightty (see `mightty ctl`)
       mightty mcp             MCP server for AI agents (stdio)
       mightty run [--cols N] [--rows N] [--screen] [--] <program> [args...]

`run` executes a program in a headless terminal and prints what it left
behind (scrollback + screen, or just the screen with --screen).

Add mightty to an agent, e.g. Claude Code:  claude mcp add mightty -- mightty mcp";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("ctl") => ctl::main(&args[1..]).map_err(io::Error::other),
        Some("mcp") => mightty_mcp::serve(mightty_mcp::Backend::connect(), io::stdin().lock(), io::stdout().lock())
            .map(|()| ExitCode::SUCCESS),
        Some("--version" | "-V") => {
            println!("mightty {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            #[cfg(target_os = "macos")]
            match mightty_macos::Config::load(&[]) {
                Ok((config, _)) => println!("\n{}", config.help().trim_end()),
                Err(e) => eprintln!("mightty: {e}"),
            }
            Ok(ExitCode::SUCCESS)
        }
        #[cfg(target_os = "macos")]
        _ if args.first().is_none_or(|a| a.starts_with('-')) => {
            match mightty_macos::Config::load(&args) {
                Ok((config, warnings)) => {
                    for warning in warnings {
                        eprintln!("mightty: {warning}");
                    }
                    mightty_macos::run(config).map(|()| ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!("mightty: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(ExitCode::from(2))
        }
    };
    result.unwrap_or_else(|e| {
        eprintln!("mightty: {e}");
        ExitCode::FAILURE
    })
}

fn run(args: &[String]) -> io::Result<ExitCode> {
    let (mut cols, mut rows, mut screen_only) = (80u16, 24u16, false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cols" | "--rows" => {
                let value = args.get(i + 1).and_then(|v| v.parse().ok()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("{} expects a number", args[i]))
                })?;
                if args[i] == "--cols" { cols = value } else { rows = value }
                i += 2;
            }
            "--screen" => {
                screen_only = true;
                i += 1;
            }
            "--" => {
                i += 1;
                break;
            }
            _ => break,
        }
    }
    let Some((program, program_args)) = args[i..].split_first() else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };

    let mut cmd = Command::new(program);
    cmd.args(program_args);
    let mut pty = Pty::spawn(cmd, WindowSize { cols, rows, ..Default::default() })?;
    let mut term = Term::new(cols.into(), rows.into());
    let mut reader = pty.reader()?;
    let mut writer = pty.writer()?;
    let mut buf = vec![0; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                term.feed(&buf[..n]);
                let replies = term.take_responses();
                if !replies.is_empty() {
                    writer.write_all(&replies)?;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // EIO: the child side closed (Linux).
            Err(_) => break,
        }
    }
    let status = pty.wait()?;

    let text = if screen_only { term.screen_text() } else { term.history_text() };
    println!("{text}");
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}
