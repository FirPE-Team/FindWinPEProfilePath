mod utils;

use std::{env, process::ExitCode};

use clap::{CommandFactory, Parser, error::ErrorKind};
use utils::{SearchError, find_marker, validate_relative_path};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Find a file or directory at the root of a WinPE-accessible volume."
)]
/// Find a file or directory at the root of a WinPE-accessible volume.
struct Arguments {
    /// a relative path below a candidate volume root, for example WinPE or WinPE\\Version.txt
    #[arg(value_name = "relative-path")]
    path: String,

    /// print volume classification and recoverable probe errors to stderr
    #[arg(short, long)]
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
    if env::args_os().len() == 1 {
        let mut command = Arguments::command();
        let _ = command.print_help();
        println!();
        return Err(ExitCode::SUCCESS);
    }
    let verbose_requested = env::args_os().any(|argument| argument == "--verbose");
    match Arguments::try_parse_from(env::args_os()) {
        Ok(arguments) => Ok(arguments),
        Err(error) => {
            let exit_code = error.exit_code();
            let output = error.to_string();
            let use_stderr = verbose_requested
                && !matches!(
                    error.kind(),
                    ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
                );
            if use_stderr {
                eprint!("{output}");
            } else {
                print!("{output}");
            }
            Err(ExitCode::from(exit_code as u8))
        }
    }
}
