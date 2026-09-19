// ast.rs — the C-subset syntax tree. The parser builds it; the emitter
// walks it. Positions are kept on nodes that can fail at emit time
// (variable resolution) or carry user-authored structure.

#[derive(Debug)]
pub struct Program {
    pub functions: Vec<Function>,
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug)]
pub enum Stmt {
    Decl(Vec<Declarator>),
    Return { value: Expr, line: usize, col: usize },
    If { cond: Expr, then: Box<Stmt>, else_: Option<Box<Stmt>> },
    While { cond: Expr, body: Box<Stmt> },
    DoWhile { body: Box<Stmt>, cond: Expr },
    For { init: Option<ForInit>, cond: Option<Expr>, post: Option<Expr>, body: Box<Stmt> },
    Break { line: usize, col: usize },
    Continue { line: usize, col: usize },
    Block(Vec<Stmt>),
    Expr(Expr),
    Empty,
}

impl Stmt {
    /// True when any `return` is reachable inside this statement's subtree —
    /// the emitter must guard the remainder of the enclosing statement list
    /// behind the early-return flag after such a statement.
    pub fn contains_return(&self) -> bool {
        match self {
            Stmt::Return { .. } => true,
            Stmt::If { then, else_, .. } => then.contains_return() || else_.as_ref().is_some_and(|s| s.contains_return()),
            Stmt::While { body, .. } | Stmt::DoWhile { body, .. } | Stmt::For { body, .. } => body.contains_return(),
            Stmt::Block(stmts) => stmts.iter().any(Stmt::contains_return),
            _ => false,
        }
    }
}

#[derive(Debug)]
pub enum ForInit {
    Decl(Declarator),
    Expr(Expr),
}

#[derive(Debug)]
pub struct Declarator {
    pub name: String,
    pub init: Option<Expr>,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug)]
pub enum ExprKind {
    /// Integer literal; raw source text (decimal or 0x hex).
    Int(String),
    /// String literal; raw source text including quotes.
    Str(String),
    /// Character literal; raw source text including quotes.
    Char(String),
    /// Variable reference by C name.
    Var(String),
    /// Assignment: plain (`op == None`) or compound. Value of the
    /// assignment is the stored value (pass-through store).
    Assign { name: String, op: Option<BinOp>, value: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Logical { op: LogicOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Unary { op: UnOp, operand: Box<Expr> },
    /// Prefix ++/-- on a variable. Expression value is the NEW value.
    Pre { name: String, delta: i64 },
    /// Postfix ++/-- on a variable. Expression value is the OLD value.
    Post { name: String, delta: i64 },
    Call { name: String, args: Vec<Expr> },
    /// `name[index]` byte load (char* indexing).
    Index { name: String, index: Box<Expr> },
    Argc,
    /// One of the C standard streams ("stdout"/"stdin"/"stderr").
    Stream(&'static str),
    Argv(Box<Expr>),
    Byte(Box<Expr>),
    Null,
    EofLit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    BitAnd,
    BitXor,
    BitOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}
