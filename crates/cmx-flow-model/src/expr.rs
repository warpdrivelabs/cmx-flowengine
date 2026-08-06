/*
 * @Describe: 受控条件表达式求值器（自包含，零外部依赖）。
 *
 * 为什么手写而不是直接上 rhai：Java 引擎重度依赖 JUEL/Groovy/SpEL 写 sequenceFlow 条件，
 * 这是移植的最大隐藏成本。M1 的策略是「先用一个受控 DSL 覆盖 95% 的审批流条件」，把
 * 表达式能力**收敛**成引擎可控、可静态校验、无沙箱风险的子集；日后要更强的能力，只需
 * 替换本模块内部实现（对外仅暴露 `eval_condition`），引擎其余部分零改动。
 *
 * 支持文法（BPMN 条件的常见形态）：
 *   expr    := or
 *   or      := and ( "||" and )*
 *   and     := cmp ( "&&" cmp )*
 *   cmp     := add ( ("=="|"!="|"<"|"<="|">"|">=") add )?
 *   add     := mul ( ("+"|"-") mul )*
 *   mul     := unary ( ("*"|"/") unary )*
 *   unary   := "!" unary | "-" unary | primary
 *   primary := number | string | bool | null | ident | "(" expr ")"
 *
 * 变量引用即裸标识符（如 `amount`、`approved`）。外层可包 `${ ... }`，会被剥离。
 * 求值结果按 JSON 真值语义归一为 bool 返回给引擎。
 */

use serde_json::Value;

use crate::error::{Error, Result};
use crate::variables::Variables;

/// 对一条条件表达式求值，返回布尔判定。
///
/// - `expr`：条件源文本，可含或不含 `${...}` 包裹。
/// - `vars`：当前实例变量，作为标识符的取值环境。
///
/// 空表达式视为「无条件」→ `true`（对齐 BPMN：无 conditionExpression 的边恒可走）。
pub fn eval_condition(expr: &str, vars: &Variables) -> Result<bool> {
    let src = strip_wrapper(expr.trim());
    if src.is_empty() {
        return Ok(true);
    }
    let tokens = tokenize(src)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let ast = parser.parse_expr()?;
    parser.expect_end()?;
    let value = eval(&ast, vars)?;
    Ok(truthy(&value))
}

/// 剥离 `${ ... }` 或 `#{ ... }` 包裹（Flowable/JUEL 风格），无包裹则原样返回。
fn strip_wrapper(s: &str) -> &str {
    let bytes = s.as_bytes();
    if s.len() >= 3
        && (bytes[0] == b'$' || bytes[0] == b'#')
        && bytes[1] == b'{'
        && bytes[s.len() - 1] == b'}'
    {
        s[2..s.len() - 1].trim()
    } else {
        s
    }
}

// ============================ 词法 ============================

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
    Ident(String),
    // 运算符
    AndAnd,
    OrOr,
    Not,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

fn tokenize(src: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = src.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            '/' => {
                tokens.push(Token::Slash);
                i += 1;
            }
            '&' => {
                if next_is(&chars, i, '&') {
                    tokens.push(Token::AndAnd);
                    i += 2;
                } else {
                    return Err(Error::ExprSyntax("单个 '&' 非法，应为 '&&'".into()));
                }
            }
            '|' => {
                if next_is(&chars, i, '|') {
                    tokens.push(Token::OrOr);
                    i += 2;
                } else {
                    return Err(Error::ExprSyntax("单个 '|' 非法，应为 '||'".into()));
                }
            }
            '!' => {
                if next_is(&chars, i, '=') {
                    tokens.push(Token::NotEq);
                    i += 2;
                } else {
                    tokens.push(Token::Not);
                    i += 1;
                }
            }
            '=' => {
                if next_is(&chars, i, '=') {
                    tokens.push(Token::EqEq);
                    i += 2;
                } else {
                    return Err(Error::ExprSyntax("单个 '=' 非法，比较用 '=='".into()));
                }
            }
            '<' => {
                if next_is(&chars, i, '=') {
                    tokens.push(Token::LtEq);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
            }
            '>' => {
                if next_is(&chars, i, '=') {
                    tokens.push(Token::GtEq);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
            }
            '\'' | '"' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    // 简单转义：\' \" \\ \n \t
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        let esc = chars[i + 1];
                        s.push(match esc {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        });
                        i += 2;
                    } else {
                        s.push(chars[i]);
                        i += 1;
                    }
                }
                if i >= chars.len() {
                    return Err(Error::ExprSyntax("字符串字面量未闭合".into()));
                }
                i += 1; // 跳过右引号
                tokens.push(Token::Str(s));
            }
            c if c.is_ascii_digit() => {
                let start = i;
                let mut seen_dot = false;
                while i < chars.len()
                    && (chars[i].is_ascii_digit() || (chars[i] == '.' && !seen_dot))
                {
                    if chars[i] == '.' {
                        seen_dot = true;
                    }
                    i += 1;
                }
                let num_str: String = chars[start..i].iter().collect();
                let num = num_str
                    .parse::<f64>()
                    .map_err(|e| Error::ExprSyntax(format!("非法数字 '{num_str}': {e}")))?;
                tokens.push(Token::Number(num));
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match word.as_str() {
                    "true" => tokens.push(Token::Bool(true)),
                    "false" => tokens.push(Token::Bool(false)),
                    "null" => tokens.push(Token::Null),
                    "and" => tokens.push(Token::AndAnd),
                    "or" => tokens.push(Token::OrOr),
                    "not" => tokens.push(Token::Not),
                    _ => tokens.push(Token::Ident(word)),
                }
            }
            other => {
                return Err(Error::ExprSyntax(format!("无法识别的字符 '{other}'")));
            }
        }
    }
    Ok(tokens)
}

fn next_is(chars: &[char], i: usize, expect: char) -> bool {
    i + 1 < chars.len() && chars[i + 1] == expect
}

// ============================ 语法 ============================

#[derive(Debug, Clone)]
enum Ast {
    Lit(Value),
    Var(String),
    Unary(UnOp, Box<Ast>),
    Binary(BinOp, Box<Ast>, Box<Ast>),
}

#[derive(Debug, Clone, Copy)]
enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy)]
enum BinOp {
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expect_end(&self) -> Result<()> {
        if self.pos == self.tokens.len() {
            Ok(())
        } else {
            Err(Error::ExprSyntax(format!(
                "表达式尾部有多余 token: {:?}",
                self.tokens.get(self.pos)
            )))
        }
    }

    fn parse_expr(&mut self) -> Result<Ast> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Ast> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::OrOr)) {
            self.advance();
            let right = self.parse_and()?;
            left = Ast::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Ast> {
        let mut left = self.parse_cmp()?;
        while matches!(self.peek(), Some(Token::AndAnd)) {
            self.advance();
            let right = self.parse_cmp()?;
            left = Ast::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Ast> {
        let left = self.parse_add()?;
        let op = match self.peek() {
            Some(Token::EqEq) => BinOp::Eq,
            Some(Token::NotEq) => BinOp::Ne,
            Some(Token::Lt) => BinOp::Lt,
            Some(Token::LtEq) => BinOp::Le,
            Some(Token::Gt) => BinOp::Gt,
            Some(Token::GtEq) => BinOp::Ge,
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_add()?;
        Ok(Ast::Binary(op, Box::new(left), Box::new(right)))
    }

    fn parse_add(&mut self) -> Result<Ast> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => BinOp::Add,
                Some(Token::Minus) => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_mul()?;
            left = Ast::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Ast> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => BinOp::Mul,
                Some(Token::Slash) => BinOp::Div,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Ast::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Ast> {
        match self.peek() {
            Some(Token::Not) => {
                self.advance();
                Ok(Ast::Unary(UnOp::Not, Box::new(self.parse_unary()?)))
            }
            Some(Token::Minus) => {
                self.advance();
                Ok(Ast::Unary(UnOp::Neg, Box::new(self.parse_unary()?)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Ast> {
        match self.advance() {
            Some(Token::Number(n)) => Ok(Ast::Lit(Value::from(*n))),
            Some(Token::Str(s)) => Ok(Ast::Lit(Value::String(s.clone()))),
            Some(Token::Bool(b)) => Ok(Ast::Lit(Value::Bool(*b))),
            Some(Token::Null) => Ok(Ast::Lit(Value::Null)),
            Some(Token::Ident(name)) => Ok(Ast::Var(name.clone())),
            Some(Token::LParen) => {
                let inner = self.parse_expr()?;
                match self.advance() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(Error::ExprSyntax("括号未闭合，缺少 ')'".into())),
                }
            }
            other => Err(Error::ExprSyntax(format!(
                "期望一个操作数，实际为: {other:?}"
            ))),
        }
    }
}

// ============================ 求值 ============================

fn eval(ast: &Ast, vars: &Variables) -> Result<Value> {
    match ast {
        Ast::Lit(v) => Ok(v.clone()),
        Ast::Var(name) => Ok(vars.get(name).cloned().unwrap_or(Value::Null)),
        Ast::Unary(op, inner) => {
            let v = eval(inner, vars)?;
            match op {
                UnOp::Not => Ok(Value::Bool(!truthy(&v))),
                UnOp::Neg => {
                    let n = as_number(&v)?;
                    Ok(Value::from(-n))
                }
            }
        }
        Ast::Binary(op, l, r) => eval_binary(*op, l, r, vars),
    }
}

fn eval_binary(op: BinOp, l: &Ast, r: &Ast, vars: &Variables) -> Result<Value> {
    // 逻辑运算短路。
    match op {
        BinOp::And => {
            let lv = eval(l, vars)?;
            if !truthy(&lv) {
                return Ok(Value::Bool(false));
            }
            return Ok(Value::Bool(truthy(&eval(r, vars)?)));
        }
        BinOp::Or => {
            let lv = eval(l, vars)?;
            if truthy(&lv) {
                return Ok(Value::Bool(true));
            }
            return Ok(Value::Bool(truthy(&eval(r, vars)?)));
        }
        _ => {}
    }

    let lv = eval(l, vars)?;
    let rv = eval(r, vars)?;
    match op {
        BinOp::Eq => Ok(Value::Bool(json_eq(&lv, &rv))),
        BinOp::Ne => Ok(Value::Bool(!json_eq(&lv, &rv))),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let a = as_number(&lv)?;
            let b = as_number(&rv)?;
            let res = match op {
                BinOp::Lt => a < b,
                BinOp::Le => a <= b,
                BinOp::Gt => a > b,
                BinOp::Ge => a >= b,
                _ => unreachable!(),
            };
            Ok(Value::Bool(res))
        }
        BinOp::Add => {
            // 数字相加；任一为字符串则做拼接（宽容，便于 assignee 拼装类场景）。
            if lv.is_string() || rv.is_string() {
                Ok(Value::String(format!(
                    "{}{}",
                    to_plain_string(&lv),
                    to_plain_string(&rv)
                )))
            } else {
                Ok(Value::from(as_number(&lv)? + as_number(&rv)?))
            }
        }
        BinOp::Sub => Ok(Value::from(as_number(&lv)? - as_number(&rv)?)),
        BinOp::Mul => Ok(Value::from(as_number(&lv)? * as_number(&rv)?)),
        BinOp::Div => {
            let b = as_number(&rv)?;
            if b == 0.0 {
                return Err(Error::ExprEval("除数为零".into()));
            }
            Ok(Value::from(as_number(&lv)? / b))
        }
        BinOp::And | BinOp::Or => unreachable!("已在上方短路处理"),
    }
}

/// JSON 真值语义：null/false/0/""/空数组空对象 → false，其余 → true。
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// 相等比较：数字按数值比，其余按 JSON 结构比。
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .zip(y.as_f64())
            .map(|(p, q)| p == q)
            .unwrap_or(false),
        _ => a == b,
    }
}

/// 取数字，失败即报求值错误（比较/算术要求数值操作数）。
fn as_number(v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| Error::ExprEval("数字无法转为 f64".into())),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| Error::ExprEval(format!("字符串 '{s}' 不是数字"))),
        Value::Null => Err(Error::ExprEval("null 不能参与数值运算".into())),
        other => Err(Error::ExprEval(format!("{other:?} 不是数值"))),
    }
}

/// 无引号地把标量转字符串（用于字符串拼接）。
fn to_plain_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
