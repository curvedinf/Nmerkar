// trans — C-subset → Enmerkar (.ent) transpiler.
//
// Pipeline: read → lex → parse (AST) → emit (Enmerkar text) → stdout.
// Errors are positioned diagnostics on stderr; exit 1.

mod ast;
mod emit;
mod error;
mod lexer;
mod parser;
mod token;

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: trans file.c [file2.c ...]");
        return ExitCode::from(1);
    }
    // Multiple inputs are concatenated in argument order; diagnostics report
    // the originating file by tracking each input's starting line.
    let mut files: Vec<(String, String)> = Vec::new();
    for path in &args[1..] {
        match std::fs::read_to_string(path) {
            Ok(s) => files.push((path.clone(), s)),
            Err(_) => {
                eprintln!("cannot open input: {path}");
                return ExitCode::from(1);
            }
        }
    }
    let mut spans: Vec<(String, usize, usize)> = Vec::new(); // (name, start_line, end_line)
    let mut combined = String::new();
    let mut line = 1usize;
    for (name, content) in &files {
        spans.push((name.clone(), line, line + content.lines().count()));
        combined.push_str(content);
        combined.push('\n');
        line += content.lines().count() + 1;
    }
    match run(&combined) {
        Ok(out) => {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(out.as_bytes());
            let _ = stdout.flush();
            ExitCode::SUCCESS
        }
        Err(e) => {
            let loc = spans
                .iter()
                .find(|(_, s, en)| e.line >= *s && e.line < *en)
                .map(|(n, s, _)| (n.clone(), e.line - s + 1))
                .unwrap_or((files[0].0.clone(), e.line));
            eprintln!("{}:{}:{}: {}", loc.0, loc.1, e.col, e.msg);
            ExitCode::from(1)
        }
    }
}

fn run(src: &str) -> error::Result<String> {
    let toks = lexer::lex(src)?;
    let prog = parser::Parser::new(toks).parse_program()?;
    emit::emit(&prog)
}
