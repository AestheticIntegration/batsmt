
#[macro_use] extern crate proptest;

use {
    std::{rc::Rc, fmt},
    fxhash::FxHashMap,
    batsmt_core::{ast::{self, HasManager, Manager},backtrack::*, ast_u32::{self, AST}},
    batsmt_cc::*,
    batsmt_hast::*,
    batsmt_pretty::{self as pp, Pretty1},
    batsmt_theory::{BoolLit, self as theory, lit_map},
};

type M = HManager<StrSymbolManager>;

// literals that are really just terms + sign.
//
// - `(true,a,b)` is `a=b`
// - `(false,a,b)` is `a!=b`
#[derive(Debug,Clone,Copy,Eq,PartialEq,Ord,PartialOrd,Hash)]
struct TermLit(bool,AST,AST);

mod term_lit {
    use super::*;

    impl TermLit {
        pub fn new(mut t1: AST, mut t2: AST, sign: bool) -> Self {
            // canonical
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            TermLit(sign,t1,t2)
        }
        pub fn mk_eq(t1: AST, t2: AST) -> Self { Self::new(t1,t2,true) }
        pub fn mk_neq(t1: AST, t2: AST) -> Self { Self::new(t1,t2,false) }
        pub fn sign(&self) -> bool { self.0 }
    }

    impl std::ops::Not for TermLit {
        type Output = Self;
        fn not(self) -> Self { TermLit(!self.0, self.1, self.2) }
    }

    impl BoolLit for TermLit {
        fn abs(&self) -> Self { TermLit(true, self.1, self.2) }
    }

    impl<M:ast_u32::ManagerU32> pp::Pretty1<M> for TermLit {
        fn pp_with(&self, m: &M, ctx: &mut pp::Ctx) {
            let s = if self.sign() {" = "} else {" != "};
            ctx.pp(&ast::pp(m,&self.1)).str(s).pp(&ast::pp(m,&self.2));
        }
    }
}

struct Ctx(M);

mod ctx {
    use super::*;
    impl HasManager for Ctx {
        type M = M;
        fn m(&self) -> &M { &self.0 }
        fn m_mut(&mut self) -> &mut M { &mut self.0 }
    }

    impl theory::BoolLitCtx for Ctx {
        type B = TermLit;
    }

    impl theory::Ctx for Ctx {}
}

type CC0 = CC<Ctx>;
type NaiveCC0 = NaiveCC<Ctx>;

// generate a series of operations for the congruence closure
mod prop_cc {
    use super::*;
    use proptest::{prelude::*,test_runner::Config};

    /// Context for generating terms
    #[derive(Clone)]
    struct AstGen(Rc<std::cell::RefCell<AstGenCell>>);

    struct AstGenCell {
        m: Ctx,
        b: Option<batsmt_cc::Builtins<AST>>,
        consts: FxHashMap<String, AST>,
    }

    impl AstGenCell {
        fn string(&mut self, s: String) -> AST {
            let c = &self.consts;
            match c.get(&s) {
                Some(t) => *t,
                None => {
                    let t = self.m.m_mut().mk_string(s.clone());
                    drop(c); // before the borrow
                    self.consts.insert(s, t);
                    t
                }
            }
        }
        fn str(&mut self, s: &str) -> AST { self.string(s.to_string()) }
    }

    impl AstGen {
        fn new(m: M) -> Self {
            let consts = FxHashMap::default();
            let m = Ctx(m);
            let mut cell = AstGenCell { m, consts, b: None, };
            // make builtins
            let b = batsmt_cc::Builtins{
                true_: cell.str("true"),
                false_: cell.str("false"),
                distinct: cell.str("distinct"),
                eq: cell.str("="),
                not_: cell.str("not"),
            };
            cell.b = Some(b);
            AstGen(Rc::new(std::cell::RefCell::new(cell)))
        }
        fn app(&self, f: AST, args: &[AST]) -> AST {
            self.0.borrow_mut().m.mk_app(f, args)
        }
        fn b(&self) -> Builtins<AST> { self.0.borrow_mut().b.clone().unwrap() }
    }

    impl ast::HasManager for AstGenCell {
        type M = Ctx;
        fn m(&self) -> &Self::M { &self.m }
        fn m_mut(&mut self) -> &mut Self::M { &mut self.m }
    }

    // just so we can `prop_map` on it
    impl fmt::Debug for AstGen {
        fn fmt(&self, out: &mut fmt::Formatter) -> fmt::Result { write!(out, "astgen") }
    }

    fn with_astgen<F,T>(mut f: F) -> BoxedStrategy<(AstGen,T)>
        where F: FnMut(&AstGen) -> BoxedStrategy<T>, T: 'static+fmt::Debug
    {
        let m = AstGen::new(HManager::new());
        f(&m)
            .prop_map(move |t| (m.clone(), t))
            .boxed()
    }

    /// Random generator of terms
    fn gen_term(m: &AstGen) -> BoxedStrategy<AST> {
        let m = m.clone();
        let leaf = {
            let m2 = m.clone();
            "f|g|a|b|c|d".prop_map(move |s| m2.0.borrow_mut().string(s))
        };
        // see https://docs.rs/proptest/*/proptest/#generating-recursive-data
        leaf.prop_recursive(
            8, 512, 10,
            move |inner| {
                let m2 = m.clone();
                (inner.clone(),prop::collection::vec(inner.clone(), 0..6)).
                    prop_map(move |(f,args)| m2.app(f,&args))
            }).boxed()
    }

    prop_compose! {
        fn gen_term2(m: &AstGen)(t1 in gen_term(m), t2 in gen_term(m)) -> (AST,AST) {
            (t1,t2)
        }
    }

    /// Operation on the congruence closure
    #[derive(Clone,Debug,Copy)]
    enum Op {
        PushLevel,
        PopLevels(usize),
        AssertEq(AST,AST),
        AssertNeq(AST,AST),
        PartialCheck,
        FinalCheck,
    }

    // check the sequence of ops is valid (doesn't pop too many levels)
    fn ops_valid(ops: &[Op]) -> bool {
        let mut lvl = 0;
        ops.iter().all(|op| match op {
            Op::PushLevel => { lvl += 1; true },
            Op::PopLevels(n) => { let ok = *n <= lvl; if ok{lvl -= *n}; ok },
            _ => true,
        })
    }

    // FIXME: first, allocate a vec of terms, then use `prop_flat_map` to sample terms
    fn cc_op(m: &AstGen) -> BoxedStrategy<Op> {
        prop_oneof![
            2 => Just(Op::PushLevel),
            1 => (1..5usize).prop_map(Op::PopLevels),
            7 => gen_term2(m).prop_map(|(t1,t2)| Op::AssertEq(t1,t2)),
            3 => gen_term2(m).prop_map(|(t1,t2)| Op::AssertNeq(t1,t2)),
            1 => Just(Op::PartialCheck),
            1 => Just(Op::FinalCheck),
        ].boxed()
    }

    // generates a vector of ops (size `i`)
    fn cc_ops(m: &AstGen, len: usize) -> BoxedStrategy<Vec<Op>> {
        prop::collection::vec(cc_op(m), 0..len)
            .prop_filter("invalid sequence of CC operations".to_string(), |v| ops_valid(&v))
            .boxed()
    }

    // use a naive CC to check this set of lits
    fn check_lits_sat<I,U>(m: &AstGen, i: I) -> bool
        where I: Iterator<Item=U>, U: Into<TermLit>
    {
        let b = m.b();
        let mut ncc = NaiveCC0::new(b.clone());
        let mut acts = theory::SimpleActions::new(|| unimplemented!("new lit"));

        for lit in i {
            let lit = lit.into();
            let TermLit(sign,t1,t2) = lit;
            if sign {
                let ctx = &mut m.0.borrow_mut().m;
                ncc.merge(ctx,t1,t2,lit)
            } else {
                let eqn = m.app(b.eq, &[t1,t2]); // `t1=t2`

                let ctx = &mut m.0.borrow_mut().m;
                ncc.merge(ctx,eqn, b.false_, lit)
            }
        }

        let ctx = &mut m.0.borrow_mut().m;
        ncc.final_check(ctx, &mut acts);
        acts.get().is_ok()
    }

    fn check_cube_is_unsat(m: &AstGen, cube: &[TermLit]) -> bool {
        ! check_lits_sat(m, cube.iter().cloned())
    }

    // test that NaiveCC's backtracking behavior is consistent
    proptest! {
        #![proptest_config(Config::with_cases(100))]
        #[test]
        fn proptest_naive_cc_backtrack(ref mut tup in with_astgen(|m| cc_ops(m, 100))) {
            let (m, ops) = tup;

            //println!("ops: {:?}", ops);

            let mut st = Stack::new(); // just accumulate lits
            let mut ncc = NaiveCC0::new(m.b());
            let b = m.b();
            let mut acts = theory::SimpleActions::new(|| unimplemented!("new lit"));

            for &op in ops.iter() {
                match op {
                    Op::PushLevel => {
                        let ctx = &mut m.0.borrow_mut().m;
                        st.push_level();
                        ncc.push_level(ctx);
                    },
                    Op::PopLevels(n) => {
                        acts.clear();
                        let ctx = &mut m.0.borrow_mut().m;
                        st.pop_levels(n, |_| ());
                        ncc.pop_levels(ctx, n);
                    },
                    Op::AssertEq(t1,t2) => {
                        let ctx = &mut m.0.borrow_mut().m;
                        let lit = TermLit::mk_eq(t1,t2);
                        st.push((t1,t2,lit));
                        ncc.merge(ctx, t1,t2,lit);
                    },
                    Op::AssertNeq(t1,t2) => {
                        let lit = TermLit::mk_neq(t1,t2);
                        let eqn = m.app(b.eq, &[t1,t2]); // term `t1=t2`
                        st.push((eqn, b.false_, lit));

                        let ctx = &mut m.0.borrow_mut().m;
                        ncc.merge(ctx, eqn, b.false_, lit);
                    },
                    Op::PartialCheck => (), // do nothing
                    Op::FinalCheck => {
                        // here be the main check
                        let r_ncc = {
                            let mut mr = m.0.borrow_mut();
                            let ctx = &mut mr.m;
                            ncc.final_check(ctx, &mut acts);
                            acts.get()
                        };
                        let sat1 = r_ncc.is_ok();

                        // check with a fresh ncc, without the push/pop stuff
                        let sat2 = {
                            let lits = st.iter().map(|(_,_,lit)| lit).cloned();
                            check_lits_sat(m, lits)
                        };

                        // must agree on satisfiability
                        prop_assert_eq!(sat1, sat2, "ncc-incremental.sat: {}, ncc-fresh.sat: {}", sat1, sat2);

                        // conflict returned by `ncc`, if any, must be valid
                        if let Err(confl) = r_ncc {
                            let lits = confl.iter().map(|lit| ! *lit);
                            let confl_sat = check_lits_sat(m, lits);

                            prop_assert!(! confl_sat, "ncc-incremental.conflict is sat");
                        }
                    }
                };
            }
        }
    }

    // test that CC and NaiveCC behave the same, and check CC conflicts
    // using naiveCC
    proptest! {
        #![proptest_config(Config::with_cases(80))]
        #[test]
        fn proptest_cc_is_correct(ref tup in with_astgen(|m| cc_ops(m, 120))) {
            let (m, ops) = tup;
            let mut stack = Stack::new(); // keep current set of ops

            //println!("ops: {:?}", ops);

            let mut cc = {
                let b = m.b();
                let m = &mut m.0.borrow_mut().m;
                CC0::new(m, b)
            };
            let mut ncc = NaiveCC0::new(m.b());
            let mut acts = theory::SimpleActions::new(|| unimplemented!("new lit"));
            let mut nacts = theory::SimpleActions::new(|| unimplemented!("new lit"));
            let b = m.b();

            // add literals, for propagations
            for &op in ops.iter() {
                match op {
                    Op::AssertEq(t1,t2) | Op::AssertNeq(t1,t2) => {
                        let mut mr = m.0.borrow_mut();
                        let lit = TermLit::mk_eq(t1,t2);
                        let eqn = mr.mk_app(b.eq, &[t1,t2]);
                        let ctx = &mut mr.m;
                        cc.add_literal(ctx, eqn, lit);
                    },
                    _ => (),
                }
            }

            for &op in ops.iter() {
                match op {
                    Op::PushLevel => {
                        let ctx = &mut m.0.borrow_mut().m;
                        cc.push_level(ctx);
                        ncc.push_level(ctx);
                        stack.push_level();
                    },
                    Op::PopLevels(n) => {
                        acts.clear();
                        nacts.clear();
                        let ctx = &mut m.0.borrow_mut().m;
                        cc.pop_levels(ctx,n);
                        ncc.pop_levels(ctx,n);
                        stack.pop_levels(n, |_| ());
                    },
                    Op::AssertEq(t1,t2) => {
                        let ctx = &mut m.0.borrow_mut().m;
                        let lit = TermLit::mk_eq(t1,t2);
                        cc.merge(ctx,t1,t2,lit);
                        ncc.merge(ctx,t1,t2,lit);
                        stack.push(lit);
                    },
                    Op::AssertNeq(t1,t2) => {
                        let lit = TermLit::mk_neq(t1,t2);
                        let eqn = m.app(b.eq, &[t1,t2]); // term `t1=t2`
                        let ctx = &mut m.0.borrow_mut().m;
                        cc.merge(ctx,eqn, b.false_, lit);
                        ncc.merge(ctx,eqn, b.false_, lit);
                        stack.push(lit);
                    },
                    Op::PartialCheck => {
                        let r1 = {
                            let mut mr = m.0.borrow_mut();
                            let ctx = &mut mr.m;
                            cc.partial_check(ctx, &mut acts);
                            acts.get()
                        };

                        match r1 {
                            Ok((props,_)) => {
                                let props = props.clone();
                                drop(r1);
                                // check each propagation using a copy of `ncc`
                                for lit in props.iter().cloned() {
                                    check_propagation(m, lit, stack.as_slice());
                                    let expl = {
                                        let ctx = &mut m.0.borrow_mut().m;
                                        let r = cc.explain_prop(ctx, lit);
                                        drop(ctx);
                                        r
                                    };
                                    check_propagation_expl(m, lit, expl);
                                }
                            },
                            Err(confl) => {
                                // check conflict, using a fresh new naiveCC
                                check_confl(m, &confl);
                            }
                        }
                    },
                    Op::FinalCheck => {
                        // here be the main check
                        let (r1,r2) = {
                            let mut mr = m.0.borrow_mut();
                            let ctx = &mut mr.m;
                            cc.final_check(ctx, &mut acts);
                            ncc.final_check(ctx, &mut nacts);
                            (acts.get(),nacts.get())
                        };

                        // must agree on satisfiability
                        let sat1 = r1.is_ok();
                        let sat2 = r2.is_ok();
                        prop_assert_eq!(sat1, sat2, "cc.sat: {}, ncc.sat: {}", sat1, sat2);

                        match r1 {
                            Ok((props,_)) => {
                                let props = props.clone();
                                drop(r1);
                                // check each propagation using a copy of `ncc`
                                for lit in props.iter().cloned() {
                                    check_propagation(m, lit, &stack.as_slice());
                                    let expl = {
                                        let ctx = &mut m.0.borrow_mut().m;
                                        let r = cc.explain_prop(ctx, lit);
                                        drop(ctx);
                                        r
                                    };
                                    check_propagation_expl(m, lit, expl);
                                }
                            },
                            Err(confl) => {
                                // check conflict, using a fresh new naiveCC
                                check_confl(m, &confl);
                            }
                        }
                    }
                };
            }
        }
    }

    // check that the propagation is valid (ie. ¬b is inconsistent with current trail)
    fn check_propagation(m: &AstGen, lit: TermLit, trail: &[TermLit]) {
        let mut cube = vec![!lit];
        cube.extend_from_slice(trail);

        let is_unsat = check_cube_is_unsat(m, &cube);
        let ctx = &mut m.0.borrow_mut().m;

        //prop_assert!(r.is_err(), "¬lit where lit was propagated should be unsat");
        assert!(
            is_unsat,
            "propagation {} should be a tauto in current trail {}, but naive cc returned sat",
            pp::display(lit.pp(ctx)),
            pp::display(pp::sexp_iter(trail.iter().map(|x| x.pp(ctx)))),
            );
    }

    // check that the propagation's explanation is valid
    // (ie `expl => lit` is valid, ie `expl & !lit` is unsat)
    fn check_propagation_expl(m: &AstGen, lit: TermLit, expl: &[TermLit]) {
        // build the cube `expl & !lit`
        let mut cube = vec![!lit];
        for &lit2 in expl.iter() {
            cube.push(lit2);
        }

        let is_unsat = check_cube_is_unsat(m, &cube);
        let ctx = &mut m.0.borrow_mut().m;

        assert!(
            is_unsat,
            "for propagation ({} => {})\n\
            negated cube {} should be unsat, but naive cc returned sat",
            pp::display(pp::sexp_iter(expl.iter().map(|x| x.pp(ctx)))),
            lit.pp(ctx),
            pp::display(pp::sexp_iter(cube.iter().map(|x| x.pp(ctx)))));
    }

    // check that the conflict is valid
    fn check_confl(m: &AstGen, confl: &[TermLit]) {
        let is_unsat = check_cube_is_unsat(m, confl);
        let ctx = &mut m.0.borrow_mut().m;

        //assert!(r.is_err(), "conflict should be unsat");
        assert!(
            is_unsat, "conflict {:?} should be unsat, but naive cc returned sat",
            pp::debug(pp::sexp_iter(confl.iter().map(|x| x.pp(ctx)))));
    }
}
