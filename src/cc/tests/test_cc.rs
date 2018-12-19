
extern crate batsmt_core;
extern crate batsmt_cc;

mod cc {
    use batsmt_core::{ast,StrSymbol};
    use batsmt_cc::*;
    use batsmt_theory::IntLit;

    type M = ast::Manager<StrSymbol>;

    fn b(m: &M) -> Builtins {
        let m = m.clone();
        Builtins {
            true_: m.mk_str("true"),
            false_: m.mk_str("false"),
            eq: m.mk_str("="),
            distinct: m.mk_str("distinct"),
        }
    }

    #[test]
    fn test_new() {
        let m : ast::Manager<StrSymbol> = ast::Manager::new();
        let mut cc: CC<_,IntLit> = CC::new(&m, b(&m));

        // TODO: actual tests, using proptest, comparing to naivecc

        // access m
        m.mk_str("f");
    }

}
