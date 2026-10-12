//! Weft Query Language (WQL) engine v0.2:
//! Min-of-Affines IR, AST parser, Query Compiler, Branch Optimizer, and Search Integration.

use crate::{
    retrieve::Level,
    store::{Hit, Store},
    vector::{Encoding, unpack_normalized},
};
use anyhow::{Result, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Abstract Syntax Tree for WQL expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Text(String),
    File(PathBuf),
    All(Vec<Expr>),
    Any(Vec<Expr>),
    Avoid(Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn has_text(&self) -> bool {
        match self {
            Self::Text(_) => true,
            Self::File(_) => false,
            Self::All(xs) | Self::Any(xs) => xs.iter().any(Self::has_text),
            Self::Avoid(a, b) => a.has_text() || b.has_text(),
        }
    }
}

/// Parsed WQL query statement with optional clauses.
#[derive(Debug, Clone, PartialEq)]
pub struct WqlQuery {
    pub expr: Expr,
    pub view: Option<String>,
    pub level: Option<Level>,
    pub top: usize,
}

impl Default for WqlQuery {
    fn default() -> Self {
        Self {
            expr: Expr::Text(String::new()),
            view: None,
            level: None,
            top: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Lexer and Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    StringLit(String),
    Number(usize),
    LParen,
    RParen,
    Comma,
}

struct Lexer<'a> {
    input: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.char_indices().peekable(),
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>> {
        loop {
            match self.chars.peek() {
                Some((_, ch)) if ch.is_whitespace() => {
                    self.chars.next();
                }
                _ => break,
            }
        }

        let Some((start, ch)) = self.chars.next() else {
            return Ok(None);
        };

        match ch {
            '(' => Ok(Some(Token::LParen)),
            ')' => Ok(Some(Token::RParen)),
            ',' => Ok(Some(Token::Comma)),
            '"' | '\'' => {
                let quote = ch;
                let mut content = String::new();
                let mut escaped = false;
                loop {
                    let Some((_, next_ch)) = self.chars.next() else {
                        bail!("unterminated string literal starting at position {start}");
                    };
                    if escaped {
                        match next_ch {
                            'n' => content.push('\n'),
                            'r' => content.push('\r'),
                            't' => content.push('\t'),
                            '\\' => content.push('\\'),
                            _ if next_ch == quote => content.push(quote),
                            _ => {
                                content.push('\\');
                                content.push(next_ch);
                            }
                        }
                        escaped = false;
                    } else if next_ch == '\\' {
                        escaped = true;
                    } else if next_ch == quote {
                        break;
                    } else {
                        content.push(next_ch);
                    }
                }
                Ok(Some(Token::StringLit(content)))
            }
            ch if ch.is_ascii_digit() => {
                let mut end = start + ch.len_utf8();
                while let Some(&(idx, next_ch)) = self.chars.peek() {
                    if next_ch.is_ascii_digit() {
                        self.chars.next();
                        end = idx + next_ch.len_utf8();
                    } else {
                        break;
                    }
                }
                let num_str = &self.input[start..end];
                let num = num_str.parse::<usize>()?;
                Ok(Some(Token::Number(num)))
            }
            ch if ch.is_alphabetic() || ch == '_' || ch == '.' || ch == '/' || ch == '-' => {
                let mut end = start + ch.len_utf8();
                while let Some(&(idx, next_ch)) = self.chars.peek() {
                    if next_ch.is_alphanumeric()
                        || next_ch == '_'
                        || next_ch == '.'
                        || next_ch == '/'
                        || next_ch == '-'
                    {
                        self.chars.next();
                        end = idx + next_ch.len_utf8();
                    } else {
                        break;
                    }
                }
                let ident = &self.input[start..end];
                Ok(Some(Token::Ident(ident.to_string())))
            }
            unexpected => bail!("unexpected character '{unexpected}' at position {start}"),
        }
    }
}

pub struct Parser<'a> {
    tokens: Vec<Token>,
    cursor: usize,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a str) -> Result<Self> {
        let mut lexer = Lexer::new(input);
        let mut tokens = Vec::new();
        while let Some(tok) = lexer.next_token()? {
            tokens.push(tok);
        }
        Ok(Self {
            tokens,
            cursor: 0,
            _marker: std::marker::PhantomData,
        })
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor)
    }

    fn advance(&mut self) -> Option<&Token> {
        if self.cursor < self.tokens.len() {
            let tok = &self.tokens[self.cursor];
            self.cursor += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn expect_lparen(&mut self) -> Result<()> {
        match self.advance() {
            Some(Token::LParen) => Ok(()),
            other => bail!("expected '(', got {other:?}"),
        }
    }

    fn expect_rparen(&mut self) -> Result<()> {
        match self.advance() {
            Some(Token::RParen) => Ok(()),
            other => bail!("expected ')', got {other:?}"),
        }
    }

    pub fn parse_query(&mut self) -> Result<WqlQuery> {
        let expr = self.parse_expr()?;
        let mut view = None;
        let mut level = None;
        let mut top = 10;

        while let Some(tok) = self.peek() {
            match tok {
                Token::Ident(s) if s.eq_ignore_ascii_case("in") => {
                    self.advance();
                    match self.advance() {
                        Some(Token::Ident(v)) | Some(Token::StringLit(v)) => {
                            view = Some(v.clone());
                        }
                        other => bail!("expected view name or path after 'in', got {other:?}"),
                    }
                }
                Token::Ident(s) if s.eq_ignore_ascii_case("level") => {
                    self.advance();
                    match self.advance() {
                        Some(Token::Ident(lvl)) => match lvl.to_ascii_lowercase().as_str() {
                            "chunk" => level = Some(Level::Chunk),
                            "file" => level = Some(Level::File),
                            "folder" => level = Some(Level::Folder),
                            unknown => {
                                bail!("unknown level '{unknown}'; expected chunk, file, or folder")
                            }
                        },
                        other => bail!("expected level identifier, got {other:?}"),
                    }
                }
                Token::Ident(s) if s.eq_ignore_ascii_case("top") => {
                    self.advance();
                    match self.advance() {
                        Some(Token::Number(n)) => {
                            if *n == 0 || *n > 10000 {
                                bail!("top must be between 1 and 10000");
                            }
                            top = *n;
                        }
                        other => bail!("expected number after 'top', got {other:?}"),
                    }
                }
                _ => break,
            }
        }

        if self.cursor < self.tokens.len() {
            bail!(
                "unexpected trailing tokens: {:?}",
                &self.tokens[self.cursor..]
            );
        }

        Ok(WqlQuery {
            expr,
            view,
            level,
            top,
        })
    }

    pub fn parse_expr(&mut self) -> Result<Expr> {
        let Some(tok) = self.advance() else {
            bail!("unexpected end of input while parsing expression");
        };

        match tok {
            Token::StringLit(s) => Ok(Expr::Text(s.clone())),
            Token::Ident(id) => match id.to_ascii_lowercase().as_str() {
                "file" => {
                    self.expect_lparen()?;
                    let path = match self.advance() {
                        Some(Token::StringLit(p)) => PathBuf::from(p),
                        Some(Token::Ident(p)) => PathBuf::from(p),
                        other => bail!("expected file path string in file(...), got {other:?}"),
                    };
                    self.expect_rparen()?;
                    Ok(Expr::File(path))
                }
                "all" => {
                    self.expect_lparen()?;
                    let mut children = Vec::new();
                    children.push(self.parse_expr()?);
                    while let Some(Token::Comma) = self.peek() {
                        self.advance();
                        children.push(self.parse_expr()?);
                    }
                    self.expect_rparen()?;
                    if children.is_empty() {
                        bail!("all() requires at least one argument");
                    }
                    Ok(Expr::All(children))
                }
                "any" => {
                    self.expect_lparen()?;
                    let mut children = Vec::new();
                    children.push(self.parse_expr()?);
                    while let Some(Token::Comma) = self.peek() {
                        self.advance();
                        children.push(self.parse_expr()?);
                    }
                    self.expect_rparen()?;
                    if children.is_empty() {
                        bail!("any() requires at least one argument");
                    }
                    Ok(Expr::Any(children))
                }
                "avoid" => {
                    self.expect_lparen()?;
                    let pos = self.parse_expr()?;
                    match self.advance() {
                        Some(Token::Comma) => {}
                        other => bail!("expected ',' between avoid arguments, got {other:?}"),
                    }
                    let neg = self.parse_expr()?;
                    self.expect_rparen()?;
                    Ok(Expr::Avoid(Box::new(pos), Box::new(neg)))
                }
                other => bail!("unknown operator or identifier '{other}' in expression"),
            },
            other => bail!("unexpected token {other:?} at start of expression"),
        }
    }
}

pub fn parse_wql(input: &str) -> Result<WqlQuery> {
    let mut parser = Parser::new(input.trim())?;
    parser.parse_query()
}

// ---------------------------------------------------------------------------
// Min-of-Affines IR & Query Compiler
// ---------------------------------------------------------------------------

/// Linear affine score formula: S(x) = bias - x^T vector
#[derive(Clone, Debug, PartialEq)]
pub struct Affine {
    pub bias: f32,
    pub vector: Vec<f32>,
}

impl Affine {
    pub fn new(bias: f32, vector: Vec<f32>) -> Self {
        Self { bias, vector }
    }

    /// Linear score evaluation: S(x) = bias - x^T vector
    #[inline]
    pub fn eval(&self, x: &[f32]) -> f32 {
        let dot: f32 = self.vector.iter().zip(x.iter()).map(|(a, b)| a * b).sum();
        self.bias - dot
    }

    /// Euclidean L2 norm of the vector
    #[allow(dead_code)]
    pub fn norm(&self) -> f32 {
        self.vector.iter().map(|v| v * v).sum::<f32>().sqrt()
    }

    /// Normalized direction unit vector for ANN queries: v / ||v||
    #[allow(dead_code)]
    pub fn unit_direction(&self) -> Option<Vec<f32>> {
        let n = self.norm();
        if n <= 1e-7 {
            None
        } else {
            let inv = 1.0 / n;
            Some(self.vector.iter().map(|v| v * inv).collect())
        }
    }
}

/// Compiled query represented as the minimum over one or more linear affine branches:
/// S(x) = min_i (bias_i - x^T vector_i)
#[derive(Clone, Debug)]
pub struct CompiledQuery {
    pub branches: Vec<Affine>,
}

impl CompiledQuery {
    pub fn new(branches: Vec<Affine>) -> Self {
        Self { branches }
    }

    /// Evaluate overall exact WQL score: min_i S_i(x)
    #[inline]
    pub fn eval(&self, x: &[f32]) -> f32 {
        self.branches
            .iter()
            .map(|b| b.eval(x))
            .fold(f32::INFINITY, f32::min)
    }

    /// Evaluate overall exact WQL score and return the matched branch index: (score, branch_index)
    #[inline]
    pub fn eval_with_branch(&self, x: &[f32]) -> (f32, usize) {
        let mut min_score = f32::INFINITY;
        let mut best_branch = 0;
        for (i, b) in self.branches.iter().enumerate() {
            let s = b.eval(x);
            if s < min_score {
                min_score = s;
                best_branch = i;
            }
        }
        (min_score, best_branch)
    }

    /// Normalized unit directions for ANN branch queries: Vec<Option<unit_vector>>
    #[allow(dead_code)]
    pub fn branch_directions(&self) -> Vec<Option<Vec<f32>>> {
        self.branches.iter().map(|b| b.unit_direction()).collect()
    }

    /// Whether the compiled branches exceed the recommended budget (16)
    #[allow(dead_code)]
    pub fn budget_exceeded(&self) -> bool {
        self.branches.len() > 16
    }

    /// Branch optimization:
    /// 1. Deduplication (same vector direction and bias)
    /// 2. Domination pruning: If c_i - c_j >= ||v_i - v_j||, then S_i(x) >= S_j(x) for all unit vectors x.
    pub fn optimize(&mut self) {
        if self.branches.len() <= 1 {
            return;
        }

        // 1. Deduplication
        let mut deduped: Vec<Affine> = Vec::with_capacity(self.branches.len());
        for b in &self.branches {
            let exists = deduped.iter().any(|d| {
                if (d.bias - b.bias).abs() > 1e-5 {
                    return false;
                }
                let diff_norm_sq: f32 = d
                    .vector
                    .iter()
                    .zip(&b.vector)
                    .map(|(v1, v2)| (v1 - v2).powi(2))
                    .sum();
                diff_norm_sq.sqrt() < 1e-5
            });
            if !exists {
                deduped.push(b.clone());
            }
        }

        if deduped.len() <= 1 {
            self.branches = deduped;
            return;
        }

        // 2. Domination pruning
        let mut keep = vec![true; deduped.len()];
        for i in 0..deduped.len() {
            if !keep[i] {
                continue;
            }
            for j in 0..deduped.len() {
                if i == j || !keep[j] {
                    continue;
                }
                let diff_norm: f32 = deduped[i]
                    .vector
                    .iter()
                    .zip(&deduped[j].vector)
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f32>()
                    .sqrt();
                // If c_i - c_j >= ||v_i - v_j||, branch i is always dominated by branch j
                if deduped[i].bias - deduped[j].bias >= diff_norm {
                    keep[i] = false;
                    break;
                }
            }
        }

        self.branches = deduped
            .into_iter()
            .enumerate()
            .filter(|(idx, _)| keep[*idx])
            .map(|(_, b)| b)
            .collect();
    }
}

/// Helper to compile AST expressions into linear Affine branches
pub fn compile_expr<E, F>(
    expr: &Expr,
    dim: usize,
    embed_text: &mut E,
    lookup_file: &mut F,
) -> Result<Vec<Affine>>
where
    E: FnMut(&str) -> Result<Vec<f32>>,
    F: FnMut(&Path) -> Result<Vec<f32>>,
{
    match expr {
        Expr::Text(text) => {
            let vec = embed_text(text)?;
            if vec.len() != dim {
                bail!(
                    "embedded text dimension mismatch: expected {dim}, got {}",
                    vec.len()
                );
            }
            // D(x, a) = (1 - x^T a) / 2 = 0.5 - x^T (a / 2)
            let half_vec: Vec<f32> = vec.into_iter().map(|v| v * 0.5).collect();
            Ok(vec![Affine::new(0.5, half_vec)])
        }
        Expr::File(path) => {
            let vec = lookup_file(path)?;
            if vec.len() != dim {
                bail!(
                    "file vector dimension mismatch: expected {dim}, got {}",
                    vec.len()
                );
            }
            let half_vec: Vec<f32> = vec.into_iter().map(|v| v * 0.5).collect();
            Ok(vec![Affine::new(0.5, half_vec)])
        }
        Expr::Any(children) => {
            if children.is_empty() {
                bail!("any() requires at least one child");
            }
            let mut branches = Vec::new();
            for child in children {
                let mut compiled = compile_expr(child, dim, embed_text, lookup_file)?;
                branches.append(&mut compiled);
            }
            Ok(branches)
        }
        Expr::All(children) => {
            if children.is_empty() {
                bail!("all() requires at least one child");
            }
            let mut compiled_children = Vec::new();
            for child in children {
                compiled_children.push(compile_expr(child, dim, embed_text, lookup_file)?);
            }

            // Cartesian mean over branches of all children
            let mut combinations: Vec<Vec<&Affine>> = vec![vec![]];
            for child_branches in &compiled_children {
                let mut next = Vec::new();
                for comb in combinations {
                    for branch in child_branches {
                        let mut new_comb = comb.clone();
                        new_comb.push(branch);
                        next.push(new_comb);
                    }
                }
                combinations = next;
            }

            let count = children.len() as f32;
            let inv_count = 1.0 / count;
            let mut result = Vec::with_capacity(combinations.len());
            for comb in combinations {
                let bias: f32 = comb.iter().map(|b| b.bias).sum::<f32>() * inv_count;
                let mut vector = vec![0.0f32; dim];
                for branch in &comb {
                    for (i, v) in branch.vector.iter().enumerate() {
                        vector[i] += v;
                    }
                }
                for v in &mut vector {
                    *v *= inv_count;
                }
                result.push(Affine::new(bias, vector));
            }
            Ok(result)
        }
        Expr::Avoid(positive, negative) => {
            let pos_branches = compile_expr(positive, dim, embed_text, lookup_file)?;
            let neg_branches = compile_expr(negative, dim, embed_text, lookup_file)?;

            // Section 3 constraint: avoid's negative expression must compile to a single linear branch!
            if neg_branches.len() != 1 {
                bail!(
                    "InvalidNegativeExpression: avoid() second operand must be a single linear affine expression and cannot contain any(); compiled to {} branches",
                    neg_branches.len()
                );
            }

            let neg = &neg_branches[0];
            // avoid(A, B) = (A + 1 - B) / 2
            // S_avoid(x) = (bias_P - x^T v_P + 1 - (bias_N - x^T v_N)) / 2
            //            = (bias_P + 1 - bias_N)/2 - x^T (v_P - v_N)/2
            let mut result = Vec::with_capacity(pos_branches.len());
            for pos in pos_branches {
                let bias = (pos.bias + 1.0 - neg.bias) * 0.5;
                let vector: Vec<f32> = pos
                    .vector
                    .iter()
                    .zip(&neg.vector)
                    .map(|(p, n)| (p - n) * 0.5)
                    .collect();
                result.push(Affine::new(bias, vector));
            }
            Ok(result)
        }
    }
}

/// Compile a WQL expression into an optimized `CompiledQuery`.
pub fn compile_wql<E, F>(
    expr: &Expr,
    dim: usize,
    embed_text: &mut E,
    lookup_file: &mut F,
) -> Result<CompiledQuery>
where
    E: FnMut(&str) -> Result<Vec<f32>>,
    F: FnMut(&Path) -> Result<Vec<f32>>,
{
    let branches = compile_expr(expr, dim, embed_text, lookup_file)?;
    let mut compiled = CompiledQuery::new(branches);
    compiled.optimize();
    Ok(compiled)
}

// ---------------------------------------------------------------------------
// Direct AST Evaluator (for mathematical consistency testing)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub fn eval_ast_direct<E, F>(
    expr: &Expr,
    candidate: &[f32],
    embed_text: &mut E,
    lookup_file: &mut F,
) -> Result<f32>
where
    E: FnMut(&str) -> Result<Vec<f32>>,
    F: FnMut(&Path) -> Result<Vec<f32>>,
{
    match expr {
        Expr::Text(text) => {
            let vec = embed_text(text)?;
            let dot: f32 = candidate.iter().zip(&vec).map(|(a, b)| a * b).sum();
            Ok((1.0 - dot) * 0.5)
        }
        Expr::File(path) => {
            let vec = lookup_file(path)?;
            let dot: f32 = candidate.iter().zip(&vec).map(|(a, b)| a * b).sum();
            Ok((1.0 - dot) * 0.5)
        }
        Expr::All(children) => {
            let mut sum = 0.0;
            for child in children {
                sum += eval_ast_direct(child, candidate, embed_text, lookup_file)?;
            }
            Ok(sum / children.len() as f32)
        }
        Expr::Any(children) => {
            let mut min_val = f32::INFINITY;
            for child in children {
                let val = eval_ast_direct(child, candidate, embed_text, lookup_file)?;
                if val < min_val {
                    min_val = val;
                }
            }
            Ok(min_val)
        }
        Expr::Avoid(pos, neg) => {
            let pos_val = eval_ast_direct(pos, candidate, embed_text, lookup_file)?;
            let neg_val = eval_ast_direct(neg, candidate, embed_text, lookup_file)?;
            Ok((pos_val + 1.0 - neg_val) * 0.5)
        }
    }
}

// ---------------------------------------------------------------------------
// Execution & Response Models
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct WqlHit {
    #[serde(flatten)]
    pub hit: Hit,
    pub distance: f32,
    pub matched_branch: usize,
    pub source_state: String,
}

#[derive(Serialize)]
pub struct WqlResponse {
    pub query: String,
    pub compiled_branches: usize,
    pub level: Level,
    pub top: usize,
    pub evaluated_candidates: usize,
    pub timings_ms: WqlTimings,
    pub hits: Vec<WqlHit>,
}

/// Wall-clock phases of execute_wql, excluding CLI parsing/printing.
/// `compile_ms` excludes model calls and file-vector lookups; `scan_ms`
/// includes Top-K selection, and (for mmap) the Hit construction.
#[derive(Debug, Default, Serialize)]
pub struct WqlTimings {
    pub embedding_ms: f64,
    pub file_lookup_ms: f64,
    pub compile_ms: f64,
    pub scope_ms: f64,
    pub cache_load_ms: f64,
    pub scan_ms: f64,
    pub result_ms: f64,
    pub total_ms: f64,
    pub text_terms: usize,
    pub unique_text_terms: usize,
    pub reused_text_terms: usize,
}

/// Run a parsed WQL query against the store.
pub fn execute_wql<E>(
    query: &WqlQuery,
    store: &Store,
    dim: usize,
    enc: Encoding,
    embed_text: &mut E,
) -> Result<WqlResponse>
where
    E: FnMut(&str) -> Result<Vec<f32>>,
{
    let total_start = Instant::now();
    let mut timings = WqlTimings::default();
    let mut text_cache: HashMap<String, Vec<f32>> = HashMap::new();
    let mut cached_embed = |text: &str| -> Result<Vec<f32>> {
        timings.text_terms += 1;
        if let Some(vector) = text_cache.get(text) {
            timings.reused_text_terms += 1;
            return Ok(vector.clone());
        }
        let start = Instant::now();
        let vector = embed_text(text)?;
        timings.embedding_ms += start.elapsed().as_secs_f64() * 1000.0;
        timings.unique_text_terms += 1;
        text_cache.insert(text.to_owned(), vector.clone());
        Ok(vector)
    };
    let mut file_lookup_ms = 0.0;
    let mut lookup_file = |path: &Path| -> Result<Vec<f32>> {
        let start = Instant::now();
        let path_str = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let vector = store.document_vector(&path_str.to_string_lossy(), enc, dim);
        file_lookup_ms += start.elapsed().as_secs_f64() * 1000.0;
        vector
    };

    let compile_start = Instant::now();
    let compiled = compile_wql(&query.expr, dim, &mut cached_embed, &mut lookup_file)?;
    let compile_ms = compile_start.elapsed().as_secs_f64() * 1000.0;
    timings.compile_ms = (compile_ms - timings.embedding_ms - file_lookup_ms).max(0.0);
    timings.file_lookup_ms = file_lookup_ms;
    if compiled.branches.is_empty() {
        bail!("compiled query has no branches");
    }

    // Resolve scope / view: error out if view is not registered and does not exist on disk
    let scope_start = Instant::now();
    let target_under: Option<PathBuf> = if let Some(ref view_name) = query.view {
        if let Some(view_path) = store.get_view(view_name)? {
            Some(PathBuf::from(view_path))
        } else {
            let p = Path::new(view_name);
            if p.exists() {
                Some(p.canonicalize()?)
            } else {
                bail!(
                    "view '{}' is not registered and path does not exist; use 'weft view set {} <path>' to register it",
                    view_name,
                    view_name
                );
            }
        }
    } else {
        None
    };
    timings.scope_ms = scope_start.elapsed().as_secs_f64() * 1000.0;

    let level = query.level.unwrap_or(Level::File);
    let per_file = level == Level::File;
    let pool_limit = query.top.max(10) * 4;

    let mut evaluated = 0;
    let mut accept = |path: &str, _kind: &str| -> bool {
        if let Some(ref under) = target_under {
            Path::new(path).starts_with(under)
        } else {
            true
        }
    };

    let hits = if level == Level::Folder {
        // Hierarchy folder ranking
        let scan_start = Instant::now();
        let (records, _chunks, _count) = crate::hierarchy::rank(
            store,
            enc,
            dim,
            true,
            pool_limit,
            &mut accept,
            |v, _cutoff| {
                evaluated += 1;
                // Hierarchy ranking expects higher score = better, so we invert distance: -dist
                Some(-compiled.eval(v))
            },
        )?;
        timings.scan_ms = scan_start.elapsed().as_secs_f64() * 1000.0;
        let result_start = Instant::now();
        let hits = records
            .into_iter()
            .take(query.top)
            .map(|r| {
                let dist = -r.hit.score;
                WqlHit {
                    hit: r.hit,
                    distance: dist,
                    matched_branch: 0,
                    source_state: "aggregate_folder".into(),
                }
            })
            .collect();
        timings.result_ms = result_start.elapsed().as_secs_f64() * 1000.0;
        hits
    } else if enc == Encoding::Q8 {
        // Try the mmap snapshot first (zero-copy, no B-tree traversal).
        // Fall back to in-process FlatVectorCache (SQLite read) if the file
        // does not exist or is stale (first run after schema change, etc.).
        let load_start = Instant::now();
        let mmap_result = store.load_mmap_cache(dim);
        timings.cache_load_ms += load_start.elapsed().as_secs_f64() * 1000.0;
        let hits = if let Ok(mmap) = mmap_result {
            evaluated = mmap.count;
            let scan_start = Instant::now();
            let hits = if target_under.is_some() {
                mmap.scan_wql(&compiled, query.top, per_file, accept)
            } else {
                mmap.scan_wql_unfiltered(&compiled, query.top, per_file)
            };
            timings.scan_ms = scan_start.elapsed().as_secs_f64() * 1000.0;
            hits
        } else {
            let load_start = Instant::now();
            let flat_result = store.load_flat_cache(dim);
            timings.cache_load_ms += load_start.elapsed().as_secs_f64() * 1000.0;
            if let Ok(flat) = flat_result {
                evaluated = flat.count;
                let scan_start = Instant::now();
                let hits = flat.scan_wql(&compiled, query.top, per_file, accept);
                timings.scan_ms = scan_start.elapsed().as_secs_f64() * 1000.0;
                hits
            } else {
                Vec::new()
            }
        };
        let result_start = Instant::now();
        let hits = hits
            .into_iter()
            .map(|h| {
                let dist = -h.score;
                WqlHit {
                    hit: h,
                    distance: dist,
                    matched_branch: 0,
                    source_state: "indexed".into(),
                }
            })
            .collect();
        timings.result_ms = result_start.elapsed().as_secs_f64() * 1000.0;
        hits
    } else {
        // Chunk or per-file ranking fallback
        let scan_start = Instant::now();
        let raw_hits = store.rank_bounded(pool_limit, per_file, accept, |blob, _cutoff| {
            evaluated += 1;
            let vec = unpack_normalized(blob, enc, dim)?;
            let dist = compiled.eval(&vec);
            // store.rank_bounded orders descending by score. Invert distance: score = -dist
            Ok(Some(-dist))
        })?;
        timings.scan_ms = scan_start.elapsed().as_secs_f64() * 1000.0;

        let result_start = Instant::now();
        let mut out = Vec::new();
        for h in raw_hits.into_iter().take(query.top) {
            let dist = -h.score;
            let mut branch = 0;
            if let Ok((blob, _hash)) = store.chunk_record(&h.path, h.chunk_index.unwrap_or(0)) {
                if let Ok(vec) = unpack_normalized(&blob, enc, dim) {
                    let (_s, b_idx) = compiled.eval_with_branch(&vec);
                    branch = b_idx;
                }
            }
            out.push(WqlHit {
                hit: h,
                distance: dist,
                matched_branch: branch,
                source_state: "indexed".into(),
            });
        }
        timings.result_ms = result_start.elapsed().as_secs_f64() * 1000.0;
        out
    };

    timings.total_ms = total_start.elapsed().as_secs_f64() * 1000.0;
    Ok(WqlResponse {
        query: format!("{:?}", query.expr),
        compiled_branches: compiled.branches.len(),
        level,
        top: query.top,
        evaluated_candidates: evaluated,
        timings_ms: timings,
        hits,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn normalize(mut v: Vec<f32>) -> Vec<f32> {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    #[test]
    fn test_parser_basic_and_nested() {
        let q = parse_wql(r#"all("재시도","백오프") in loom top 10"#).unwrap();
        assert_eq!(
            q.expr,
            Expr::All(vec![
                Expr::Text("재시도".into()),
                Expr::Text("백오프".into())
            ])
        );
        assert_eq!(q.view, Some("loom".into()));
        assert_eq!(q.top, 10);
        assert_eq!(q.level, None);

        let q2 = parse_wql(
            r#"any( all("재시도","백오프"), avoid("모델 라우팅","테스트 구현") ) in loom top 10"#,
        )
        .unwrap();
        assert_eq!(
            q2.expr,
            Expr::Any(vec![
                Expr::All(vec![
                    Expr::Text("재시도".into()),
                    Expr::Text("백오프".into())
                ]),
                Expr::Avoid(
                    Box::new(Expr::Text("모델 라우팅".into())),
                    Box::new(Expr::Text("테스트 구현".into()))
                ),
            ])
        );

        let q3 = parse_wql(r#"file("/path/to/doc.md") level chunk top 5"#).unwrap();
        assert_eq!(q3.expr, Expr::File(PathBuf::from("/path/to/doc.md")));
        assert_eq!(q3.level, Some(Level::Chunk));
        assert_eq!(q3.top, 5);
    }

    #[test]
    fn test_avoid_with_any_is_rejected() {
        let q = parse_wql(r#"avoid("A", any("B", "C"))"#).unwrap();
        let mut mock_embed = |_: &str| Ok(normalize(vec![1.0, 0.0]));
        let mut mock_file = |_: &Path| Ok(normalize(vec![0.0, 1.0]));

        let res = compile_wql(&q.expr, 2, &mut mock_embed, &mut mock_file);
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("InvalidNegativeExpression"));
    }

    #[test]
    fn test_section_10_mathematical_exactness() {
        // Dimension = 4
        let dim = 4;
        let mut dict: HashMap<String, Vec<f32>> = HashMap::new();
        dict.insert("A".into(), normalize(vec![1.0, 0.2, -0.5, 0.1]));
        dict.insert("B".into(), normalize(vec![0.3, 0.9, 0.1, -0.4]));
        dict.insert("C".into(), normalize(vec![-0.2, 0.4, 0.8, 0.5]));
        dict.insert("D".into(), normalize(vec![0.5, -0.3, 0.2, 0.8]));

        let mut embed = |s: &str| Ok(dict.get(s).cloned().unwrap());
        let mut lookup_file = |_: &Path| Ok(vec![0.0; dim]);

        // 8 test queries from Section 10:
        let test_queries = [
            r#""A""#,
            r#"all("A","B")"#,
            r#"any("A","B")"#,
            r#"avoid("A","B")"#,
            r#"all(any("A","B"),"C")"#,
            r#"avoid(any("A","B"),"C")"#,
            r#"any(all("A","B"),avoid("C","D"))"#,
            r#"all(any("A","B"),any("C","D"))"#,
        ];

        // Generate 100 random unit candidates
        let mut rng_state: u64 = 123456789;
        let mut random_candidates = Vec::new();
        for _ in 0..100 {
            let mut v = Vec::with_capacity(dim);
            for _ in 0..dim {
                rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let val = ((rng_state >> 32) as i32 as f32) / (i32::MAX as f32);
                v.push(val);
            }
            random_candidates.push(normalize(v));
        }

        for query_str in &test_queries {
            let parsed = parse_wql(query_str).unwrap();
            let compiled = compile_wql(&parsed.expr, dim, &mut embed, &mut lookup_file).unwrap();

            for cand in &random_candidates {
                let direct_score =
                    eval_ast_direct(&parsed.expr, cand, &mut embed, &mut lookup_file).unwrap();
                let compiled_score = compiled.eval(cand);

                let diff = (direct_score - compiled_score).abs();
                assert!(
                    diff < 1e-5,
                    "Query '{}' failed on candidate {:?}: direct={}, compiled={}, diff={}",
                    query_str,
                    cand,
                    direct_score,
                    compiled_score,
                    diff
                );
            }
        }
    }

    #[test]
    fn test_domination_pruning_soundness() {
        // Construct two branches where branch 0 strictly dominates branch 1:
        // c0 = 0.3, v0 = [0.5, 0.5]
        // c1 = 0.8, v1 = [0.5, 0.5] -> diff_norm = 0, c1 - c0 = 0.5 >= 0 -> branch 1 is dominated!
        let b0 = Affine::new(0.3, vec![0.5, 0.5]);
        let b1 = Affine::new(0.8, vec![0.5, 0.5]);
        let mut q = CompiledQuery::new(vec![b0.clone(), b1]);
        assert_eq!(q.branches.len(), 2);
        q.optimize();
        assert_eq!(q.branches.len(), 1);
        assert_eq!(q.branches[0].bias, 0.3);

        // Another case: ||v1 - v0|| = 0.1, c1 - c0 = 0.2 >= 0.1
        let b0 = Affine::new(0.2, vec![1.0, 0.0]);
        let b1 = Affine::new(0.5, vec![0.9, 0.0]); // diff_norm = 0.1, bias diff = 0.3 >= 0.1
        let mut q2 = CompiledQuery::new(vec![b0.clone(), b1]);
        q2.optimize();
        assert_eq!(q2.branches.len(), 1);
        assert_eq!(q2.branches[0].bias, 0.2);
    }

    #[test]
    fn test_budget_branch_explosion() {
        let q_str = r#"all( any("A","B","C","D"), any("E","F","G","H"), any("I","J","K","L") )"#;
        let parsed = parse_wql(q_str).unwrap();

        let mut embed = |_: &str| Ok(vec![0.5, 0.5]);
        let mut lookup_file = |_: &Path| Ok(vec![0.5, 0.5]);

        let compiled = compile_wql(&parsed.expr, 2, &mut embed, &mut lookup_file).unwrap();
        // Since all vectors are identical, deduplication will collapse them, but unoptimized has 64 branches
        let raw_branches = compile_expr(&parsed.expr, 2, &mut embed, &mut lookup_file).unwrap();
        assert_eq!(raw_branches.len(), 64);
        let raw_query = CompiledQuery::new(raw_branches);
        assert!(raw_query.budget_exceeded());
        // Optimized collapses duplicates
        assert!(compiled.branches.len() <= 16);
    }

    #[test]
    fn test_wql_views_and_execution() {
        let dir = std::env::temp_dir().join(format!("weft-wql-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("index.sqlite");
        let store = Store::open(&db_path).unwrap();

        // Register a view: "myview" -> dir
        store
            .set_view("myview", &dir.to_string_lossy(), None)
            .unwrap();
        let retrieved = store.get_view("myview").unwrap();
        assert_eq!(retrieved, Some(dir.to_string_lossy().into_owned()));

        let views = store.list_views().unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].0, "myview");

        // Parse query with view
        let q = parse_wql(r#"all("foo", "bar") in myview top 5"#).unwrap();
        assert_eq!(q.view, Some("myview".into()));
        assert_eq!(q.top, 5);

        // Clean up
        store.delete_view("myview").unwrap();
        assert_eq!(store.get_view("myview").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_repeated_text_embeds_once_without_changing_compiled_scores() {
        let dir = std::env::temp_dir().join(format!(
            "weft-wql-cache-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("index.sqlite")).unwrap();
        let q = parse_wql(r#"any(all("same","other"),avoid("same","other"))"#).unwrap();
        let mut calls = Vec::new();
        let mut embed = |text: &str| -> Result<Vec<f32>> {
            calls.push(text.to_owned());
            Ok(if text == "same" {
                normalize(vec![0.8, 0.6])
            } else {
                normalize(vec![0.6, 0.8])
            })
        };
        let result = execute_wql(&q, &store, 2, Encoding::Q8, &mut embed).unwrap();
        assert_eq!(calls, vec!["same", "other"]);
        assert_eq!(result.timings_ms.text_terms, 4);
        assert_eq!(result.timings_ms.unique_text_terms, 2);
        assert_eq!(result.timings_ms.reused_text_terms, 2);

        let mut raw_embed = |text: &str| -> Result<Vec<f32>> {
            Ok(if text == "same" {
                normalize(vec![0.8, 0.6])
            } else {
                normalize(vec![0.6, 0.8])
            })
        };
        let compiled = compile_wql(&q.expr, 2, &mut raw_embed, &mut |_| unreachable!()).unwrap();
        for candidate in [normalize(vec![1.0, 1.0]), vec![1.0, 0.0]] {
            let direct =
                eval_ast_direct(&q.expr, &candidate, &mut raw_embed, &mut |_| unreachable!())
                    .unwrap();
            assert!((compiled.eval(&candidate) - direct).abs() < 1e-6);
        }
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
