// SPDX-License-Identifier: Apache-2.0

//! Recursive-descent + Pratt parser for Dray.
//!
//! Produces a **lossless** green CST: every token the lexer emitted — including
//! whitespace, comments, and lexer-error tokens — is attached somewhere in the
//! tree, so the source can be reprinted byte-for-byte.

use crate::cst::{GreenElement, GreenNode, GreenToken, SyntaxKind, SyntaxNode};
use crate::lexer::tokenize;
use crate::token::{LexError, Span, Token, TokenKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

pub struct Parse {
    pub root: SyntaxNode,
    pub errors: Vec<ParseError>,
}

impl Parse {
    pub fn debug_tree(&self) -> String {
        crate::cst::debug_tree(&self.root)
    }
}

pub fn parse(src: &str) -> Parse {
    let tokens = tokenize(src);
    let mut p = Parser::new(src, tokens);
    p.source_file();
    p.finish()
}

// ── the builder: assembles the green tree from a stack of in-progress nodes ──

/// A node under construction: its kind and the children accumulated so far.
struct Building {
    kind: SyntaxKind,
    children: Vec<GreenElement>,
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    stack: Vec<Building>,
    errors: Vec<ParseError>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, tokens: Vec<Token>) -> Parser<'a> {
        Parser {
            src,
            tokens,
            pos: 0,
            stack: Vec::new(),
            errors: Vec::new(),
        }
    }

    // ── node stack ───────────────────────────────────────────────────────────

    fn start(&mut self, kind: SyntaxKind) {
        self.stack.push(Building {
            kind,
            children: Vec::new(),
        });
    }

    fn finish_node(&mut self) {
        let done = self.stack.pop().expect("finish_node with empty stack");
        let green = GreenNode::new(done.kind, done.children);
        match self.stack.last_mut() {
            Some(parent) => parent.children.push(GreenElement::Node(green)),
            None => {
                // Re-push as the sole root holder so `finish` can grab it.
                self.stack.push(Building {
                    kind: done.kind,
                    children: vec![GreenElement::Node(green)],
                });
            }
        }
    }

    fn finish(mut self) -> Parse {
        // The outermost node is the SourceFile; after `source_file` it's the only
        // frame left. Its single child is the real root green node.
        let root_frame = self.stack.pop().expect("no root");
        let green = match root_frame.children.into_iter().next() {
            Some(GreenElement::Node(n)) => n,
            _ => GreenNode::new(SyntaxKind::SourceFile, vec![]),
        };
        Parse {
            root: SyntaxNode::new_root(green),
            errors: self.errors,
        }
    }

    // ── trivia + token flow ──────────────────────────────────────────────────

    fn eat_trivia(&mut self) {
        while let Some(&tok) = self.tokens.get(self.pos) {
            let is_trivia = tok.is_trivia();
            let is_lex_error = matches!(tok.kind, TokenKind::Error(_));
            if is_trivia || is_lex_error {
                let sk = SyntaxKind::from_token(tok.kind);
                let text = tok.span.text(self.src).to_string();
                self.push_leaf(sk, text);
                if let TokenKind::Error(e) = tok.kind {
                    self.error_at(tok.span, lex_error_message(e));
                }
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn push_leaf(&mut self, kind: SyntaxKind, text: String) {
        let leaf = GreenElement::Token(GreenToken::new(kind, text));
        self.stack
            .last_mut()
            .expect("push_leaf with empty stack")
            .children
            .push(leaf);
    }

    /// The kind of the next significant token (trivia skipped), or `Eof`.
    fn peek(&self) -> TokenKind {
        self.peek_nth(0)
    }

    fn peek_nth(&self, n: usize) -> TokenKind {
        let mut seen = 0;
        let mut i = self.pos;
        while let Some(tok) = self.tokens.get(i) {
            if tok.is_trivia() || matches!(tok.kind, TokenKind::Error(_)) {
                i += 1;
                continue;
            }
            if seen == n {
                return tok.kind;
            }
            seen += 1;
            i += 1;
        }
        TokenKind::Eof
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek() == kind
    }

    fn at_eof(&self) -> bool {
        self.peek() == TokenKind::Eof
    }

    /// Consume the current significant token as a leaf of the current node,
    /// flushing leading trivia first. Panics only if called at EOF.
    fn bump(&mut self) {
        self.eat_trivia();
        let tok = self.tokens[self.pos];
        debug_assert!(tok.kind != TokenKind::Eof, "bump at EOF");
        let sk = SyntaxKind::from_token(tok.kind);
        let text = tok.span.text(self.src).to_string();
        self.push_leaf(sk, text);
        self.pos += 1;
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind, what: &str) {
        if !self.eat(kind) {
            let span = self.cur_span();
            self.error_at(span, format!("expected {what}"));
        }
    }

    fn cur_span(&self) -> Span {
        let mut i = self.pos;
        while let Some(tok) = self.tokens.get(i) {
            if tok.is_trivia() || matches!(tok.kind, TokenKind::Error(_)) {
                i += 1;
                continue;
            }
            return tok.span;
        }
        // EOF: empty span at end of source.
        let end = self.src.len() as u32;
        Span::new(end, end)
    }

    fn error_at(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(ParseError {
            message: message.into(),
            span,
        });
    }

    fn err_and_bump(&mut self, message: impl Into<String>) {
        let span = self.cur_span();
        self.error_at(span, message);
        self.start(SyntaxKind::Error);
        if !self.at_eof() {
            self.bump();
        }
        self.finish_node();
    }

    // ── grammar: top level ───────────────────────────────────────────────────

    fn source_file(&mut self) {
        self.start(SyntaxKind::SourceFile);
        while !self.at_eof() {
            let progress = self.pos;
            self.top_level_decl();
            // Guarantee forward progress even if a sub-parser bailed early.
            if self.pos == progress && !self.at_eof() {
                self.err_and_bump("unexpected token at top level");
            }
        }
        self.eat_trivia(); // trailing whitespace/comments before EOF
        self.finish_node();
    }

    fn top_level_decl(&mut self) {
        match self.peek() {
            TokenKind::KwCHeader => self.c_header_decl(),
            TokenKind::KwPub | TokenKind::Ident => self.named_decl(),
            _ => self.err_and_bump("expected a top-level declaration"),
        }
    }

    /// `c_header ( string_lit ) ;`
    fn c_header_decl(&mut self) {
        self.start(SyntaxKind::CHeaderDecl);
        self.bump(); // c_header
        self.expect(TokenKind::LParen, "'(' after c_header");
        self.expect(TokenKind::StringLit, "a header name string");
        self.expect(TokenKind::RParen, "')'");
        self.expect(TokenKind::Semi, "';'");
        self.finish_node();
    }

    /// `[ "pub" ] identifier "::" ConstExpr`. Only the ProcDef form of
    /// `ConstExpr` is implemented; other forms degrade to an Error node.
    fn named_decl(&mut self) {
        let base = if self.peek() == TokenKind::KwPub {
            1
        } else {
            0
        };
        let head_ok = self.peek_nth(base) == TokenKind::Ident
            && self.peek_nth(base + 1) == TokenKind::ColonColon;
        let after_head = self.peek_nth(base + 2);

        if head_ok && after_head == TokenKind::KwProc {
            self.proc_def();
        } else if head_ok && after_head == TokenKind::KwExtern {
            self.extern_proc_decl();
        } else if head_ok && after_head == TokenKind::KwStruct {
            self.struct_def();
        } else if head_ok && after_head == TokenKind::KwEnum {
            self.enum_def();
        } else {
            self.start(SyntaxKind::Error);
            self.error_at(
                self.cur_span(),
                "only `name :: proc(...)` and `name :: extern \"…\" proc(...)` top-level decls are implemented so far",
            );
            self.eat(TokenKind::KwPub);
            self.eat(TokenKind::Ident);
            // consume up to and including the binding op if present
            if self.at(TokenKind::ColonColon) {
                self.bump();
            }
            self.recover_to_top_level();
            self.finish_node();
        }
    }

    /// `[ "pub" ] identifier "::" "struct" "{" { FieldDecl [ "," ] } "}"`.
    /// The generic-parameter receiver form (`struct ( ... ) { ... }`) is deferred
    fn struct_def(&mut self) {
        self.start(SyntaxKind::StructDef);
        self.eat(TokenKind::KwPub);
        self.expect(TokenKind::Ident, "the struct name");
        self.expect(TokenKind::ColonColon, "'::'");
        self.expect(TokenKind::KwStruct, "'struct'");
        if self.at(TokenKind::LParen) {
            self.param_list();
        }
        self.expect(TokenKind::LBrace, "'{' to open the struct body");
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            self.field_decl();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "'}' to close the struct body");
        self.finish_node();
    }

    /// `identifier ":" Type` — one field of a struct.
    fn field_decl(&mut self) {
        self.start(SyntaxKind::FieldDecl);
        self.expect(TokenKind::Ident, "a field name");
        self.expect(TokenKind::Colon, "':' before the field type");
        self.type_ref();
        self.finish_node();
    }

    /// `[ "pub" ] identifier "::" "enum" [ "(" ParamList ")" ] "{" { EnumVariant [ "," ] } "}"`.
    fn enum_def(&mut self) {
        self.start(SyntaxKind::EnumDef);
        self.eat(TokenKind::KwPub);
        self.expect(TokenKind::Ident, "the enum name");
        self.expect(TokenKind::ColonColon, "'::'");
        self.expect(TokenKind::KwEnum, "'enum'");
        // Optional generic parameter clause: `enum(comptime T: type) { ... }`.
        if self.at(TokenKind::LParen) {
            self.param_list();
        }
        self.expect(TokenKind::LBrace, "'{' to open the enum body");
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            self.enum_variant();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "'}' to close the enum body");
        self.finish_node();
    }

    /// `identifier [ "(" Type { "," Type } ")" ]` — a variant, with an optional
    /// tuple-style payload type list.
    fn enum_variant(&mut self) {
        self.start(SyntaxKind::EnumVariant);
        self.expect(TokenKind::Ident, "a variant name");
        if self.eat(TokenKind::LParen) {
            self.start(SyntaxKind::TypeList);
            while !self.at(TokenKind::RParen) && !self.at_eof() {
                self.type_ref();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.finish_node(); // TypeList
            self.expect(TokenKind::RParen, "')' after the payload types");
        }
        self.finish_node();
    }

    /// `[ "pub" ] identifier "::" "extern" string_lit "proc" "(" ParamList ")"
    /// [ "->" Type ] ";"`. No receiver, no body — an external C function.
    fn extern_proc_decl(&mut self) {
        self.start(SyntaxKind::ExternProcDecl);
        self.eat(TokenKind::KwPub);
        self.expect(TokenKind::Ident, "the binding name");
        self.expect(TokenKind::ColonColon, "'::'");
        self.expect(TokenKind::KwExtern, "'extern'");
        self.expect(TokenKind::StringLit, "the C symbol name string");
        self.expect(TokenKind::KwProc, "'proc'");
        self.param_list();
        if self.at(TokenKind::Arrow) {
            self.ret_type();
        }
        self.expect(TokenKind::Semi, "';' after an extern declaration");
        self.finish_node();
    }

    /// Skip tokens until something that plausibly starts a new top-level decl,
    /// so one bad decl doesn't swallow the whole file.
    fn recover_to_top_level(&mut self) {
        let mut depth: i32 = 0;
        while !self.at_eof() {
            match self.peek() {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth <= 0 {
                        self.bump(); // consume the closing brace
                        break;
                    }
                }
                TokenKind::KwCHeader if depth <= 0 => break,
                // A `pub`/ident at brace depth 0 likely starts the next decl.
                TokenKind::KwPub if depth <= 0 => break,
                TokenKind::Semi if depth <= 0 => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// `identifier "::" proc ( ParamList ) [ "->" Type ] Block`
    fn proc_def(&mut self) {
        self.start(SyntaxKind::ProcDef);
        self.eat(TokenKind::KwPub);
        self.expect(TokenKind::Ident, "the proc's name");
        self.expect(TokenKind::ColonColon, "'::'");
        self.expect(TokenKind::KwProc, "'proc'");
        self.param_list();
        if self.at(TokenKind::Arrow) {
            self.ret_type();
        }
        if self.at(TokenKind::LBrace) {
            self.block();
        } else {
            self.error_at(self.cur_span(), "expected '{' to begin the proc body");
        }
        self.finish_node();
    }

    /// `( [ Param { "," Param } ] )`
    fn param_list(&mut self) {
        self.start(SyntaxKind::ParamList);
        self.expect(TokenKind::LParen, "'('");
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            self.param();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.at(TokenKind::RParen) {
                let span = self.cur_span();
                self.error_at(span, "expected a parameter after `,`");
                break;
            }
        }
        self.expect(TokenKind::RParen, "')'");
        self.finish_node();
    }

    /// `[ "comptime" ] identifier ":" Type`
    fn param(&mut self) {
        self.start(SyntaxKind::Param);
        self.eat(TokenKind::KwComptime);
        self.expect(TokenKind::Ident, "a parameter name");
        self.expect(TokenKind::Colon, "':' before the parameter type");
        self.type_ref();
        self.finish_node();
    }

    /// `"(" Type { "," Type } ")"`
    fn type_arg_list(&mut self) {
        self.start(SyntaxKind::TypeArgList);
        self.expect(TokenKind::LParen, "'('");
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            self.type_ref();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "')' after the type arguments");
        self.finish_node();
    }

    /// `-> Type`
    fn ret_type(&mut self) {
        self.start(SyntaxKind::RetType);
        self.bump(); // ->
        self.type_ref();
        self.finish_node();
    }

    // ── grammar: types ───────────────────────────────────────────────────────

    /// A subset of `Type`: pointer, RC pointer, slice, array, name, generic.
    /// Proc types and variadic types are deferred.
    fn type_ref(&mut self) {
        match self.peek() {
            TokenKind::Star => {
                self.start(SyntaxKind::PointerType);
                self.bump();
                self.type_ref();
                self.finish_node();
            }
            TokenKind::At => {
                self.start(SyntaxKind::RcPointerType);
                self.bump();
                self.type_ref();
                self.finish_node();
            }
            TokenKind::LBracket => {
                // `[]T` slice vs `[N]T` array. `[...]T` variadic is deferred
                if self.peek_nth(1) == TokenKind::RBracket {
                    self.start(SyntaxKind::SliceType);
                    self.bump(); // [
                    self.bump(); // ]
                    self.type_ref();
                    self.finish_node();
                } else {
                    self.start(SyntaxKind::ArrayType);
                    self.bump(); // [
                    self.expr(); // the size expression
                    self.expect(TokenKind::RBracket, "']' after array size");
                    self.type_ref();
                    self.finish_node();
                }
            }
            TokenKind::Ident => {
                if self.peek_nth(1) == TokenKind::LParen {
                    self.start(SyntaxKind::GenericType);
                    self.bump(); // type name
                    self.type_arg_list();
                    self.finish_node();
                } else {
                    self.start(SyntaxKind::NameType);
                    self.bump();
                    self.finish_node();
                }
            }
            _ => self.err_and_bump("expected a type"),
        }
    }

    // ── grammar: blocks & statements ─────────────────────────────────────────

    /// `{ Statement }`
    fn block(&mut self) {
        self.start(SyntaxKind::Block);
        self.expect(TokenKind::LBrace, "'{'");
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let progress = self.pos;
            self.statement();
            if self.pos == progress {
                self.err_and_bump("unexpected token in block");
            }
        }
        self.expect(TokenKind::RBrace, "'}'");
        self.finish_node();
    }

    fn statement(&mut self) {
        match self.peek() {
            TokenKind::KwReturn => self.return_stmt(),
            TokenKind::KwBreak => self.simple_kw_stmt(SyntaxKind::BreakStmt),
            TokenKind::KwContinue => self.simple_kw_stmt(SyntaxKind::ContinueStmt),
            TokenKind::KwIf => self.if_stmt(),
            TokenKind::KwFor => self.for_stmt(),
            TokenKind::KwSwitch => self.switch_stmt(),
            TokenKind::LBrace => self.block(),
            _ => self.simple_stmt(true),
        }
    }

    fn simple_stmt(&mut self, want_semi: bool) {
        match self.peek() {
            // `identifier ( "::" | ":=" | "::=" ) ...` — bare VarDecl
            TokenKind::Ident
                if matches!(
                    self.peek_nth(1),
                    TokenKind::ColonColon | TokenKind::ColonEq | TokenKind::ColonColonEq
                ) =>
            {
                self.var_decl_bare(want_semi)
            }
            TokenKind::Ident if self.peek_nth(1) == TokenKind::Colon => {
                self.var_decl_typed(want_semi)
            }
            _ => self.assign_or_expr_stmt(want_semi),
        }
    }

    /// `return [ Expression ] ;`
    fn return_stmt(&mut self) {
        self.start(SyntaxKind::ReturnStmt);
        self.bump(); // return
        if !self.at(TokenKind::Semi) && !self.at(TokenKind::RBrace) && !self.at_eof() {
            self.expr();
        }
        self.expect(TokenKind::Semi, "';' after return");
        self.finish_node();
    }

    /// `break ;` / `continue ;`
    fn simple_kw_stmt(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.bump(); // break | continue
        self.expect(TokenKind::Semi, "';'");
        self.finish_node();
    }

    /// Bare VarDecl: `identifier ( "::" | ":=" | "::=" ) Expression [ ";" ]`.
    fn var_decl_bare(&mut self, want_semi: bool) {
        self.start(SyntaxKind::VarDecl);
        self.bump(); // name
        self.bump(); // :: | := | ::=
        self.expr();
        if want_semi {
            self.expect(TokenKind::Semi, "';' after declaration");
        }
        self.finish_node();
    }

    /// Explicit-type VarDecl:
    /// `identifier ":" Type ( ":" | "=" | ":=" ) Expression [ ";" ]`.
    fn var_decl_typed(&mut self, want_semi: bool) {
        self.start(SyntaxKind::VarDecl);
        self.bump(); // name
        self.bump(); // ':'
        self.type_ref();
        match self.peek() {
            TokenKind::Colon | TokenKind::Eq | TokenKind::ColonEq => self.bump(),
            _ => self.error_at(
                self.cur_span(),
                "expected ':' , '=' , or ':=' after the type annotation",
            ),
        }
        self.expr();
        if want_semi {
            self.expect(TokenKind::Semi, "';' after declaration");
        }
        self.finish_node();
    }

    /// Parse an expression; if an `AssignOp` follows, fold into an `AssignStmt`,
    /// otherwise it's an `ExprStmt`. `[ ";" ]` per `want_semi`.
    fn assign_or_expr_stmt(&mut self, want_semi: bool) {
        let cp = self.checkpoint();
        self.expr();
        if is_assign_op(self.peek()) {
            self.wrap_at(cp, SyntaxKind::AssignStmt);
            self.bump(); // the assignment operator
            self.expr(); // right-hand side
            if want_semi {
                self.expect(TokenKind::Semi, "';' after assignment");
            }
            self.finish_node();
        } else {
            self.wrap_at(cp, SyntaxKind::ExprStmt);
            if want_semi {
                self.expect(TokenKind::Semi, "';' after expression");
            }
            self.finish_node();
        }
    }

    /// `"switch" [ Expression ] "{" { CaseClause } "}"`. The scrutinee is
    /// optional (a bare `switch { }` matches on `true`, like Go).
    fn switch_stmt(&mut self) {
        self.start(SyntaxKind::SwitchStmt);
        self.expect(TokenKind::KwSwitch, "'switch'");
        if !self.at(TokenKind::LBrace) {
            self.expr();
        }
        self.expect(TokenKind::LBrace, "'{' to open the switch body");
        while self.at(TokenKind::KwCase) && !self.at_eof() {
            self.case_clause();
        }
        self.expect(TokenKind::RBrace, "'}' to close the switch body");
        self.finish_node();
    }

    /// `"case" PatternList ":" { Statement }`.
    fn case_clause(&mut self) {
        self.start(SyntaxKind::CaseClause);
        self.expect(TokenKind::KwCase, "'case'");
        self.pattern();
        while self.eat(TokenKind::Comma) {
            self.pattern();
        }
        self.expect(TokenKind::Colon, "':' after the case pattern");
        // Statements until the next `case` or the closing `}`.
        while !self.at(TokenKind::KwCase) && !self.at(TokenKind::RBrace) && !self.at_eof() {
            self.statement();
        }
        self.finish_node();
    }

    /// `Pattern = EnumPattern | Expression`. An `EnumPattern` is
    /// `TypeName "." identifier [ "(" IdentifierList ")" ]`; anything else is an
    /// ordinary expression pattern (value match).
    fn pattern(&mut self) {
        // Lookahead for `Ident . Ident` — the enum-pattern shape.
        if self.peek() == TokenKind::Ident
            && self.peek_nth(1) == TokenKind::Dot
            && self.peek_nth(2) == TokenKind::Ident
        {
            self.start(SyntaxKind::EnumPattern);
            self.bump(); // type name
            self.bump(); // '.'
            self.bump(); // variant name
            if self.eat(TokenKind::LParen) {
                while self.at(TokenKind::Ident) {
                    self.bump(); // binding identifier
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RParen, "')' after the pattern bindings");
            }
            self.finish_node();
        } else {
            self.expr();
        }
    }

    /// `if [ SimpleStmt ";" ] Expression Block [ "else" ( Block | IfStmt ) ]`.
    fn if_stmt(&mut self) {
        self.start(SyntaxKind::IfStmt);
        self.bump(); // if
        // optional init clause: present iff a top-level `;` precedes the block
        if self.header_has_semi_before_brace() {
            self.simple_stmt(false);
            self.expect(TokenKind::Semi, "';' after the if-init statement");
        }
        self.condition();
        if self.at(TokenKind::LBrace) {
            self.block();
        } else {
            self.error_at(self.cur_span(), "expected '{' after the if condition");
        }
        if self.at(TokenKind::KwElse) {
            self.else_clause();
        }
        self.finish_node();
    }

    /// `"else" ( IfStmt | Block )` — chains as else-if via the `IfStmt` branch.
    fn else_clause(&mut self) {
        self.start(SyntaxKind::ElseClause);
        self.bump(); // else
        match self.peek() {
            TokenKind::KwIf => self.if_stmt(),
            TokenKind::LBrace => self.block(),
            _ => self.error_at(self.cur_span(), "expected 'if' or '{' after else"),
        }
        self.finish_node();
    }

    /// A `for` loop, disambiguated across its four grammar forms.
    fn for_stmt(&mut self) {
        self.start(SyntaxKind::ForStmt);
        self.bump(); // for

        if self.at(TokenKind::LBrace) {
            // form 1: infinite loop — `for { ... }`
            self.block();
            self.finish_node();
            return;
        }

        if self.at(TokenKind::Ident)
            && matches!(self.peek_nth(1), TokenKind::KwIn | TokenKind::Comma)
        {
            self.bump(); // element identifier
            if self.at(TokenKind::Comma) {
                self.bump(); // ,
                self.expect(TokenKind::LBracket, "'[' before the index variable");
                self.expect(TokenKind::Ident, "the index variable name");
                self.expect(TokenKind::RBracket, "']' after the index variable");
            }
            self.expect(TokenKind::KwIn, "'in'");
            self.condition(); // the iterable expression
            if self.at(TokenKind::LBrace) {
                self.block();
            } else {
                self.error_at(self.cur_span(), "expected '{' for the loop body");
            }
            self.finish_node();
            return;
        }

        if self.header_has_semi_before_brace() {
            if !self.at(TokenKind::Semi) {
                self.simple_stmt(false); // init
            }
            self.expect(TokenKind::Semi, "';' after the for-init");
            if !self.at(TokenKind::Semi) {
                self.condition(); // condition (lenient: may be empty)
            }
            self.expect(TokenKind::Semi, "';' after the for-condition");
            if !self.at(TokenKind::LBrace) {
                self.simple_stmt(false); // post
            }
        } else {
            self.condition();
        }

        if self.at(TokenKind::LBrace) {
            self.block();
        } else {
            self.error_at(self.cur_span(), "expected '{' for the loop body");
        }
        self.finish_node();
    }

    fn condition(&mut self) {
        self.start(SyntaxKind::Condition);
        self.expr();
        self.finish_node();
    }

    fn header_has_semi_before_brace(&self) -> bool {
        let mut i = self.pos;
        let mut paren = 0i32;
        let mut brack = 0i32;
        while let Some(tok) = self.tokens.get(i) {
            if tok.is_trivia() || matches!(tok.kind, TokenKind::Error(_)) {
                i += 1;
                continue;
            }
            match tok.kind {
                TokenKind::LParen => paren += 1,
                TokenKind::RParen => paren -= 1,
                TokenKind::LBracket => brack += 1,
                TokenKind::RBracket => brack -= 1,
                TokenKind::Semi if paren <= 0 && brack <= 0 => return true,
                TokenKind::LBrace if paren <= 0 && brack <= 0 => return false,
                TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    // ── grammar: expressions (Pratt) ─────────────────────────────────────────

    fn expr(&mut self) {
        self.expr_bp(0);
    }

    /// Pratt loop: parse a unary/primary, then fold in binary operators whose
    /// left binding power is >= `min_bp`.
    fn expr_bp(&mut self, min_bp: u8) {
        let checkpoint = self.checkpoint();
        self.unary_expr();

        loop {
            let op = self.peek();
            let Some((lbp, rbp)) = infix_bp(op) else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.wrap_at(checkpoint, SyntaxKind::BinaryExpr);
            self.bump(); // operator
            self.expr_bp(rbp);
            self.finish_node();
        }
    }

    /// `UnaryExpr = CastExpr | prefix-op UnaryExpr | PostfixExpr`
    fn unary_expr(&mut self) {
        match self.peek() {
            TokenKind::KwCast => self.cast_expr(),
            TokenKind::Minus
            | TokenKind::Bang
            | TokenKind::Tilde
            | TokenKind::Star
            | TokenKind::Amp => {
                self.start(SyntaxKind::PrefixExpr);
                self.bump(); // the prefix operator
                self.unary_expr();
                self.finish_node();
            }
            _ => self.postfix_expr(),
        }
    }

    /// `cast ( Type ) UnaryExpr`
    fn cast_expr(&mut self) {
        self.start(SyntaxKind::CastExpr);
        self.bump(); // cast
        self.expect(TokenKind::LParen, "'(' after cast");
        self.type_ref();
        self.expect(TokenKind::RParen, "')' after cast type");
        self.unary_expr();
        self.finish_node();
    }

    /// `PrimaryExpr { Selector | Call | Index }`
    fn postfix_expr(&mut self) {
        let checkpoint = self.checkpoint();
        self.primary_expr();
        loop {
            match self.peek() {
                TokenKind::Dot => {
                    self.wrap_at(checkpoint, SyntaxKind::FieldExpr);
                    self.bump(); // .
                    self.expect(TokenKind::Ident, "a field or method name");
                    self.finish_node();
                }
                TokenKind::LParen => {
                    self.wrap_at(checkpoint, SyntaxKind::CallExpr);
                    self.arg_list();
                    self.finish_node();
                }
                TokenKind::LBracket => {
                    self.wrap_at(checkpoint, SyntaxKind::IndexExpr);
                    self.bump(); // [
                    self.expr();
                    self.expect(TokenKind::RBracket, "']' after index");
                    self.finish_node();
                }
                _ => break,
            }
        }
    }

    /// `( ArgumentList )` — argument spread (`...`) is deferred.
    fn arg_list(&mut self) {
        self.start(SyntaxKind::ArgList);
        self.expect(TokenKind::LParen, "'('");
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            if self.at(TokenKind::At) || self.at(TokenKind::LBracket) {
                self.type_ref();
            } else {
                self.expr();
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.at(TokenKind::RParen) {
                let span = self.cur_span();
                self.error_at(span, "expected an argument after `,`");
                break;
            }
        }
        self.expect(TokenKind::RParen, "')'");
        self.finish_node();
    }

    fn primary_expr(&mut self) {
        match self.peek() {
            TokenKind::IntLit
            | TokenKind::FloatLit
            | TokenKind::StringLit
            | TokenKind::RuneLit
            | TokenKind::KwTrue
            | TokenKind::KwFalse => {
                self.start(SyntaxKind::LiteralExpr);
                self.bump();
                self.finish_node();
            }
            TokenKind::Ident => {
                self.start(SyntaxKind::NameExpr);
                self.bump();
                self.finish_node();
            }
            TokenKind::KwAlloc | TokenKind::KwTryAlloc => self.alloc_expr(),
            TokenKind::LParen => {
                self.start(SyntaxKind::ParenExpr);
                self.bump(); // (
                self.expr();
                self.expect(TokenKind::RParen, "')'");
                self.finish_node();
            }
            _ => self.err_and_bump("expected an expression"),
        }
    }

    /// `( "alloc" | "try_alloc" ) Type` — the composite-literal form
    /// (`alloc T{...}`) is deferred along with CompositeLit generally.
    fn alloc_expr(&mut self) {
        self.start(SyntaxKind::AllocExpr);
        self.bump(); // alloc | try_alloc
        let cp = self.checkpoint();
        self.type_ref();
        if self.at(TokenKind::LBrace) {
            self.wrap_at(cp, SyntaxKind::CompositeLit);
            self.composite_body();
            self.finish_node(); // CompositeLit
        }
        self.finish_node(); // AllocExpr
    }

    /// `"{" [ Element { "," Element } [ "," ] ] "}"` — the body of a composite
    /// literal, parsed into the currently-open `CompositeLit` node.
    fn composite_body(&mut self) {
        self.expect(TokenKind::LBrace, "'{' to open the composite literal");
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            self.element();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "'}' to close the composite literal");
    }

    /// `[ identifier ":" ] Expression`
    fn element(&mut self) {
        self.start(SyntaxKind::Element);
        if self.peek() == TokenKind::Ident && self.peek_nth(1) == TokenKind::Colon {
            self.bump(); // field name
            self.bump(); // ':'
        }
        self.expr();
        self.finish_node();
    }

    fn checkpoint(&self) -> usize {
        self.stack
            .last()
            .expect("checkpoint with empty stack")
            .children
            .len()
    }

    fn wrap_at(&mut self, cp: usize, kind: SyntaxKind) {
        let frame = self.stack.last_mut().expect("wrap_at with empty stack");
        let moved: Vec<GreenElement> = frame.children.drain(cp..).collect();
        self.stack.push(Building {
            kind,
            children: moved,
        });
    }
}

fn lex_error_message(e: LexError) -> String {
    e.to_string()
}

/// True for any of the grammar's `AssignOp` tokens (`=`, `+=`, …, `>>=`).
fn is_assign_op(kind: TokenKind) -> bool {
    use TokenKind::*;
    matches!(
        kind,
        Eq | PlusEq
            | MinusEq
            | StarEq
            | SlashEq
            | PercentEq
            | AmpEq
            | PipeEq
            | CaretEq
            | ShlEq
            | ShrEq
    )
}

fn infix_bp(kind: TokenKind) -> Option<(u8, u8)> {
    use TokenKind::*;
    let bp = match kind {
        PipePipe => (1, 2),
        AmpAmp => (3, 4),
        Pipe => (5, 6),
        Caret => (7, 8),
        Amp => (9, 10),
        EqEq | BangEq => (11, 12),
        Lt | LtEq | Gt | GtEq => (13, 14),
        Shl | Shr => (15, 16),
        Plus | Minus => (17, 18),
        Star | Slash | Percent => (19, 20),
        _ => return None,
    };
    Some(bp)
}
