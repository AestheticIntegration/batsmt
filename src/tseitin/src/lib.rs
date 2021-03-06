
//! Tseitin Transformation
//!
//! See [wikipedia](https://en.wikipedia.org/wiki/Tseytin_transformation)
//! for a primer.

#[macro_use] extern crate log;

use {
    batsmt_core::{
        ast_u32::{self, AST, }, gc, AstView,
        ast::{self, AstMap, iter_dag::State as AstIter},
    },
    fxhash::FxHashSet,
    batsmt_theory::{
        self as theory, TheoryLit, TheoryClauseSet, TheoryClauseRef,
        LitMap, pp_ast, },
};

/// A boolean-centric view of formulas.
pub enum View<'a, AST> {
    TyBool,
    Bool(bool),
    Not(AST),
    And(&'a [AST]),
    Or(&'a [AST]),
    Imply(&'a [AST]),
    Eq(AST,AST),
    Distinct(&'a [AST]),
    Atom(AST), // other
    Ite(AST,AST,AST),
}

/// A relatively big small-vec
type SVec<T> = smallvec::SmallVec<[T; 6]>;

pub trait Ctx : theory::Ctx {
    /// How to view an AST.
    fn view_as_formula(&self, t: AST) -> View<AST>;

    /// How to build an AST.
    fn mk_formula(&mut self, v: View<AST>) -> AST;

    fn is_bool(&self, t: AST) -> bool {
        self.view_as_formula(t).is_bool()
    }
    fn is_true(&self, t: AST) -> bool {
        self.view_as_formula(t).is_true()
    }
    fn is_false(&self, t: AST) -> bool {
        self.view_as_formula(t).is_false()
    }
}

impl<'a, AST> View<'a, AST> {
    #[inline(always)]
    pub fn is_bool(&self) -> bool {
        match self { View::Bool(_) => true, _ => false }
    }
    #[inline(always)]
    pub fn is_true(&self) -> bool {
        match self { View::Bool(true) => true, _ => false }
    }
    #[inline(always)]
    pub fn is_false(&self) -> bool {
        match self { View::Bool(false) => true, _ => false }
    }
}

/// Main state for the Tseitin transformation.
///
/// The state remembers which formulas have been translated to clauses already.
#[derive(Clone)]
pub struct Tseitin<C:Ctx> {
    simp_map: ast::HashMap<AST, AST>, // for simplify
    iter: AstIter<AST, ast_u32::HashSet>, // to traverse subterms
    tmp: Vec<TheoryLit<C>>, // temp clause
    tmp2: Vec<TheoryLit<C>>, // temp clause
    tmp_ast: Vec<AST>, // for arguments
    cs: TheoryClauseSet<C>, // clauses
    lits: FxHashSet<TheoryLit<C>>, // lits
}

/// Temporary structure
struct LitMapB<'a, C:Ctx, LM: LitMap<C::B>> {
    m: &'a mut C,
    lit_map: &'a mut LM,
}

impl<'a,C,LM> LitMapB<'a,C,LM>
    where C: Ctx, LM:LitMap<C::B>
{
    /// Map `t,sign` to either a theory literal, or a lazy pure boolean literal
    fn term_to_lit(&mut self, t: &AST) -> TheoryLit<C> {
        let (t,sign) = self.lit_map.unfold_not(self.m, t, true);
        let view_t = self.m.view_as_formula(t);
        match view_t {
            View::Bool(..) => {
                TheoryLit::new_b(t, sign)
            },
            View::And(..) | View::Or(..) | View::Imply(..) => {
                TheoryLit::new_t(t, sign)
            },
            View::Distinct(args) if args.len() == 2 => {
                // turn `distinct(a,b)` into `!(a=b)`
                let t0 = args[0];
                let t1 = args[1];
                drop(view_t);
                let eqn = self.m.mk_formula(View::Eq(t0, t1));
                ! TheoryLit::new_t(eqn, sign)
            },
            View::Distinct(..) => {
                TheoryLit::new_b(t,sign) // encoded away
            },
            View::TyBool => panic!("type bool doesn't map to a literal"),
            View::Not(..) => panic!("should not have a negation"), // unfold-not
            View::Atom(..) | View::Eq(..) | View::Ite(..) => {
                // theory literal
                TheoryLit::new_t(t, sign)
            },
        }
    }
}

#[derive(Copy,Clone,Debug,PartialEq)]
enum Conn { And, Or, }

struct SimpStruct<'a, C:Ctx> {
    m: &'a mut C,
    map: &'a mut ast::HashMap<AST, AST>,
}

/// Push each element `t` of `args` into `v`, but if `t=conn(u1…un)` then flatten `u1…un` into `v`
fn flatten_conn_rec<C:Ctx>(m: &C, conn: Conn, v: &mut SVec<AST>, args: &[AST]) {
    for t in args.iter() {
        match m.view_as_formula(*t) {
            View::And(args2) if conn == Conn::And => {
                flatten_conn_rec(m, conn, v, args2)
            },
            View::Or(args2) if conn == Conn::Or => {
                flatten_conn_rec(m, conn, v, args2)
            },
            View::Bool(true) if conn == Conn::And => (), // skip
            View::Bool(false) if conn == Conn::Or => (), // skip
            _ => {
                v.push(*t)
            }
        }
    }
}

/// Push each element `t` of `args` into `v`, but if `t=conn(u1…un)` then flatten `u1…un` into `v`
fn flatten_conn<C:Ctx>(m: &C, conn: Conn, v: &mut SVec<AST>, args: &[AST]) {
    flatten_conn_rec(m, conn, v, args);
    v.sort_unstable();
    v.dedup();
}

impl<'a, C:Ctx> SimpStruct<'a, C> {
    fn simplify_rec(&mut self, t: AST) -> AST {
        if let Some(u) = self.map.get(&t) {
            *u // in cache
        } else {
            //trace!("simplify-rec {}", pp::pp1(self.m, &t));
            let view_t = self.m.view_as_formula(t);
            let u = match view_t {
                View::Bool(..) | View::TyBool => t,
                View::Distinct(&[_]) => {
                    self.m.mk_formula(View::Bool(true))
                },
                View::Distinct(&[a,b]) => {
                    let a = self.simplify_rec(a);
                    let b = self.simplify_rec(b);
                    let eq = self.m.mk_formula(View::Eq(a,b));
                    self.m.mk_formula(View::Not(eq))
                },
                View::Distinct(args) => {
                    // distinct(t1…tn) --> and_{i<j} t_i != t_j
                    // simplify each subterm first
                    let mut args: Vec<AST> = args.iter().cloned().collect();
                    drop(view_t);

                    for u in args.iter_mut() { *u = self.simplify_rec(*u) }
                    let mut conj = vec!();

                    for i in 0 .. args.len()-1 {
                        let t_i = args[i];
                        for j in i+1 .. args.len() {
                            let t_j = args[j];
                            let eqn_i_j = self.m.mk_formula(View::Eq(t_i, t_j));
                            conj.push(self.m.mk_formula(View::Not(eqn_i_j)));
                        }
                    }

                    self.m.mk_formula(View::And(&conj))
                },
                View::Eq(t, u) if t==u => {
                    self.m.mk_formula(View::Bool(true))
                }
                View::Eq(..) | View::Atom(..) => {
                    // just map one level.
                    drop(view_t);
                    match self.m.view(&t) {
                        AstView::Const(_) | AstView::Index(..) => t,
                        AstView::App{f, args} => {
                            let mut args: SVec<AST> = args.iter().cloned().collect();
                            let f = self.simplify_rec(*f);
                            for u in args.iter_mut() { *u = self.simplify_rec(*u) }
                            self.m.mk_app(f, &args[..], self.m.ty(&t))
                        }
                    }
                },
                View::Not(u) => {
                    let u = self.simplify_rec(u);
                    match self.m.view_as_formula(u) {
                        View::Bool(b) => self.m.mk_formula(View::Bool(!b)),
                        View::Not(v) => v,
                        _ => self.m.mk_formula(View::Not(u))
                    }
                }
                View::And(args0) => {
                    let mut args = SVec::new();
                    flatten_conn(self.m, Conn::And, &mut args, args0);
                    for u in args.iter_mut() { *u = self.simplify_rec(*u) }
                    if args.iter().any(|u| self.m.is_false(*u)) {
                        self.m.mk_formula(View::Bool(false)) // shortcut
                    } else {
                        self.m.mk_formula(View::And(&args))
                    }
                }
                View::Or(args0) => {
                    let mut args = SVec::new();
                    flatten_conn(self.m, Conn::Or, &mut args, args0);
                    for u in args.iter_mut() { *u = self.simplify_rec(*u) }
                    if args.iter().any(|u| self.m.is_true(*u)) {
                        self.m.mk_formula(View::Bool(true)) // shortcut
                    } else {
                        self.m.mk_formula(View::Or(&args))
                    }
                },
                View::Imply(args0) => {
                    // transform into `or`
                    let n = args0.len();
                    assert!(n >= 2);
                    let mut disj: Vec<AST> = args0.iter().cloned().collect();
                    drop(view_t);

                    for u in disj[.. n-1].iter_mut() {
                        *u = self.m.mk_formula(View::Not(*u))
                    }

                    let u = self.m.mk_formula(View::Or(&disj));
                    // and simplify the disjunction as is, including flattening
                    self.simplify_rec(u)
                },
                View::Ite(a,b,c) => {
                    let a = self.simplify_rec(a);
                    let b = self.simplify_rec(b);
                    let c = self.simplify_rec(c);
                    match self.m.view_as_formula(a) {
                        View::Bool(true) => b,
                        View::Bool(false) => c,
                        _ if b == c => b,
                        _ => self.m.mk_formula(View::Ite(a,b,c))
                    }
                },
            };
            if t != u { trace!("(simp_rec :from {} :to {})", pp_ast(self.m,&t), pp_ast(self.m,&u)); }
            self.map.insert(t, u);
            u
        }
    }
}

/// Boolean-typed term?
fn has_ty_bool<C:Ctx>(c: &C, t: &AST) -> bool {
    match c.ty(t) {
        Some(b) =>
            match c.view_as_formula(b) {
                View::TyBool => true,
                _ => false
            },
        None => false
    }
}

impl<C> Tseitin<C> where C: Ctx {
    /// Create a new Tseitin transformation
    pub fn new() -> Self {
        Self {
            tmp: Vec::new(),
            tmp2: Vec::new(),
            tmp_ast: vec!(),
            lits: FxHashSet::default(),
            iter: ast::iter_dag::new(),
            simp_map: ast::HashMap::new(),
            cs: TheoryClauseSet::new(),
        }
    }

    /// Clear internal caches.
    ///
    /// This means that formulas already defined in previous calls to
    /// `self.clauses(t)` will be re-defined if we meet them again.
    pub fn clear(&mut self) {
        self.iter.clear();
    }

    /// Simplify boolean expressions.
    pub fn simplify(&mut self, m: &mut C, t: AST) -> AST {
        let mut simp = SimpStruct{m, map: &mut self.simp_map};
        let u = simp.simplify_rec(t);
        if t != u {
            debug!("tseitin.simplify\nfrom {}\nto {}", pp_ast(m,&t), pp_ast(m,&u));
        }
        u
    }

    /// `tseitin.clauses(t)` turns the boolean term `t` into a set of clauses and literals.
    ///
    /// The clauses define boolean connectives occurring inside `t`.
    /// ## params
    /// - `t` is the formula to normalize
    pub fn clauses<LM>(
        &mut self, m: &mut C, lit_map: &mut LM, t: AST
    ) -> (impl Iterator<Item=TheoryClauseRef<C>>, impl Iterator<Item=&TheoryLit<C>>)
        where LM: LitMap<C::B>
    {
        // first, simplify to flatten connectives and remove `distinct`
        let t = self.simplify(m, t);

        let Tseitin { tmp_ast: args, cs, lits: all_lits, tmp, tmp2, ..} = self;
        cs.clear();

        // traverse `t` as a DAG
        self.iter.iter_mut(m, &t, |m, u| {
            // `u` is a subterm that has never been processed.
            let view_u = m.view_as_formula(*u);
            args.clear();
            tmp.clear();
            match view_u {
                View::TyBool => (),
                View::Atom(_) => {
                    drop(view_u);
                    if has_ty_bool(m, u) {
                        // map to a literal
                        let lit = TheoryLit::new_t(*u, true);
                        all_lits.insert(lit);
                    }
                }, 
                View::Not(a) => {
                    let lit = TheoryLit::new_t(a, true);
                    all_lits.insert(lit);
                },
                View::Eq(..) => {
                    let lit = TheoryLit::new_t(*u, true);
                    all_lits.insert(lit);
                },
                View::Bool(true) => {
                    cs.push(&[TheoryLit::new_b(*u, true)]) // clause [true]
                },
                View::Bool(false) => {
                    // TODO: is this needed? `u` maps to `not true` anyway?
                    cs.push(&[TheoryLit::new_b(*u, false)]) // clause [¬false]
                },
                View::Ite(a,_,_) => {
                    let mut lmb = LitMapB{lit_map, m};
                    let lit = lmb.term_to_lit(&a);
                    all_lits.insert(lit);
                },
                View::And(args2) => {
                    args.extend_from_slice(args2);
                    drop(view_u);
                    let mut lmb = LitMapB{lit_map, m};
                    for t in args.iter() {
                        tmp.push(lmb.term_to_lit(t));
                    }
                    let lit_and = lmb.term_to_lit(u); // pure bool

                    // `lit_and => args[i]`
                    for &sub in tmp.iter() {
                        cs.push(&[!lit_and, sub]);
                    }
                    // `args[i] ==> lit_and`
                    {
                        tmp2.clear();
                        for &sub in tmp.iter() {
                            tmp2.push(!sub)
                        }
                        tmp2.push(lit_and);
                        cs.push(&tmp2);
                    }
                },
                View::Or(args2) => {
                    args.extend_from_slice(args2);
                    drop(view_u);
                    let mut lmb = LitMapB{lit_map, m};
                    for t in args.iter() {
                        tmp.push(lmb.term_to_lit(t));
                    }
                    let lit_or = lmb.term_to_lit(u); // pure bool

                    // `args[i] => lit_or`
                    for &sub in tmp.iter() {
                        cs.push(&[!sub, lit_or]);
                    }
                    // `lit_or => ∨_i args[i]`
                    {
                        tmp2.clear();
                        tmp2.extend_from_slice(&tmp);
                        tmp2.push(!lit_or);
                        cs.push(&tmp2);
                    }
                },
                View::Imply(args2) => {
                    // same as `or`, but all literals but the last are negated
                    args.extend_from_slice(args2);
                    drop(view_u);
                    let mut lmb = LitMapB{lit_map, m};
                    {
                        let t_last = args[args.len()-1];
                        tmp.push(lmb.term_to_lit(&t_last));
                    }
                    for t in args[.. args.len()-1].iter() {
                        // negation here, LHS of implication
                        tmp.push(! lmb.term_to_lit(t));
                    }
                    let lit_or = lmb.term_to_lit(u); // pure bool
                    debug_assert!(lit_or.is_pure_bool());

                    // `args[i] => lit_or`
                    for &sub in tmp.iter() {
                        cs.push(&[!sub, lit_or]);
                    }
                    // `lit_or => ∨_i args[i]`
                    {
                        tmp2.clear();
                        tmp2.extend_from_slice(&tmp);
                        tmp2.push(!lit_or);
                        cs.push(&tmp2);
                    }
                },
                View::Distinct(_) => panic!("distinct post-simplification"),
            }
        });

        {
            // unit clause asserting that `t` is true
            let mut lmb = LitMapB{lit_map, m};
            let top_lit = lmb.term_to_lit(&t);
            self.cs.push(&[top_lit]);
        }

        (self.cs.iter(), self.lits.iter())
    }

}

impl<C> gc::HasInternalMemory for Tseitin<C> where C: Ctx {
    fn reclaim_unused_memory(&mut self) {
        self.tmp.shrink_to_fit();
        self.tmp2.shrink_to_fit();
        self.tmp_ast.shrink_to_fit();
        self.cs.reclaim_unused_memory();
        self.iter.reclaim_unused_memory();
        self.simp_map.reclaim_unused_memory();
    }
}

