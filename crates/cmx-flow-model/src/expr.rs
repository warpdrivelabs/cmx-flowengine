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
 *   primary := number | string | bool | null | call | ident | "(" expr ")"
 *   call    := ident "(" ( expr ( "," expr )* )? ")"
 *
 * 变量引用即裸标识符（如 `amount`、`approved`）。外层可包 `${ ... }`，会被剥离。
 * 求值结果按 JSON 真值语义归一为 bool 返回给引擎。
 *
 * 【嵌套路径】标识符可含点号（`order.amount`）：**先按整名做精确 key 查找**（后向
 * 兼容——历史上平坦 key 就叫 `order.amount` 时仍命中），未命中再按点号逐段下钻对象/
 * 数组（`order` 对象取 `amount` 字段、`items.0` 取数组首元素）。二者叠加，旧流程零回归。
 *
 * 【内置函数】纯函数库（字符串/数值/逻辑/集合），见 `call_builtin`。**时间类函数
 * （NOW/DAYS/AGE）刻意不在此**：引擎的"当前时间"经可注入 Clock 提供（见 M2.5），
 * 纯表达式层拿不到也不应硬编码系统时钟——需要时另走注入通道，勿在此模块引入不确定性。
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

/// 校验一条条件表达式的**语法**（不求值、不需变量环境）。
///
/// 设计器「保存前校验」分支条件用：只回答「这串能不能被解析」，Ok(()) = 语法合法。
/// 空表达式合法（= 无条件边）。注意：语法合法不代表运行必为真——变量缺失/类型不符是运行时语义。
pub fn validate_syntax(expr: &str) -> Result<()> {
    let src = strip_wrapper(expr.trim());
    if src.is_empty() {
        return Ok(());
    }
    let tokens = tokenize(src)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
    };
    parser.parse_expr()?;
    parser.expect_end()?;
    Ok(())
}

/// 求值一条表达式为 `Value`（非布尔）。空表达式 → `Null`。
///
/// 与 `eval_condition` 同一解析/求值内核，只是不做 `truthy` 归一——供「多实例逐元素办理人」等
/// 需要拿到具体值（字符串/数字）的场景（②）。
pub fn eval_value(expr: &str, vars: &Variables) -> Result<Value> {
    let src = strip_wrapper(expr.trim());
    if src.is_empty() {
        return Ok(Value::Null);
    }
    let tokens = tokenize(src)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let ast = parser.parse_expr()?;
    parser.expect_end()?;
    eval(&ast, vars)
}

/// 模板插值：把 `template` 里每个 `${expr}` 求值并字符串化后替换；无 `${}` 则**原样返回**
/// （静态字面量零改动，保证老流程后向兼容）。
///
/// 整串即一个表达式（`${approver}`）与嵌入式（`user_${product.id}`）都支持。
/// `${}` 内含 `}` 字面量的场景不支持（条件表达式子集本就不产生 `}`），遇缺闭合报语法错。
/// Value 字符串化见 [`value_to_string`]。
pub fn interpolate(template: &str, vars: &Variables) -> Result<String> {
    if !template.contains("${") {
        return Ok(template.to_string());
    }
    let mut out = String::new();
    let mut rest = template;
    while let Some(pos) = rest.find("${") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 2..];
        let close = after
            .find('}')
            .ok_or_else(|| Error::ExprSyntax("插值 ${...} 缺少闭合 }".into()))?;
        let inner = &after[..close];
        let v = eval_value(inner, vars)?;
        out.push_str(&value_to_string(&v));
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 把求值出的 `Value` 转成办理人/文本字符串：字符串取本身，数字/布尔取字面量，`null` → 空串；
/// 数组/对象 → 空串（办理人不应是复合值，宽容不报错，由上层按「空 = 无此人」兜底）。
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// 内置函数目录项（供前端「函数列」下拉：名称 + 元数（参数个数，None=可变）+ 分类 + 说明）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BuiltinFn {
    /// 函数名（大写规范形）。
    pub name: &'static str,
    /// 分类（string / collection / number / logic）。
    pub category: &'static str,
    /// 固定参数个数；`None` 表示可变参数。
    pub arity: Option<usize>,
    /// 一句话说明（中文，直接给设计器 tooltip）。
    pub desc: &'static str,
}

/// 内置函数目录：条件构造器「函数列」的权威来源（契约驱动——前端下拉即读此表，勿另维护）。
///
/// 与 `call_builtin` 的分支一一对应；新增函数须两处同步（编译期无法强约束，靠此注释 + 测试兜底）。
pub fn builtin_catalog() -> &'static [BuiltinFn] {
    &[
        BuiltinFn { name: "LEN", category: "string", arity: Some(1), desc: "字符串/数组/对象长度" },
        BuiltinFn { name: "UPPER", category: "string", arity: Some(1), desc: "转大写" },
        BuiltinFn { name: "LOWER", category: "string", arity: Some(1), desc: "转小写" },
        BuiltinFn { name: "TRIM", category: "string", arity: Some(1), desc: "去首尾空白" },
        BuiltinFn { name: "STARTSWITH", category: "string", arity: Some(2), desc: "是否以某前缀开头" },
        BuiltinFn { name: "ENDSWITH", category: "string", arity: Some(2), desc: "是否以某后缀结尾" },
        BuiltinFn { name: "CONTAINS", category: "collection", arity: Some(2), desc: "字符串含子串 / 数组含元素" },
        BuiltinFn { name: "IN", category: "collection", arity: None, desc: "值是否等于后续任一参数" },
        BuiltinFn { name: "IS_EMPTY", category: "collection", arity: Some(1), desc: "为空(null/空串/空数组/空对象)" },
        BuiltinFn { name: "ABS", category: "number", arity: Some(1), desc: "绝对值" },
        BuiltinFn { name: "ROUND", category: "number", arity: Some(1), desc: "四舍五入" },
        BuiltinFn { name: "FLOOR", category: "number", arity: Some(1), desc: "向下取整" },
        BuiltinFn { name: "CEIL", category: "number", arity: Some(1), desc: "向上取整" },
        BuiltinFn { name: "MIN", category: "number", arity: None, desc: "最小值(可变参数)" },
        BuiltinFn { name: "MAX", category: "number", arity: None, desc: "最大值(可变参数)" },
        BuiltinFn { name: "COALESCE", category: "logic", arity: None, desc: "取第一个非空参数" },
        BuiltinFn { name: "IF", category: "logic", arity: Some(3), desc: "IF(条件,则,否)" },
        BuiltinFn { name: "NOT", category: "logic", arity: Some(1), desc: "逻辑取反" },
    ]
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
    Comma,
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
            ',' => {
                tokens.push(Token::Comma);
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
    Call(String, Vec<Ast>),
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
            Some(Token::Ident(name)) => {
                let name = name.clone();
                // ident 后紧跟 '(' → 函数调用；否则是变量引用。
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.advance(); // 吃掉 '('
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(Token::RParen)) {
                        loop {
                            args.push(self.parse_expr()?);
                            match self.peek() {
                                Some(Token::Comma) => {
                                    self.advance();
                                }
                                _ => break,
                            }
                        }
                    }
                    match self.advance() {
                        Some(Token::RParen) => Ok(Ast::Call(name, args)),
                        _ => Err(Error::ExprSyntax(format!(
                            "函数 '{name}' 参数列表未闭合，缺少 ')'"
                        ))),
                    }
                } else {
                    Ok(Ast::Var(name))
                }
            }
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
        Ast::Var(name) => Ok(lookup_var(name, vars)),
        Ast::Call(name, args) => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval(a, vars)?);
            }
            call_builtin(name, &vals)
        }
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
            // null/未赋值参与数值比较 → false（而非报错）：贴合审批语义「缺数据即条件不满足」，
            // 也让「决策未写变量 → 后续网关不误抛」。两侧都非 null 时才做数值比较。
            if lv.is_null() || rv.is_null() {
                return Ok(Value::Bool(false));
            }
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

/// 变量查找：先整名精确匹配（后向兼容平坦 key），未命中再按点号逐段下钻。
///
/// - `order.amount`：若存在名为 "order.amount" 的平坦变量则直接返回它；
///   否则取 `order` 对象的 `amount` 字段。
/// - `items.0`：数组下标下钻（`items` 为数组时取第 0 元素）。
/// - 任一段缺失 → `Null`（对齐原「变量缺失即 Null」语义，不报错）。
fn lookup_var(name: &str, vars: &Variables) -> Value {
    // ① 整名精确命中（历史平坦 key 语义，零回归）。
    if let Some(v) = vars.get(name) {
        return v.clone();
    }
    // ② 无点号且未命中 → Null（与原行为一致）。
    if !name.contains('.') {
        return Value::Null;
    }
    // ③ 点号下钻：首段查顶层变量，其余段在 JSON 内逐层深入。
    let mut segs = name.split('.');
    let head = match segs.next() {
        Some(h) => h,
        None => return Value::Null,
    };
    let mut cur = match vars.get(head) {
        Some(v) => v.clone(),
        None => return Value::Null,
    };
    for seg in segs {
        cur = descend(&cur, seg);
        if cur.is_null() {
            return Value::Null;
        }
    }
    cur
}

/// 在一个 JSON 值内按单段路径下钻：对象取字段、数组按数字下标取元素，否则 Null。
fn descend(value: &Value, seg: &str) -> Value {
    match value {
        Value::Object(map) => map.get(seg).cloned().unwrap_or(Value::Null),
        Value::Array(arr) => seg
            .parse::<usize>()
            .ok()
            .and_then(|i| arr.get(i))
            .cloned()
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// 内置函数库：纯函数（无副作用、不读系统时钟）。参数已求值为 JSON 值。
///
/// 覆盖字符串 / 数值 / 逻辑 / 集合四类常用算子，供分支条件可视化构造器的「函数列」下拉。
/// 未知函数名 → 语法错误（设计器只应产出白名单内的函数）。
fn call_builtin(name: &str, args: &[Value]) -> Result<Value> {
    let arity = |n: usize| -> Result<()> {
        if args.len() == n {
            Ok(())
        } else {
            Err(Error::ExprEval(format!(
                "函数 {name} 需要 {n} 个参数，实际 {}",
                args.len()
            )))
        }
    };
    match name.to_ascii_uppercase().as_str() {
        // —— 字符串 ——
        "LEN" | "LENGTH" => {
            arity(1)?;
            let n = match &args[0] {
                Value::String(s) => s.chars().count(),
                Value::Array(a) => a.len(),
                Value::Object(o) => o.len(),
                Value::Null => 0,
                other => to_plain_string(other).chars().count(),
            };
            Ok(Value::from(n))
        }
        "UPPER" => {
            arity(1)?;
            Ok(Value::String(to_plain_string(&args[0]).to_uppercase()))
        }
        "LOWER" => {
            arity(1)?;
            Ok(Value::String(to_plain_string(&args[0]).to_lowercase()))
        }
        "TRIM" => {
            arity(1)?;
            Ok(Value::String(to_plain_string(&args[0]).trim().to_string()))
        }
        "STARTSWITH" => {
            arity(2)?;
            Ok(Value::Bool(
                to_plain_string(&args[0]).starts_with(&to_plain_string(&args[1])),
            ))
        }
        "ENDSWITH" => {
            arity(2)?;
            Ok(Value::Bool(
                to_plain_string(&args[0]).ends_with(&to_plain_string(&args[1])),
            ))
        }
        // —— 集合 / 包含 ——
        // CONTAINS(haystack, needle)：字符串子串，或数组含某元素。
        "CONTAINS" => {
            arity(2)?;
            let found = match &args[0] {
                Value::Array(a) => a.iter().any(|e| json_eq(e, &args[1])),
                Value::String(s) => s.contains(&to_plain_string(&args[1])),
                Value::Object(o) => o.contains_key(&to_plain_string(&args[1])),
                _ => false,
            };
            Ok(Value::Bool(found))
        }
        // IN(needle, a, b, c...)：needle 是否等于后续任一参数。
        "IN" => {
            if args.is_empty() {
                return Err(Error::ExprEval("函数 IN 至少需要 1 个参数".into()));
            }
            let needle = &args[0];
            Ok(Value::Bool(args[1..].iter().any(|e| json_eq(e, needle))))
        }
        "IS_EMPTY" | "ISEMPTY" => {
            arity(1)?;
            let empty = match &args[0] {
                Value::Null => true,
                Value::String(s) => s.is_empty(),
                Value::Array(a) => a.is_empty(),
                Value::Object(o) => o.is_empty(),
                _ => false,
            };
            Ok(Value::Bool(empty))
        }
        // —— 数值 ——
        "ABS" => {
            arity(1)?;
            Ok(Value::from(as_number(&args[0])?.abs()))
        }
        "ROUND" => {
            arity(1)?;
            Ok(Value::from(as_number(&args[0])?.round()))
        }
        "FLOOR" => {
            arity(1)?;
            Ok(Value::from(as_number(&args[0])?.floor()))
        }
        "CEIL" | "CEILING" => {
            arity(1)?;
            Ok(Value::from(as_number(&args[0])?.ceil()))
        }
        "MIN" => {
            if args.is_empty() {
                return Err(Error::ExprEval("函数 MIN 至少需要 1 个参数".into()));
            }
            let mut m = f64::INFINITY;
            for a in args {
                m = m.min(as_number(a)?);
            }
            Ok(Value::from(m))
        }
        "MAX" => {
            if args.is_empty() {
                return Err(Error::ExprEval("函数 MAX 至少需要 1 个参数".into()));
            }
            let mut m = f64::NEG_INFINITY;
            for a in args {
                m = m.max(as_number(a)?);
            }
            Ok(Value::from(m))
        }
        // —— 逻辑 ——
        // COALESCE(a, b, ...)：返回第一个非 null 参数。
        "COALESCE" => {
            for a in args {
                if !a.is_null() {
                    return Ok(a.clone());
                }
            }
            Ok(Value::Null)
        }
        // IF(cond, then, else)：三目。
        "IF" => {
            arity(3)?;
            Ok(if truthy(&args[0]) {
                args[1].clone()
            } else {
                args[2].clone()
            })
        }
        "NOT" => {
            arity(1)?;
            Ok(Value::Bool(!truthy(&args[0])))
        }
        other => Err(Error::ExprSyntax(format!("未知函数 '{other}'"))),
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
