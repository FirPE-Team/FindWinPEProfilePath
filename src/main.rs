mod utils;

use std::{env, ffi::OsString, process::ExitCode};

use utils::{SearchError, find_marker, validate_relative_path_os};

struct Arguments {
    path: OsString,
    verbose: bool,
}

fn main() -> ExitCode {
    // 解析命令行参数
    let arguments = match parse_arguments() {
        Ok(arguments) => arguments,
        Err(exit_code) => return exit_code,
    };

    // 验证路径是否为相对路径
    if let Err(error) = validate_relative_path_os(&arguments.path) {
        if arguments.verbose {
            eprintln!("Invalid path: {error}");
        }
        return ExitCode::from(2);
    }

    // 搜索文件或目录
    match find_marker(&arguments.path, arguments.verbose) {
        Ok(Some(path)) => {
            println!("{path}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::from(1),
        Err(SearchError::System(error)) => {
            if arguments.verbose {
                eprintln!("Search failed: {error}");
            }
            ExitCode::from(2)
        }
    }
}

/// 解析命令行参数
fn parse_arguments() -> Result<Arguments, ExitCode> {
    let arguments: Vec<OsString> = env::args_os().collect();
    let verbose_requested = arguments.iter().any(|argument| argument == "--verbose");
    let program_name = arguments
        .first()
        .map(|argument| argument.to_string_lossy().into_owned())
        .unwrap_or_else(|| concat!(env!("CARGO_PKG_NAME"), ".exe").to_owned());
    let usage = || {
        format!(
            "Usage: {program_name} [--verbose] <relative-path>\n\nFind a file or directory at the root of a WinPE-accessible volume.\n"
        )
    };

    let mut path = None;
    let mut end_of_options = false;
    for argument in arguments.into_iter().skip(1) {
        if !end_of_options && argument == "--" {
            end_of_options = true;
        } else if !end_of_options && argument == "--verbose" {
            continue;
        } else if !end_of_options && (argument == "--help" || argument == "-h") {
            print!("{}", usage());
            return Err(ExitCode::SUCCESS);
        } else if !end_of_options && argument.to_string_lossy().starts_with('-') {
            let message = format!(
                "{}: unrecognized option\n{}",
                argument.to_string_lossy(),
                usage()
            );
            if verbose_requested {
                eprint!("{message}");
            } else {
                print!("{message}");
            }
            return Err(ExitCode::from(2));
        } else if path.is_some() {
            let message = format!("too many positional arguments\n{}", usage());
            if verbose_requested {
                eprint!("{message}");
            } else {
                print!("{message}");
            }
            return Err(ExitCode::from(2));
        } else {
            path = Some(argument);
        }
    }

    let Some(path) = path else {
        print!("{}", usage());
        return Err(ExitCode::SUCCESS);
    };
    Ok(Arguments {
        path,
        verbose: verbose_requested,
    })
}
