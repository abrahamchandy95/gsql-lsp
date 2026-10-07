use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
gsql-lsp: a language server for TigerGraph GSQL

USAGE:
    gsql-lsp [--stdio]                 Run the language server over stdin/stdout
    gsql-lsp check [--errors-only] [--format text|github|json] <path>...
                                       Report diagnostics for files or directories
                                       (github: annotations for GitHub Actions)
    gsql-lsp format [--check] [--keyword-case upper|lower|preserve] [--indent N] <path>...
                                       Re-indent files (and split long parameter and
                                       tuple lists) in place (`-` formats stdin to
                                       stdout); --check lists files that would change
    gsql-lsp config <editor> [--variant NAME | --all] [--settings] [--absolute] [--repo URL]
                                       Print a ready-to-paste configuration for neovim,
                                       vscode, helix, zed, emacs or vim; --settings: only
                                       the server settings with their defaults;
                                       --absolute: use this executable's path
    gsql-lsp config --list             List the editors and their variants
    gsql-lsp --version                 Print the version
    gsql-lsp --help                    Print this help
";

fn main() -> ExitCode {
    std::thread::Builder::new()
        .name("gsql-lsp".into())
        .stack_size(gsql_lsp::STACK_SIZE)
        .spawn(run)
        .expect("spawn the main thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

/// 2 when a requested file could not be read, else 1 when there were problems.
fn exit_code(summary: gsql_lsp::check::Summary) -> ExitCode {
    if summary.unreadable > 0 {
        ExitCode::from(2)
    } else if summary.problems > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("--stdio") => {
            let stdin = BufReader::new(io::stdin());
            let stdout = io::stdout();
            let code = gsql_lsp::server::run(stdin, stdout.lock());
            ExitCode::from(code as u8)
        }
        Some("check") => {
            let mut options = gsql_lsp::check::Options {
                paths: Vec::new(),
                errors_only: false,
                format: gsql_lsp::check::OutputFormat::Text,
            };
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--errors-only" => options.errors_only = true,
                    "--format" => {
                        options.format = match rest.next().map(String::as_str) {
                            Some("text") => gsql_lsp::check::OutputFormat::Text,
                            Some("github") => gsql_lsp::check::OutputFormat::Github,
                            Some("json") => gsql_lsp::check::OutputFormat::Json,
                            _ => {
                                eprintln!("--format takes text, github or json\n\n{USAGE}");
                                return ExitCode::from(2);
                            }
                        }
                    }
                    "--help" | "-h" => {
                        print!("{USAGE}");
                        return ExitCode::SUCCESS;
                    }
                    flag if flag.starts_with('-') => {
                        eprintln!("unknown option {flag}\n\n{USAGE}");
                        return ExitCode::from(2);
                    }
                    path => options.paths.push(PathBuf::from(path)),
                }
            }
            if options.paths.is_empty() {
                options.paths.push(PathBuf::from("."));
            }
            match gsql_lsp::check::run(&options, &mut io::stdout().lock()) {
                Ok(summary) => exit_code(summary),
                // The reader went away (`gsql-lsp check | head`).
                Err(err) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("gsql-lsp: {err}");
                    ExitCode::from(2)
                }
            }
        }
        Some("format") => {
            let mut options = gsql_lsp::format::Options::default();
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--check" => options.check = true,
                    "--keyword-case" => {
                        options.keyword_case = match rest.next().map(String::as_str) {
                            Some("upper") => gsql_lsp::features::KeywordCase::Upper,
                            Some("lower") => gsql_lsp::features::KeywordCase::Lower,
                            Some("preserve") => gsql_lsp::features::KeywordCase::Preserve,
                            _ => {
                                eprintln!("--keyword-case takes upper, lower or preserve\n\n{USAGE}");
                                return ExitCode::from(2);
                            }
                        }
                    }
                    "--indent" => match rest.next().and_then(|n| n.parse::<u32>().ok()) {
                        Some(n) if n > 0 => options.indent = n,
                        _ => {
                            eprintln!("--indent takes a positive number\n\n{USAGE}");
                            return ExitCode::from(2);
                        }
                    },
                    "-" => options.paths.push(PathBuf::from("-")),
                    "--help" | "-h" => {
                        print!("{USAGE}");
                        return ExitCode::SUCCESS;
                    }
                    flag if flag.starts_with('-') => {
                        eprintln!("unknown option {flag}\n\n{USAGE}");
                        return ExitCode::from(2);
                    }
                    path => options.paths.push(PathBuf::from(path)),
                }
            }
            if options.paths.is_empty() {
                options.paths.push(PathBuf::from("."));
            }
            match gsql_lsp::format::run(&options, &mut io::stdout().lock()) {
                Ok(summary) => exit_code(summary),
                Err(err) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("gsql-lsp: {err}");
                    ExitCode::from(2)
                }
            }
        }
        Some("config") => {
            match gsql_lsp::editor_config::run(&args[1..], &mut io::stdout().lock(), &mut io::stderr().lock()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("gsql-lsp config: {message}\n\n{}", gsql_lsp::editor_config::HELP);
                    ExitCode::from(2)
                }
            }
        }
        Some("--version" | "-V" | "version") => {
            println!("gsql-lsp {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown argument {other}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
