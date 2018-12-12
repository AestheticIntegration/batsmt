
// A test binary

extern crate env_logger;
extern crate batsmt_core;
extern crate batsmt_cc;
extern crate batsmt_parser;
extern crate batsmt_pretty;
extern crate batsmt_tseitin;
extern crate fxhash;
#[macro_use] extern crate log;

mod ast_builder;
mod ast_printer;

use {
    std::{env,fs,error::Error},
    batsmt_core::{ast,StrSymbol,solver},
    batsmt_cc as cc,
    batsmt_parser::{self as parser, Statement},
    batsmt_tseitin::Tseitin,
    crate::ast_printer::PP,
};

fn main() -> Result<(), Box<Error>> {
    env_logger::init();

    let m : ast::Manager<StrSymbol> = ast::Manager::new();

    let mut builder = ast_builder::AstBuilder::new(&m);

    // parse
    let stmts = {
        let mut args = env::args();
        match args.skip(1).next() {
            None => {
                info!("parse stdin");
                parser::parse_stdin(&mut builder)?
            },
            Some(file) => {
                info!("parse file {:?}", file);
                let file = fs::File::open(file)?;
                parser::parse(&mut builder, file)?
            },
        }
    };

    info!("parsed {} statements", stmts.len());

    let mut solver =
        solver::Solver::new(&m, builder.builtins(),
            cc::CCTheory::new(&m, builder.builtins()));

    // Tseitin transformation, to handle formulas
    let tseitin = Tseitin::new(&m, builder.builtins());

    for s in &stmts {
        info!("parsed statement {}", PP::new(&m, s.clone()));

        // process statement
        match s {
            Statement::Assert(t) => {
                let cs = tseitin.clauses(t);
                for c in cs.into_iter() {
                    solver.add_clause(c);
                }
            },
            Statement::CheckSat => {
                let r = solver.solve();
                println!("{:?}", r)
            },
            Statement::Exit => {
                info!("exit");
                break;
            }
            _ => (),
        }
    }

    Ok(())
}