mod utils;

use std::{env, process::ExitCode};

use argh::FromArgs;
use utils::{SearchError, find_marker, validate_relative_path};

#[derive(FromArgs)]
/// Find a file or directory at the root of a WinPE-accessible volume.
struct Arguments {
    /// a relative path below a candidate volume root, for example FirPE or FirPE\\Version.txt
    #[argh(positional)]
    path: String,

    /// print volume classification and recoverable probe errors to stderr
    #[argh(switch)]
    verbose: bool,
}

fn main() -> ExitCode {
    // 解析命令行参数
    let arguments = match parse_arguments() {
        Ok(arguments) => arguments,
        Err(exit_code) => return exit_code,
    };

    // 验证路径是否为相对路径
    if let Err(error) = validate_relative_path(&arguments.path) {
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
    let verbose_requested = env::args_os().any(|argument| argument == "--verbose");
    let arguments: Vec<String> = match env::args_os()
        .map(|argument| argument.into_string())
        .collect()
    {
        Ok(arguments) => arguments,
        Err(argument) => {
            if verbose_requested {
                eprintln!("Invalid UTF-8 argument: {}", argument.to_string_lossy());
            }
            return Err(ExitCode::from(2));
        }
    };
    let program_name = arguments
        .first()
        .map(String::as_str)
        .unwrap_or(concat!(env!("CARGO_PKG_NAME"), ".exe"));
    let values: Vec<&str> = arguments.iter().map(String::as_str).collect();

    match Arguments::from_args(&[program_name], &values[1..]) {
        Ok(arguments) => Ok(arguments),
        Err(early_exit) => match early_exit.status {
            Ok(()) => {
                print!("{}", early_exit.output);
                Err(ExitCode::SUCCESS)
            }
            Err(()) => {
                if verbose_requested {
                    eprint!("{}", early_exit.output);
                } else {
                    print!("{}", early_exit.output);
                }
                Err(ExitCode::from(2))
            }
        },
    }
}
