use joker;
use joker::track::*;
use joker::token::{Token, TokenData};
use joker::word::{Atom, Name, Reserved};
use joker::lexer::Lexer;
use joker::context::Mode;
use easter::prog::Script;
use easter::stmt::{Stmt, StmtListItem, ForHead, ForInHead, ForOfHead, Case, Catch};
use easter::expr::Expr;
use easter::decl::{Decl, Dtor, DtorExt};
use easter::patt::{Patt, CompoundPatt};
use easter::fun::{Fun, Params};
use easter::obj::{PropKey, PropVal, Prop, DotKey};
use easter::id::{Id, IdExt};
use easter::punc::{Unop, UnopTag, ToOp, Op};
use easter::cover::{IntoAssignTarget, IntoAssignPatt};

use std::cell::Cell;
use std::rc::Rc;
use std::mem::replace;
use std::convert::From;
use std::str::Chars;
use context;
use context::{LabelType, WithContext};
use tokens::{First, Follows, HasLabelType};
use atom::AtomExt;
use track::Newline;
use result::Result;
use error::Error;
use track::Tracking;
use state::State;
use expr::{Deref, Suffix, Arguments, Prefix, Postfix};
use stack::{Stack, Infix};

pub struct Parser<I> {
    pub lexer: Lexer<I>,
    pub shared_cx: Rc<Cell<joker::context::Context>>,
    pub parser_cx: context::Context
}

impl<'a> From<&'a str> for Parser<Chars<'a>> {
    fn from(s: &'a str) -> Parser<Chars<'a>> {
        Parser::from(s.chars())
    }
}

impl<I: Iterator<Item=char>> From<I> for Parser<I> {
    fn from(i: I) -> Parser<I> {
        let cx = Rc::new(Cell::new(joker::context::Context::new(Mode::Sloppy)));
        let lexer = Lexer::new(i, cx.clone());
        Parser::new(lexer, cx.clone())
    }
}

impl<I: Iterator<Item=char>> Parser<I> {
    pub fn new(lexer: Lexer<I>, cx: Rc<Cell<joker::context::Context>>) -> Parser<I> {
        Parser { lexer: lexer, shared_cx: cx, parser_cx: context::Context::new() }
    }

    pub fn script(&mut self) -> Result<Script> {
        let items = try!(self.statement_list());
        Ok(Script { location: self.vec_span(&items), body: items })
    }

    fn statement_list(&mut self) -> Result<Vec<StmtListItem>> {
        let mut items = Vec::new();
        while !try!(self.peek()).follow_statement_list() {
            //println!("statement at: {:?}", try!(self.peek()).location().unwrap().start);
            match try!(self.declaration_opt()) {
                Some(decl) => { items.push(StmtListItem::Decl(decl)); }
                None       => { items.push(StmtListItem::Stmt(try!(self.statement()))); }
            }
        }
        Ok(items)
    }

/*
    pub fn declaration(&mut self) -> Result<Decl> {
        match try!(self.declaration_opt()) {
            Some(decl) => Ok(decl),
            None       => Err(Error::UnexpectedToken(try!(self.read())))
        }
    }
*/

    fn declaration_opt(&mut self) -> Result<Option<Decl>> {
        match try!(self.peek()).value {
            TokenData::Reserved(Reserved::Function) => Ok(Some(try!(self.function_declaration()))),
            _                                       => Ok(None)
        }
    }

    fn function_declaration(&mut self) -> Result<Decl> {
        self.span(&mut |this| {
            Ok(Decl::Fun(try!(this.function())))
        })
    }

    fn formal_parameters(&mut self) -> Result<Params> {
        self.span(&mut |this| {
            try!(this.expect(TokenData::LParen));
            let list = try!(this.pattern_list());
            try!(this.expect(TokenData::RParen));
            Ok(Params { location: None, list: list })
        })
    }

    fn pattern_list(&mut self) -> Result<Vec<Patt<Id>>> {
        let mut patts = Vec::new();
        if try!(self.peek()).value == TokenData::RParen {
            return Ok(patts);
        }
        patts.push(try!(self.pattern()));
        while try!(self.matches(TokenData::Comma)) {
            patts.push(try!(self.pattern()));
        }
        Ok(patts)
    }

    fn pattern(&mut self) -> Result<Patt<Id>> {
        match try!(self.peek()).value {
            TokenData::Identifier(_) => {
                let id = try!(self.binding_id());
                Ok(Patt::Simple(id))
            }
            _ => {
                let patt = try!(self.binding_pattern());
                Ok(Patt::Compound(patt))
            }
        }
    }

    fn binding_pattern(&mut self) -> Result<CompoundPatt<Id>> {
        if !try!(self.peek()).first_binding() {
            return Err(Error::UnexpectedToken(try!(self.read())));
        }
        Err(Error::UnsupportedFeature("destructuring"))
    }

    fn function(&mut self) -> Result<Fun> {
        let outer_cx = replace(&mut self.parser_cx, context::Context::new_function());
        let result = self.span(&mut |this| {
            this.reread(TokenData::Reserved(Reserved::Function));
            let id = try!(this.id_opt());
            let params = try!(this.formal_parameters());
            try!(this.expect(TokenData::LBrace));
            let body = try!(this.statement_list());
            try!(this.expect(TokenData::RBrace));
            Ok(Fun { location: None, id: id, params: params, body: body })
        });
        replace(&mut self.parser_cx, outer_cx);
        result
    }

    fn statement(&mut self) -> Result<Stmt> {
        match try!(self.peek()).value {
            TokenData::LBrace                       => self.block_statement(),
            TokenData::Reserved(Reserved::Var)      => self.var_statement(),
            TokenData::Semi                         => self.empty_statement(),
            TokenData::Reserved(Reserved::If)       => self.if_statement(),
            TokenData::Reserved(Reserved::Continue) => self.continue_statement(),
            TokenData::Reserved(Reserved::Break)    => self.break_statement(),
            TokenData::Reserved(Reserved::Return)   => self.return_statement(),
            TokenData::Reserved(Reserved::With)     => self.with_statement(),
            TokenData::Reserved(Reserved::Switch)   => self.switch_statement(),
            TokenData::Reserved(Reserved::Throw)    => self.throw_statement(),
            TokenData::Reserved(Reserved::Try)      => self.try_statement(),
            TokenData::Reserved(Reserved::While)    => self.while_statement(),
            TokenData::Reserved(Reserved::Do)       => self.do_statement(),
            TokenData::Reserved(Reserved::For)      => self.for_statement(),
            TokenData::Reserved(Reserved::Debugger) => self.debugger_statement(),
            TokenData::Identifier(_)                => {
                let id = self.id().unwrap();
                self.id_statement(id)
            }
            _                                       => self.expression_statement()
        }
    }

    fn id_statement(&mut self, id: Id) -> Result<Stmt> {
        match try!(self.peek_op()).value {
            TokenData::Colon => self.labelled_statement(id),
            _                => {
                let span = self.start();
                let expr = try!(self.id_expression(id));
                Ok(try!(span.end_with_auto_semi(self, Newline::Required, |semi| Stmt::Expr(None, expr, semi))))
            }
        }
    }

    fn labelled_statement(&mut self, id: Id) -> Result<Stmt> {
        self.reread(TokenData::Colon);

        let mut labels = vec![id]; // vector of consecutive labels
        let mut expr_id = None;    // id that starts the statement following the labels, if any

        while let TokenData::Identifier(_) = try!(self.peek()).value {
            let id = self.id().unwrap();
            if !try!(self.matches_op(TokenData::Colon)) {
                expr_id = Some(id);
                break;
            }
            labels.push(id);
        }

        match expr_id {
            Some(id) => {
                self.with_labels(labels, LabelType::Statement, |this| this.id_statement(id))
            }
            None     => {
                let label_type = try!(self.peek()).label_type();
                self.with_labels(labels, label_type, |this| this.statement())
            }
        }
    }

    fn expression_statement(&mut self) -> Result<Stmt> {
        let span = self.start();
        let expr = try!(self.allow_in(true, |this| this.expression()));
        Ok(try!(span.end_with_auto_semi(self, Newline::Required, |semi| Stmt::Expr(None, expr, semi))))
    }

    fn block_statement(&mut self) -> Result<Stmt> {
        self.span(&mut |this| {
            this.reread(TokenData::LBrace);
            let items = try!(this.statement_list());
            try!(this.expect(TokenData::RBrace));
            Ok(Stmt::Block(None, items))
        })
    }

    fn var_statement(&mut self) -> Result<Stmt> {
        let span = self.start();
        self.reread(TokenData::Reserved(Reserved::Var));
        let dtors = try!(self.declarator_list());
        Ok(try!(span.end_with_auto_semi(self, Newline::Required, |semi| Stmt::Var(None, dtors, semi))))
    }

    fn declarator_list(&mut self) -> Result<Vec<Dtor>> {
        let mut items = Vec::new();
        items.push(try!(self.declarator()));
        while try!(self.matches(TokenData::Comma)) {
            items.push(try!(self.declarator()));
        }
        Ok(items)
    }

    fn binding_id(&mut self) -> Result<Id> {
        let id = try!(self.id());
        if self.shared_cx.get().mode.is_strict() && id.name.is_illegal_strict_binding() {
            return Err(Error::IllegalStrictBinding(id));
        }
        Ok(id)
    }

    fn id(&mut self) -> Result<Id> {
        let Token { location, newline, value: data } = try!(self.read());
        match data {
            TokenData::Identifier(name) => {
                if name.is_reserved(self.shared_cx.get().mode) {
                    return Err(Error::ContextualKeyword(Id {
                        location: Some(location),
                        name: name
                    }));
                }
                Ok(Id { location: Some(location), name: name })
            }
            _ => Err(Error::UnexpectedToken(Token {
                location: location,
                newline: newline,
                value: data
            }))
        }
    }

    fn id_opt(&mut self) -> Result<Option<Id>> {
        let next = try!(self.read());
        match next.value {
            TokenData::Identifier(name) => {
                Ok(Some(Id { location: Some(next.location), name: name }))
            }
            _                           => { self.lexer.unread_token(next); Ok(None) }
        }
    }

    fn declarator(&mut self) -> Result<Dtor> {
        self.span(&mut |this| {
            match try!(this.peek()).value {
                TokenData::Identifier(_) => {
                    let id = try!(this.binding_id());
                    let init = if try!(this.matches(TokenData::Assign)) {
                        Some(try!(this.assignment_expression()))
                    } else {
                        None
                    };
                    Ok(Dtor::Simple(None, id, init))
                }
                _ => {
                    let lhs = try!(this.binding_pattern());
                    try!(this.expect(TokenData::Assign));
                    let rhs = try!(this.assignment_expression());
                    Ok(Dtor::Compound(None, lhs, rhs))
                }
            }
        })
    }

    fn empty_statement(&mut self) -> Result<Stmt> {
        self.span(&mut |this| {
            try!(this.expect(TokenData::Semi));
            Ok(Stmt::Empty(None))
        })
    }

    fn if_statement(&mut self) -> Result<Stmt> {
        self.span(&mut |this| {
            try!(this.expect(TokenData::Reserved(Reserved::If)));
            let test = try!(this.paren_expression());
            let cons = Box::new(try!(this.statement()));
            let alt = if try!(this.peek()).value == TokenData::Reserved(Reserved::Else) {
                this.reread(TokenData::Reserved(Reserved::Else));
                Some(Box::new(try!(this.statement())))
            } else {
                None
            };
            Ok(Stmt::If(None, test, cons, alt))
        })
    }

    fn iteration_body(&mut self) -> Result<Stmt> {
        let iteration = replace(&mut self.parser_cx.iteration, true);
        let result = self.statement();
        replace(&mut self.parser_cx.iteration, iteration);
        result
    }

    fn do_statement(&mut self) -> Result<Stmt> {
        let span = self.start();
        self.reread(TokenData::Reserved(Reserved::Do));
        let body = Box::new(try!(self.iteration_body()));
        try!(self.expect(TokenData::Reserved(Reserved::While)));
        let test = try!(self.paren_expression());
        Ok(try!(span.end_with_auto_semi(self, Newline::Optional, |semi| {
            Stmt::DoWhile(None, body, test, semi)
        })))
    }

    fn while_statement(&mut self) -> Result<Stmt> {
        self.span(&mut |this| {
            this.reread(TokenData::Reserved(Reserved::While));
            let test = try!(this.paren_expression());
            let body = Box::new(try!(this.iteration_body()));
            Ok(Stmt::While(None, test, body))
        })
    }

    fn for_statement(&mut self) -> Result<Stmt> {
        self.span(&mut |this| {
            this.reread(TokenData::Reserved(Reserved::For));
            try!(this.expect(TokenData::LParen));
            match try!(this.peek()).value {
                TokenData::Reserved(Reserved::Var)           => this.for_var(),
                TokenData::Identifier(Name::Atom(Atom::Let)) => this.for_let(),
                TokenData::Reserved(Reserved::Const)         => { return Err(Error::UnsupportedFeature("const")); }
                TokenData::Semi                              => {
                    this.reread(TokenData::Semi);
                    this.more_for(None)
                }
                _                                            => this.for_expr()
            }
        })
    }

    // 'for' '(' 'var' .
    fn for_var(&mut self) -> Result<Stmt> {
        let var_token = self.reread(TokenData::Reserved(Reserved::Var));
        let var_location = Some(var_token.location);
        let lhs = try!(self.pattern());
        match try!(self.peek()).value {
            // 'for' '(' 'var' id   '=' .
            // 'for' '(' 'var' patt '=' . ==> C-style
            TokenData::Assign => {
                self.reread(TokenData::Assign);
                match lhs {
                    Patt::Simple(id) => {
                        let rhs = try!(self.allow_in(false, |this| this.assignment_expression()));
                        match try!(self.peek()).value {
                            // 'for' '(' 'var' id '=' expr ','  . ==> C-style
                            // 'for' '(' 'var' id '=' expr ';'  . ==> C-style
                            TokenData::Comma
                          | TokenData::Semi => {
                                let head = Some(try!(self.more_for_head(&var_location, Dtor::from_simple_init(id, rhs), ForHead::Var)));
                                self.more_for(head)
                            }
                            // 'for' '(' 'var' id '=' expr 'in' . ==> legacy enumeration
                            TokenData::Reserved(Reserved::In) => {
                                self.reread(TokenData::Reserved(Reserved::In));
                                let head = Box::new(ForInHead::VarInit(span(&var_location, &rhs), id, rhs));
                                self.more_for_in(head)
                            }
                            _ => Err(Error::UnexpectedToken(try!(self.read())))
                        }
                    }
                    // 'for' '(' 'var' patt '=' . ==> C-style
                    Patt::Compound(patt) => {
                        let rhs = try!(self.allow_in(false, |this| this.assignment_expression()));
                        let head = Some(try!(self.more_for_head(&var_location, Dtor::from_compound_init(patt, rhs), ForHead::Var)));
                        self.more_for(head)
                    }
                }
            }
            TokenData::Comma
          | TokenData::Semi => {
                // 'for' '(' 'var' id   ',' . ==> C-style
                // 'for' '(' 'var' id   ';' . ==> C-style
                // 'for' '(' 'var' patt ',' . ==> syntax error
                // 'for' '(' 'var' patt ';' . ==> syntax error
                let dtor = match Dtor::from_init_opt(lhs, None) {
                    Ok(dtor) => dtor,
                    Err(_) => { return Err(Error::UnexpectedToken(try!(self.read()))); }
                };
                let head = Some(try!(self.more_for_head(&var_location, dtor, ForHead::Var)));
                self.more_for(head)
            }
            // 'for' '(' 'var' id   'in' . ==> enumeration
            // 'for' '(' 'var' patt 'in' . ==> enumeration
            TokenData::Reserved(Reserved::In) => {
                self.reread(TokenData::Reserved(Reserved::In));
                let head = Box::new(ForInHead::Var(span(&var_location, &lhs), lhs));
                self.more_for_in(head)
            }
            // 'for' '(' 'var' id   'of' . ==> enumeration
            // 'for' '(' 'var' patt 'of' . ==> enumeration
            TokenData::Identifier(Name::Atom(Atom::Of)) => {
                self.reread(TokenData::Identifier(Name::Atom(Atom::Of)));
                let head = Box::new(ForOfHead::Var(span(&var_location, &lhs), lhs));
                self.more_for_of(head)
            }
            _ => Err(Error::UnexpectedToken(try!(self.read())))
        }
    }

    // 'for' '(' 'let' .
    fn for_let(&mut self) -> Result<Stmt> {
        let let_token = self.reread(TokenData::Identifier(Name::Atom(Atom::Let)));
        let let_location = Some(let_token.location);
        // 'for' '(' 'let' . !{id, patt} ==> error
        let lhs = try!(self.pattern());
        match try!(self.peek()).value {
            // 'for' '(' 'let' id   '=' . ==> C-style
            // 'for' '(' 'let' patt '=' . ==> C-style
            TokenData::Assign => {
                self.reread(TokenData::Assign);
                let rhs = try!(self.allow_in(false, |this| this.assignment_expression()));
                let head = Some(try!(self.more_for_head(&let_location, Dtor::from_init(lhs, rhs), ForHead::Let)));
                self.more_for(head)
            }
            TokenData::Comma
          | TokenData::Semi => {
                // 'for' '(' 'let' id   ',' . ==> C-style
                // 'for' '(' 'let' id   ';' . ==> C-style
                // 'for' '(' 'let' patt ',' . ==> error
                // 'for' '(' 'let' patt ';' . ==> error
                let dtor = match Dtor::from_init_opt(lhs, None) {
                    Ok(dtor) => dtor,
                    Err(_) => { return Err(Error::UnexpectedToken(try!(self.read()))); }
                };
                let head = Some(try!(self.more_for_head(&let_location, dtor, ForHead::Let)));
                self.more_for(head)
            }
            // 'for' '(' 'let' id   'in' . ==> enumeration
            // 'for' '(' 'let' patt 'in' . ==> enumeration
            TokenData::Reserved(Reserved::In) => {
                self.reread(TokenData::Reserved(Reserved::In));
                let head = Box::new(ForInHead::Let(span(&let_location, &lhs), lhs));
                self.more_for_in(head)
            }
            // 'for' '(' 'let' id   'of' . ==> enumeration
            // 'for' '(' 'let' patt 'of' . ==> enumeration
            TokenData::Identifier(Name::Atom(Atom::Of)) => {
                self.reread(TokenData::Identifier(Name::Atom(Atom::Of)));
                let head = Box::new(ForOfHead::Let(span(&let_location, &lhs), lhs));
                self.more_for_of(head)
            }
            _ => Err(Error::UnexpectedToken(try!(self.read())))
        }
    }

    fn for_expr(&mut self) -> Result<Stmt> {
        let lhs = try!(self.allow_in(false, |this| this.expression()));
        match try!(self.peek()).value {
            TokenData::Semi => {
                let semi_location = Some(self.reread(TokenData::Semi).location);
                let head = Some(Box::new(ForHead::Expr(span(&lhs, &semi_location), lhs)));
                self.more_for(head)
            }
            TokenData::Reserved(Reserved::In) => {
                self.reread(TokenData::Reserved(Reserved::In));
                let lhs_location = *lhs.tracking_ref();
                let lhs = match lhs.into_assign_patt() {
                    Ok(lhs) => lhs,
                    Err(cover_err) => { return Err(Error::InvalidLHS(lhs_location, cover_err)); }
                };
                let head = Box::new(ForInHead::Patt(lhs));
                self.more_for_in(head)
            }
            TokenData::Identifier(Name::Atom(Atom::Of)) => {
                self.reread(TokenData::Identifier(Name::Atom(Atom::Of)));
                let lhs_location = *lhs.tracking_ref();
                let lhs = match lhs.into_assign_patt() {
                    Ok(lhs) => lhs,
                    Err(cover_err) => { return Err(Error::InvalidLHS(lhs_location, cover_err)); }
                };
                let head = Box::new(ForOfHead::Patt(lhs));
                self.more_for_of(head)
            }
            _ => Err(Error::UnexpectedToken(try!(self.read())))
        }
    }

    // 'for' '(' dtor .
    fn more_for_head<F>(&mut self, start: &Option<Span>, dtor: Dtor, op: F) -> Result<Box<ForHead>>
      where F: FnOnce(Option<Span>, Vec<Dtor>) -> ForHead
    {
        let dtors = try!(self.allow_in(false, |this| {
            let mut dtors = vec![dtor];
            try!(this.more_dtors(&mut dtors));
            Ok(dtors)
        }));
        let semi_location = Some(try!(self.expect(TokenData::Semi)).location);
        Ok(Box::new(op(span(start, &semi_location), dtors)))
    }

    // 'for' '(' head ';' .
    fn more_for(&mut self, head: Option<Box<ForHead>>) -> Result<Stmt> {
        let test = try!(self.expression_opt_semi());
        let update = if try!(self.matches(TokenData::RParen)) {
            None
        } else {
            let node = Some(try!(self.allow_in(true, |this| this.expression())));
            try!(self.expect(TokenData::RParen));
            node
        };
        let body = Box::new(try!(self.iteration_body()));
        Ok(Stmt::For(None, head, test, update, body))
    }

    // 'for' '(' head 'in' .
    fn more_for_in(&mut self, head: Box<ForInHead>) -> Result<Stmt> {
        let obj = try!(self.allow_in(true, |this| this.assignment_expression()));
        try!(self.expect(TokenData::RParen));
        let body = Box::new(try!(self.iteration_body()));
        Ok(Stmt::ForIn(None, head, obj, body))
    }

    // 'for' '(' head 'of' .
    fn more_for_of(&mut self, head: Box<ForOfHead>) -> Result<Stmt> {
        let obj = try!(self.allow_in(true, |this| this.assignment_expression()));
        try!(self.expect(TokenData::RParen));
        let body = Box::new(try!(self.iteration_body()));
        Ok(Stmt::ForOf(None, head, obj, body))
    }

    fn expression_opt_semi(&mut self) -> Result<Option<Expr>> {
        Ok(if try!(self.matches(TokenData::Semi)) {
            None
        } else {
            let expr = try!(self.allow_in(true, |this| this.expression()));
            try!(self.expect(TokenData::Semi));
            Some(expr)
        })
    }

    fn more_dtors(&mut self, dtors: &mut Vec<Dtor>) -> Result<()> {
        while try!(self.matches(TokenData::Comma)) {
            dtors.push(try!(self.declarator()));
        }
        Ok(())
    }

    fn switch_statement(&mut self) -> Result<Stmt> {
        self.span(&mut |this| {
            this.reread(TokenData::Reserved(Reserved::Switch));
            let disc = try!(this.paren_expression());
            let outer_switch = replace(&mut this.parser_cx.switch, true);
            let cases = this.switch_cases();
            replace(&mut this.parser_cx.switch, outer_switch);
            Ok(Stmt::Switch(None, disc, try!(cases)))
        })
    }

    fn switch_cases(&mut self) -> Result<Vec<Case>> {
        try!(self.expect(TokenData::LBrace));
        let mut cases = Vec::new();
        let mut found_default = false;
        loop {
            match try!(self.peek()).value {
                TokenData::Reserved(Reserved::Case) => { cases.push(try!(self.case())); }
                TokenData::Reserved(Reserved::Default) => {
                    if found_default {
                        let token = self.reread(TokenData::Reserved(Reserved::Default));
                        return Err(Error::DuplicateDefault(token));
                    }
                    found_default = true;
                    cases.push(try!(self.default()));
                }
                _ => { break; }
            }
        }
        try!(self.expect(TokenData::RBrace));
        Ok(cases)
    }

    fn case(&mut self) -> Result<Case> {
        self.span(&mut |this| {
            this.reread(TokenData::Reserved(Reserved::Case));
            let test = try!(this.allow_in(true, |this| this.expression()));
            try!(this.expect(TokenData::Colon));
            let body = try!(this.case_body());
            Ok(Case { location: None, test: Some(test), body: body })
        })
    }

    fn case_body(&mut self) -> Result<Vec<StmtListItem>> {
        let mut items = Vec::new();
        loop {
            match try!(self.peek()).value {
                TokenData::Reserved(Reserved::Case)
              | TokenData::Reserved(Reserved::Default)
              | TokenData::RBrace => { break; }
                _ => { }
            }
            match try!(self.declaration_opt()) {
                Some(decl) => { items.push(StmtListItem::Decl(decl)); }
                None       => { items.push(StmtListItem::Stmt(try!(self.statement()))); }
            }
        }
        Ok(items)
    }

    fn default(&mut self) -> Result<Case> {
        self.span(&mut |this| {
            this.reread(TokenData::Reserved(Reserved::Default));
            try!(this.expect(TokenData::Colon));
            let body = try!(this.case_body());
            Ok(Case { location: None, test: None, body: body })
        })
    }

    fn break_statement(&mut self) -> Result<Stmt> {
        let span = self.start();
        let break_token = self.reread(TokenData::Reserved(Reserved::Break));
        let arg = if try!(self.has_arg_same_line()) {
            let id = try!(self.id());
            if !self.parser_cx.labels.contains_key(&Rc::new(id.name.clone())) {
                return Err(Error::InvalidLabel(id));
            }
            Some(id)
        } else {
            if !self.parser_cx.iteration && !self.parser_cx.switch {
                return Err(Error::IllegalBreak(break_token));
            }
            None
        };
        span.end_with_auto_semi(self, Newline::Required, |semi| {
            Stmt::Break(None, arg, semi)
        })
    }

    fn continue_statement(&mut self) -> Result<Stmt> {
        let span = self.start();
        let continue_token = self.reread(TokenData::Reserved(Reserved::Continue));
        let arg = if try!(self.has_arg_same_line()) {
            let id = try!(self.id());
            match self.parser_cx.labels.get(&Rc::new(id.name.clone())) {
                None                        => { return Err(Error::InvalidLabel(id)); }
                Some(&LabelType::Statement) => { return Err(Error::InvalidLabelType(id)); }
                _                           => { }
            }
            Some(id)
        } else {
            if !self.parser_cx.iteration {
                return Err(Error::IllegalContinue(continue_token));
            }
            None
        };
        span.end_with_auto_semi(self, Newline::Required, |semi| {
            Stmt::Cont(None, arg, semi)
        })
    }

    fn return_statement(&mut self) -> Result<Stmt> {
        let span = self.start();
        self.reread(TokenData::Reserved(Reserved::Return));
        let arg = if try!(self.has_arg_same_line()) {
            Some(try!(self.allow_in(true, |this| this.expression())))
        } else {
            None
        };
        let result = try!(span.end_with_auto_semi(self, Newline::Required, |semi| {
            Stmt::Return(None, arg, semi)
        }));
        if !self.parser_cx.function {
            Err(Error::TopLevelReturn(result.tracking_ref().unwrap()))
        } else {
            Ok(result)
        }
    }

    fn with_statement(&mut self) -> Result<Stmt> {
        self.span(&mut |this| {
            let token = this.reread(TokenData::Reserved(Reserved::With));
            if this.shared_cx.get().mode.is_strict() {
                return Err(Error::StrictWith(token));
            }
            let obj = try!(this.paren_expression());
            let body = Box::new(try!(this.statement()));
            Ok(Stmt::With(None, obj, body))
        })
    }

    fn throw_statement(&mut self) -> Result<Stmt> {
        let span = self.start();
        let token = self.reread(TokenData::Reserved(Reserved::Throw));
        if !try!(self.has_arg_same_line()) {
            return Err(Error::ThrowArgument(token));
        }
        let arg = try!(self.allow_in(true, |this| this.expression()));
        span.end_with_auto_semi(self, Newline::Required, |semi| {
            Stmt::Throw(None, arg, semi)
        })
    }

    fn block(&mut self) -> Result<Vec<StmtListItem>> {
        try!(self.expect(TokenData::LBrace));
        let result = try!(self.statement_list());
        try!(self.expect(TokenData::RBrace));
        Ok(result)
    }

    fn try_statement(&mut self) -> Result<Stmt> {
        self.span(&mut |this| {
            this.reread(TokenData::Reserved(Reserved::Try));
            let body = try!(this.block());
            match try!(this.peek()).value {
                TokenData::Reserved(Reserved::Catch)
              | TokenData::Reserved(Reserved::Finally) => { }
                _ => {
                    return Err(Error::OrphanTry(try!(this.read())));
                }
            }
            let catch = try!(this.catch_opt()).map(Box::new);
            let finally = try!(this.finally_opt());
            Ok(Stmt::Try(None, body, catch, finally))
        })
    }

    fn catch_opt(&mut self) -> Result<Option<Catch>> {
        match try!(self.peek()).value {
            TokenData::Reserved(Reserved::Catch) => {
                self.span(&mut |this| {
                    this.reread(TokenData::Reserved(Reserved::Catch));
                    try!(this.expect(TokenData::LParen));
                    let param = try!(this.pattern());
                    try!(this.expect(TokenData::RParen));

                    let body = try!(this.block());
                    Ok(Catch { location: None, param: param, body: body })
                }).map(Some)
            }
            _ => Ok(None)
        }
    }

    fn finally_opt(&mut self) -> Result<Option<Vec<StmtListItem>>> {
        Ok(match try!(self.peek()).value {
            TokenData::Reserved(Reserved::Finally) => {
                self.reread(TokenData::Reserved(Reserved::Finally));
                Some(try!(self.block()))
            }
            _ => None
        })
    }

    fn debugger_statement(&mut self) -> Result<Stmt> {
        let span = self.start();
        self.reread(TokenData::Reserved(Reserved::Debugger));
        Ok(try!(span.end_with_auto_semi(self, Newline::Required, |semi| Stmt::Debugger(None, semi))))
    }

/*
    pub fn module(&mut self) -> Result<Module> {
        unimplemented!()
    }
*/

    fn paren_expression(&mut self) -> Result<Expr> {
        try!(self.expect(TokenData::LParen));
        let result = try!(self.allow_in(true, |this| this.expression()));
        try!(self.expect(TokenData::RParen));
        Ok(result)
    }

    // PrimaryExpression ::=
    //   "this"
    //   IdentifierReference
    //   Literal
    //   ArrayLiteral
    //   ObjectLiteral
    //   FunctionExpression
    //   ClassExpression
    //   GeneratorExpression
    //   RegularExpressionLiteral
    //   "(" Expression ")"
    fn primary_expression(&mut self) -> Result<Expr> {
        let token = try!(self.read());
        let location = Some(token.location);
        Ok(match token.value {
            TokenData::Identifier(name)          => Expr::Id(Id::new(name, location)),
            TokenData::Reserved(Reserved::Null)  => Expr::Null(location),
            TokenData::Reserved(Reserved::This)  => Expr::This(location),
            TokenData::Reserved(Reserved::True)  => Expr::True(location),
            TokenData::Reserved(Reserved::False) => Expr::False(location),
            TokenData::Number(literal)           => Expr::Number(location, literal),
            TokenData::String(literal)           => Expr::String(location, literal),
            TokenData::RegExp(literal)           => Expr::RegExp(location, literal),
            TokenData::LBrack                    => { return self.array_literal(token); }
            TokenData::LBrace                    => { return self.object_literal(token); }
            TokenData::Reserved(Reserved::Function) => {
                self.lexer.unread_token(token);
                let fun = try!(self.function());
                return Ok(Expr::Fun(fun));
            }
            TokenData::LParen => {
                self.lexer.unread_token(token);
                return self.paren_expression();
            }
            // ES6: more cases
            _ => { return Err(Error::UnexpectedToken(token)); }
        })
    }

    fn array_literal(&mut self, start: Token) -> Result<Expr> {
        let mut elts = Vec::new();
        let start_location = Some(start.location);
        if let Some(end) = try!(self.matches_token(TokenData::RBrack)) {
            return Ok(Expr::Arr(span(&start_location, &Some(end.location)), elts));
        }
        loop {
            let elt = try!(self.array_element());
            elts.push(elt);
            if !try!(self.matches(TokenData::Comma)) {
                break;
            }
            // Optional final comma does not count as an element.
            if try!(self.peek()).value == TokenData::RBrack {
                break;
            }
        }
        let end_location = Some(try!(self.expect(TokenData::RBrack)).location);
        Ok(Expr::Arr(span(&start_location, &end_location), elts))
    }

    fn array_element(&mut self) -> Result<Option<Expr>> {
        if { let t = try!(self.peek()); t.value == TokenData::Comma || t.value == TokenData::RBrack } {
            return Ok(None);
        }
        // ES6: ellipsis
        self.allow_in(true, |this| this.assignment_expression().map(Some))
    }

    fn object_literal(&mut self, start: Token) -> Result<Expr> {
        let mut props = Vec::new();
        let start_location = Some(start.location);
        if let Some(end) = try!(self.matches_token(TokenData::RBrace)) {
            return Ok(Expr::Obj(span(&start_location, &Some(end.location)), props));
        }
        loop {
            let prop = try!(self.object_property());
            props.push(prop);
            if !try!(self.matches(TokenData::Comma)) {
                break;
            }
            if try!(self.peek()).value == TokenData::RBrack {
                break;
            }
        }
        let end_location = Some(try!(self.expect(TokenData::RBrace)).location);
        Ok(Expr::Obj(span(&start_location, &end_location), props))
    }

    fn more_prop_init(&mut self, key: PropKey) -> Result<Prop> {
        self.reread(TokenData::Colon);
        let val = try!(self.allow_in(true, |this| this.assignment_expression()));
        let key_location = *key.tracking_ref();
        let val_location = *val.tracking_ref();
        Ok(Prop {
            location: span(&key_location, &val_location),
            key: key,
            val: PropVal::Init(val)
        })
    }

    fn property_key_opt(&mut self) -> Result<Option<PropKey>> {
        let token = try!(self.read());
        let location = Some(token.location);
        Ok(Some(match token.value {
            TokenData::Identifier(name) => PropKey::Id(location, name.into_string()),
            TokenData::Reserved(word) => PropKey::Id(location, word.into_string()),
            TokenData::String(s) => PropKey::String(location, s),
            TokenData::Number(n) => PropKey::Number(location, n),
            _ => {
                self.lexer.unread_token(token);
                return Ok(None);
            }
        }))
    }

    fn property_key(&mut self) -> Result<PropKey> {
        match try!(self.property_key_opt()) {
            Some(key) => Ok(key),
            None => Err(Error::UnexpectedToken(try!(self.read())))
        }
    }

    fn object_property(&mut self) -> Result<Prop> {
        let first = try!(self.read());
        match first.value {
            TokenData::Identifier(Name::Atom(Atom::Get)) => {
                if let Some(key) = try!(self.property_key_opt()) {
                    let paren_location = Some(try!(self.expect(TokenData::LParen)).location);
                    try!(self.expect(TokenData::RParen));
                    try!(self.expect(TokenData::LBrace));
                    let outer_cx = replace(&mut self.parser_cx, context::Context::new_function());
                    let body = self.statement_list();
                    replace(&mut self.parser_cx, outer_cx);
                    let body = try!(body);
                    let end_location = Some(try!(self.expect(TokenData::RBrace)).location);
                    let val_location = span(&paren_location, &end_location);
                    let prop_location = span(&key, &end_location);
                    return Ok(Prop {
                        location: prop_location,
                        key: key,
                        val: PropVal::Get(val_location, body)
                    });
                }
                match try!(self.peek()).value {
                    // ES6: TokenData::LParen => unimplemented!(),
                    TokenData::Colon => {
                        let key_location = Some(first.location);
                        self.more_prop_init(PropKey::Id(key_location, "get".to_string()))
                    }
                    // ES6: treat as elided optional initializer
                    _ => { return Err(Error::UnexpectedToken(try!(self.read()))); }
                }
            }
            TokenData::Identifier(Name::Atom(Atom::Set)) => {
                if let Some(key) = try!(self.property_key_opt()) {
                    let paren_location = Some(try!(self.expect(TokenData::LParen)).location);
                    let param = try!(self.pattern());
                    try!(self.expect(TokenData::RParen));
                    try!(self.expect(TokenData::LBrace));
                    let outer_cx = replace(&mut self.parser_cx, context::Context::new_function());
                    let body = self.statement_list();
                    replace(&mut self.parser_cx, outer_cx);
                    let body = try!(body);
                    let end_location = Some(try!(self.expect(TokenData::RBrace)).location);
                    let val_location = span(&paren_location, &end_location);
                    let prop_location = span(&key, &end_location);
                    return Ok(Prop {
                        location: prop_location,
                        key: key,
                        val: PropVal::Set(val_location, param, body)
                    });
                }
                match try!(self.peek()).value {
                    // ES6: TokenData::LParen => unimplemented!(),
                    TokenData::Colon => {
                        let key_location = Some(first.location);
                        self.more_prop_init(PropKey::Id(key_location, "set".to_string()))
                    }
                    // ES6: treat as elided optional initializer
                    _ => { return Err(Error::UnexpectedToken(try!(self.read()))); }
                }
            }
            // ES6: TokenData::Star
            _ => {
                self.lexer.unread_token(first);
                let key = try!(self.property_key());
                match try!(self.peek()).value {
                    TokenData::Colon => self.more_prop_init(key),
                    // ES6: TokenData::LParen =>
                    // ES6: treat as elided optional initializer
                    _ => { return Err(Error::UnexpectedToken(try!(self.read()))); }
                }
            }
        }
    }

    // MemberBaseExpression ::=
    //   PrimaryExpression
    //   "new" "." "target"
    fn member_base_expression(&mut self) -> Result<Expr> {
        if let Some(new) = try!(self.matches_token(TokenData::Reserved(Reserved::New))) {
            try!(self.expect(TokenData::Dot));
            let target_location = Some(try!(self.expect(TokenData::Identifier(Name::Atom(Atom::Target)))).location);
            return Ok(Expr::NewTarget(span(&Some(new.location), &target_location)));
        }
        self.primary_expression()
    }

    // "new"+n . (MemberBaseExpression | "super" Deref) Deref* Arguments<n Suffix*
    fn new_expression(&mut self, news: Vec<Token>) -> Result<Expr> {
        // ES6: if let Some(super) = try!(self.match_token(TokenData::Reserved(Reserved::Super))) {
        let base = try!(self.member_base_expression());
        self.more_new_expression(news, base)
    }

    // "new"+n MemberBaseExpression . Deref* Arguments<n Suffix*
    fn more_new_expression(&mut self, news: Vec<Token>, mut base: Expr) -> Result<Expr> {
        let mut derefs = Vec::new();
        while let Some(deref) = try!(self.deref_opt()) {
            derefs.push(deref);
        }
        let mut args_lists = Vec::new();
        for _ in 0..news.len() {
            if try!(self.peek_op()).value != TokenData::LParen {
                break;
            }
            args_lists.push(try!(self.arguments()));
        }
        let suffixes = try!(self.suffixes());
        for deref in derefs {
            base = deref.append_to(base);
        }
        let mut news = news.into_iter().rev();
        for args in args_lists {
            base = args.append_to_new(news.next().unwrap(), base);
        }
        for new in news {
            let location = span(&Some(new.location), &base);
            base = Expr::New(location, Box::new(base), None);
        }
        for suffix in suffixes {
            base = suffix.append_to(base);
        }
        Ok(base)
    }

    // CallExpression ::=
    //   (MemberBaseExpression | "super" Suffix) Suffix*
    fn call_expression(&mut self) -> Result<Expr> {
        // ES6: super
        let base = try!(self.primary_expression());
        self.more_call_expression(base)
    }

    // Suffix ::=
    //   Deref
    //   Arguments
    fn suffix_opt(&mut self) -> Result<Option<Suffix>> {
        match try!(self.peek_op()).value {
            TokenData::Dot    => self.deref_dot().map(|deref| Some(Suffix::Deref(deref))),
            TokenData::LBrack => self.deref_brack().map(|deref| Some(Suffix::Deref(deref))),
            TokenData::LParen => self.arguments().map(|args| Some(Suffix::Arguments(args))),
            _ => Ok(None)
        }
    }


    // Argument ::= "..."? AssignmentExpression
    fn argument(&mut self) -> Result<Expr> {
        // ES6: if let ellipsis = try!(self.matches(TokenData::Ellipsis)) { ... }
        self.allow_in(true, |this| this.assignment_expression())
    }

    // Arguments ::= "(" Argument*[","] ")"
    fn arguments(&mut self) -> Result<Arguments> {
        try!(self.expect(TokenData::LParen));
        if let Some(end) = try!(self.matches_token(TokenData::RParen)) {
            return Ok(Arguments { args: Vec::new(), end: end });
        }
        let mut args = Vec::new();
        loop {
            args.push(try!(self.argument()));
            if !try!(self.matches(TokenData::Comma)) {
                break;
            }
        }
        let end = try!(self.expect(TokenData::RParen));
        Ok(Arguments { args: args, end: end })
    }

/*
    fn deref(&mut self) -> Result<Deref> {
        match try!(self.peek_op()).value {
            TokenData::LBrack => self.deref_brack(),
            TokenData::Dot    => self.deref_dot(),
            _ => Err(Error::UnexpectedToken(try!(self.read_op())))
        }
    }
*/

    // Deref ::=
    //   "[" Expression "]"
    //   "." IdentifierName
    fn deref_opt(&mut self) -> Result<Option<Deref>> {
        match try!(self.peek_op()).value {
            TokenData::LBrack => self.deref_brack().map(Some),
            TokenData::Dot    => self.deref_dot().map(Some),
            _ => Ok(None)
        }
    }

    fn deref_brack(&mut self) -> Result<Deref> {
        self.reread(TokenData::LBrack);
        let expr = try!(self.allow_in(true, |this| this.expression()));
        let end = try!(self.expect(TokenData::RBrack));
        Ok(Deref::Brack(expr, end))
    }

    fn id_name(&mut self) -> Result<DotKey> {
        let token = try!(self.read());
        Ok(DotKey {
            location: Some(token.location),
            value: match token.value {
                TokenData::Identifier(name) => name.into_string(),
                TokenData::Reserved(word) => word.into_string(),
                _ => { return Err(Error::UnexpectedToken(token)); }
            }
        })
    }

    fn deref_dot(&mut self) -> Result<Deref> {
        self.reread(TokenData::Dot);
        let key = try!(self.id_name());
        Ok(Deref::Dot(key))
    }

    // MemberBaseExpression . Suffix*
    fn more_call_expression(&mut self, base: Expr) -> Result<Expr> {
        let mut result = base;
        let suffixes = try!(self.suffixes());
        for suffix in suffixes {
            result = suffix.append_to(result);
        }
        Ok(result)
    }

    fn suffixes(&mut self) -> Result<Vec<Suffix>> {
        let mut suffixes = Vec::new();
        while let Some(suffix) = try!(self.suffix_opt()) {
            suffixes.push(suffix);
        }
        Ok(suffixes)
    }

    // LHSExpression ::=
    //   NewExpression
    //   CallExpression
    fn lhs_expression(&mut self) -> Result<Expr> {
        let mut news = Vec::new();
        while try!(self.peek()).value == TokenData::Reserved(Reserved::New) {
            news.push(self.reread(TokenData::Reserved(Reserved::New)));
        }
        if news.len() > 0 {
            if try!(self.matches_op(TokenData::Dot)) {
                let target_location = Some(try!(self.expect(TokenData::Identifier(Name::Atom(Atom::Target)))).location);
                let new = news.pop();
                let new_location = new.map(|new| new.location);
                let new_target = Expr::NewTarget(span(&new_location, &target_location));
                if news.len() > 0 {
                    self.more_new_expression(news, new_target)
                } else {
                    self.more_call_expression(new_target)
                }
            } else {
                self.new_expression(news)
            }
        } else {
            self.call_expression()
        }
    }

    // IDUnaryExpression ::=
    //   IdentifierReference Suffix* PostfixOperator?
    fn id_unary_expression(&mut self, id: Id) -> Result<Expr> {
        let mut result = Expr::Id(id);
        let suffixes = try!(self.suffixes());
        for suffix in suffixes {
            result = suffix.append_to(result);
        }
        if let Some(postfix) = try!(self.match_postfix_operator_opt()) {
            let result_location = *result.tracking_ref();
            result = match result.into_assign_target().map(Box::new) {
                Ok(target) => {
                    match postfix {
                        Postfix::Inc(location) => Expr::PostInc(Some(location), target),
                        Postfix::Dec(location) => Expr::PostDec(Some(location), target)
                    }
                }
                Err(cover_err) => { return Err(Error::InvalidLHS(result_location, cover_err)); }
            };
        }
        Ok(result)
    }

    // UnaryExpression ::=
    //   Prefix* LHSExpression PostfixOperator?
    fn unary_expression(&mut self) -> Result<Expr> {
        let mut prefixes = Vec::new();
        while let Some(prefix) = try!(self.match_prefix()) {
            prefixes.push(prefix);
        }
        let mut arg = try!(self.lhs_expression());
        if let Some(postfix) = try!(self.match_postfix_operator_opt()) {
            let arg_location = *arg.tracking_ref();
            arg = match arg.into_assign_target().map(Box::new) {
                Ok(target) => {
                    match postfix {
                        Postfix::Inc(location) => Expr::PostInc(Some(location), target),
                        Postfix::Dec(location) => Expr::PostDec(Some(location), target)
                    }
                }
                Err(cover_err) => { return Err(Error::InvalidLHS(arg_location, cover_err)); }
            };
        }
        for prefix in prefixes.into_iter().rev() {
            match prefix {
                Prefix::Unop(op)      => {
                    let location = span(&op, &arg);
                    arg = Expr::Unop(location, op, Box::new(arg));
                }
                _ => {
                    let arg_location = *arg.tracking_ref();
                    arg = match arg.into_assign_target().map(Box::new) {
                        Ok(target) => {
                            match prefix {
                                Prefix::Inc(location) => Expr::PreInc(Some(location), target),
                                Prefix::Dec(location) => Expr::PreDec(Some(location), target),
                                Prefix::Unop(_) => unreachable!()
                            }
                        }
                        Err(cover_err) => { return Err(Error::InvalidLHS(arg_location, cover_err)); }
                    };
                }
            }
        }
        Ok(arg)
    }

    // Prefix ::=
    //   Unop
    //   "++"
    //   "--"
    fn match_prefix(&mut self) -> Result<Option<Prefix>> {
        let token = try!(self.read());
        Ok(match token.value {
            TokenData::Inc => Some(Prefix::Inc(token.location)),
            TokenData::Dec => Some(Prefix::Dec(token.location)),
            _ => {
                self.lexer.unread_token(token);
                try!(self.match_unop()).map(Prefix::Unop)
            }
        })
    }

    // Unop ::=
    //   "delete"
    //   "void"
    //   "typeof"
    //   "+"
    //   "-"
    //   "~"
    //   "!"
    fn match_unop(&mut self) -> Result<Option<Unop>> {
        let token = try!(self.read());
        let tag = match token.value {
            TokenData::Reserved(Reserved::Delete) => UnopTag::Delete,
            TokenData::Reserved(Reserved::Void)   => UnopTag::Void,
            TokenData::Reserved(Reserved::Typeof) => UnopTag::Typeof,
            TokenData::Plus                       => UnopTag::Plus,
            TokenData::Minus                      => UnopTag::Minus,
            TokenData::Tilde                      => UnopTag::BitNot,
            TokenData::Bang                       => UnopTag::Not,
            _ => { self.lexer.unread_token(token); return Ok(None); }
        };
        Ok(Some(Op { location: Some(token.location), tag: tag }))
    }

    // PostfixOperator ::=
    //   [no line terminator] "++"
    //   [no line terminator] "--"
    fn match_postfix_operator_opt(&mut self) -> Result<Option<Postfix>> {
        let next = try!(self.read_op());
        if !next.newline {
            match next.value {
                TokenData::Inc => { return Ok(Some(Postfix::Inc(next.location))); }
                TokenData::Dec => { return Ok(Some(Postfix::Dec(next.location))); }
                _ => { }
            }
        }
        self.lexer.unread_token(next);
        Ok(None)
    }

    // ConditionalExpression ::=
    //   UnaryExpression (Infix UnaryExpression)* ("?" AssignmentExpression ":" AssignmentExpression)?
    fn conditional_expression(&mut self) -> Result<Expr> {
        let left = try!(self.unary_expression());
        let test = try!(self.more_infix_expressions(left));
        self.more_conditional(test)
    }

    // IDConditionalExpression ::=
    //   IDUnaryExpression (Infix UnaryExpression)* ("?" AssignmentExpression ":" AssignmentExpression)?
    fn id_conditional_expression(&mut self, id: Id) -> Result<Expr> {
        let left = try!(self.id_unary_expression(id));
        let test = try!(self.more_infix_expressions(left));
        self.more_conditional(test)
    }

    fn more_conditional(&mut self, left: Expr) -> Result<Expr> {
        if try!(self.matches_op(TokenData::Question)) {
            let cons = try!(self.allow_in(true, |this| this.assignment_expression()));
            try!(self.expect(TokenData::Colon));
            let alt = try!(self.assignment_expression());
            let location = span(&cons, &alt);
            return Ok(Expr::Cond(location, Box::new(left), Box::new(cons), Box::new(alt)));
        }
        Ok(left)
    }

    // AssignmentExpression ::=
    //   YieldPrefix* "yield"
    //   YieldPrefix* ConditionalExpression (("=" | AssignmentOperator) AssignmentExpression)?
    fn assignment_expression(&mut self) -> Result<Expr> {
        let left = try!(self.conditional_expression());
        self.more_assignment(left)
    }

    // IDAssignmentExpression ::=
    //   YieldPrefix* "yield"
    //   YieldPrefix+ ConditionalExpression (("=" | AssignmentOperator) AssignmentExpression)?
    //   IDConditionalExpression (("=" | AssignmentOperator) AssignmentExpression)?
    fn id_assignment_expression(&mut self, id: Id) -> Result<Expr> {
        let left = try!(self.id_conditional_expression(id));
        self.more_assignment(left)
    }

    fn more_assignment(&mut self, left: Expr) -> Result<Expr> {
        let token = try!(self.read_op());
        let left_location = *left.tracking_ref();
        if token.value == TokenData::Assign {
            let left = match left.into_assign_patt() {
                Ok(left) => left,
                Err(cover_err) => { return Err(Error::InvalidLHS(left_location, cover_err)); }
            };
            let right = try!(self.assignment_expression());
            let location = span(&left, &right);
            return Ok(Expr::Assign(location, left, Box::new(right)));
        } else if let Some(op) = token.to_assop() {
            let left = match left.into_assign_target() {
                Ok(left) => left,
                Err(cover_err) => { return Err(Error::InvalidLHS(left_location, cover_err)); }
            };
            let right = try!(self.assignment_expression());
            let location = span(&left, &right);
            return Ok(Expr::BinAssign(location, op, left, Box::new(right)));
        }
        self.lexer.unread_token(token);
        Ok(left)
    }

    fn more_infix_expressions(&mut self, left: Expr) -> Result<Expr> {
        let mut stack = Stack::new();
        let mut operand = left;
        while let Some(op) = try!(self.match_infix()) {
            stack.extend(operand, op);
            //println!("{}\n", stack);
            operand = try!(self.unary_expression());
        }
        Ok(stack.finish(operand))
    }

    fn match_infix(&mut self) -> Result<Option<Infix>> {
        let token = try!(self.read_op());
        let result = token.to_binop(self.parser_cx.allow_in).map_or_else(|| {
            token.to_logop().map(Infix::Logop)
        }, |op| Some(Infix::Binop(op)));
        if result.is_none() {
            self.lexer.unread_token(token);
        }
        Ok(result)
    }

    // Expression ::=
    //   AssignmentExpression ("," AssignmentExpression)*
    fn expression(&mut self) -> Result<Expr> {
        let first = try!(self.assignment_expression());
        self.more_expressions(first)
    }

    // IDExpression ::=
    //   IDAssignmentExpression ("," AssignmentExpression)*
    fn id_expression(&mut self, id: Id) -> Result<Expr> {
        let first = try!(self.id_assignment_expression(id));
        self.more_expressions(first)
    }

    fn more_expressions(&mut self, first: Expr) -> Result<Expr> {
        if try!(self.peek()).value != TokenData::Comma {
            return Ok(first);
        }
        let mut elts = vec![first];
        while try!(self.matches(TokenData::Comma)) {
            elts.push(try!(self.assignment_expression()));
        }
        let location = self.vec_span(&elts);
        Ok(Expr::Seq(location, elts))
    }
}
