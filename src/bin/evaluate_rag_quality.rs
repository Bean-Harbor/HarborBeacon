use std::path::PathBuf;

use harborbeacon_local_agent::scripts::rag_quality::{load_suite, run_suite, write_report};

struct Cli {
    suite: PathBuf,
    output: PathBuf,
    source_revision: String,
}

impl Cli {
    fn parse() -> Self {
        let mut suite = PathBuf::from("tests/fixtures/rag-quality-v1/suite.json");
        let mut output = PathBuf::from("rag-quality-report.json");
        let mut source_revision = "unknown".to_string();
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--suite" => suite = PathBuf::from(take_value(&args, &mut index, "--suite")),
                "--output" => output = PathBuf::from(take_value(&args, &mut index, "--output")),
                "--source-revision" => {
                    source_revision = take_value(&args, &mut index, "--source-revision")
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                value => fail(&format!("unknown argument: {value}")),
            }
            index += 1;
        }
        Self {
            suite,
            output,
            source_revision,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let suite = load_suite(&cli.suite).unwrap_or_else(|error| fail(&error));
    let report = run_suite(&suite, cli.source_revision).unwrap_or_else(|error| fail(&error));
    write_report(&cli.output, &report).unwrap_or_else(|error| fail(&error));
    println!(
        "RAG quality gate: {} ({} cases); report={}",
        if report.gate.passed { "PASS" } else { "FAIL" },
        report.metrics.case_count,
        cli.output.display()
    );
    if !report.gate.passed {
        for reason in &report.gate.reasons {
            eprintln!("- {reason}");
        }
        std::process::exit(2);
    }
}

fn take_value(args: &[String], index: &mut usize, flag: &str) -> String {
    *index += 1;
    args.get(*index)
        .cloned()
        .unwrap_or_else(|| fail(&format!("missing value for {flag}")))
}

fn print_usage() {
    println!("Usage: evaluate-rag-quality [--suite PATH] [--output PATH] [--source-revision SHA]");
}

fn fail(message: &str) -> ! {
    eprintln!("evaluate-rag-quality: {message}");
    std::process::exit(1);
}
