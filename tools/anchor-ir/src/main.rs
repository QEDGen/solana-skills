mod analyzer;
mod ir;

use analyzer::analyze_project;
use std::env;
use std::path::PathBuf;

fn usage() -> &'static str {
    "Usage: cargo run --manifest-path tools/anchor-ir/Cargo.toml -- [--idl path/to/idl.json] [--input path/to/lib.rs] [--tests test.ts ...] [--output-dir dir]"
}

fn main() {
    let mut args = env::args().skip(1);
    let mut idl: Option<PathBuf> = None;
    let mut input: Option<PathBuf> = None;
    let mut tests: Vec<PathBuf> = Vec::new();
    let mut output_dir: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--idl" => idl = args.next().map(PathBuf::from),
            "--input" => input = args.next().map(PathBuf::from),
            "--tests" => {
                if let Some(value) = args.next() {
                    tests.push(PathBuf::from(value));
                }
            }
            "--output-dir" => output_dir = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                println!("{}", usage());
                return;
            }
            other => {
                eprintln!("Unexpected argument: {other}");
                eprintln!("{}", usage());
                std::process::exit(1);
            }
        }
    }

    if idl.is_none() && input.is_none() {
        eprintln!("{}", usage());
        std::process::exit(1);
    }

    match analyze_project(idl.as_deref(), input.as_deref(), &tests, output_dir.as_deref()) {
        Ok(summary) => println!("{summary}"),
        Err(error) => {
            eprintln!("analyzer error: {error}");
            std::process::exit(1);
        }
    }
}
