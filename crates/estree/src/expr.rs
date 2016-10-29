use serde_json::value::Value;
use easter::expr::Expr;
use easter::obj::DotKey;
use easter::id::IdExt;
use easter::punc::{Unop, Binop, Assop, Logop};
use unjson::ty::{Object, TyOf};
use unjson::ExtractField;
use joker::token::RegExpLiteral;

use tag::{Tag, TagOf};
use id::IntoId;
use result::Result;
use error::{Error, string_error, node_type_error, type_error};
use node::ExtractNode;
use fun::IntoFun;
use lit::{IntoStringLiteral, IntoNumberLiteral};

pub trait IntoExpr {
    fn into_expr(self) -> Result<Expr>;
    fn into_lit(self) -> Result<Expr>;
}

impl IntoExpr for Object {
    fn into_expr(mut self) -> Result<Expr> {
        let tag = try!(self.tag());
        Ok(match tag {
            Tag::Identifier => { return Ok(try!(self.into_id()).into_expr()); }
            Tag::Literal => try!(IntoExpr::into_lit(self)),
            Tag::BinaryExpression => {
                let str = try!(self.extract_string("operator").map_err(Error::Json));
                let op: Binop = match str.parse() {
                    Ok(op) => op,
                    Err(_) => { return string_error("binary operator", str); }
                };
                let left = try!(self.extract_expr("left"));
                let right = try!(self.extract_expr("right"));
                Expr::Binop(None, op, Box::new(left), Box::new(right))
            }
            Tag::AssignmentExpression => {
                let str = try!(self.extract_string("operator").map_err(Error::Json));
                let right = Box::new(try!(self.extract_expr("right")));
                match &str[..] {
                    "=" => Expr::Assign(None, try!(self.extract_assign_patt("left")), right),
                    _ => {
                        let op: Assop = match str.parse() {
                            Ok(op) => op,
                            Err(_) => { return string_error("assignment operator", str); }
                        };
                        Expr::BinAssign(None, op, try!(self.extract_assign_target("left")), right)
                    }
                }
            }
            Tag::LogicalExpression => {
                let str = try!(self.extract_string("operator").map_err(Error::Json));
                let op: Logop = match str.parse() {
                    Ok(op) => op,
                    Err(_) => { return string_error("logical operator", str); }
                };
                let left = try!(self.extract_expr("left"));
                let right = try!(self.extract_expr("right"));
                Expr::Logop(None, op, Box::new(left), Box::new(right))
            }
            Tag::UnaryExpression => {
                let str = try!(self.extract_string("operator").map_err(Error::Json));
                let op: Unop = match str.parse() {
                    Ok(op) => op,
                    Err(_) => { return string_error("unary operator", str); }
                };
                let arg = try!(self.extract_expr("argument"));
                Expr::Unop(None, op, Box::new(arg))
            }
            Tag::UpdateExpression => {
                let op = try!(self.extract_string("operator").map_err(Error::Json));
                let arg = Box::new(try!(self.extract_assign_target("argument")));
                let prefix = try!(self.extract_bool("prefix").map_err(Error::Json));
                match (&op[..], prefix) {
                    ("++", true)  => Expr::PreInc(None, arg),
                    ("++", false) => Expr::PostInc(None, arg),
                    ("--", true)  => Expr::PreDec(None, arg),
                    ("--", false) => Expr::PostDec(None, arg),
                    _ => { return string_error("'++' or '--'", op); }
                }
            }
            Tag::MemberExpression => {
                let obj = Box::new(try!(self.extract_expr("object")));
                if try!(self.extract_bool("computed").map_err(Error::Json)) {
                    let prop = Box::new(try!(self.extract_expr("property")));
                    Expr::Brack(None, obj, prop)
                } else {
                    let id = try!(try!(self.extract_object("property").map_err(Error::Json)).into_id());
                    let key = DotKey { location: None, value: id.name.into_string() };
                    Expr::Dot(None, obj, key)
                }
            }
            Tag::CallExpression => {
                let callee = Box::new(try!(self.extract_expr("callee")));
                let args = try!(self.extract_expr_list("arguments"));
                Expr::Call(None, callee, args)
            }
            Tag::NewExpression => {
                let callee = Box::new(try!(self.extract_expr("callee")));
                let args = try!(self.extract_expr_list("arguments"));
                Expr::New(None, callee, Some(args))
            }
            Tag::ArrayExpression => {
                let elts = try!(self.extract_expr_opt_list("elements"));
                Expr::Arr(None, elts)
            }
            Tag::FunctionExpression => {
                let fun = try!(self.into_fun());
                Expr::Fun(fun)
            }
            Tag::SequenceExpression => {
                let exprs = try!(self.extract_expr_list("expressions"));
                Expr::Seq(None, exprs)
            }
            Tag::ObjectExpression => {
                let props = try!(self.extract_prop_list("properties"));
                Expr::Obj(None, props)
            }
            Tag::ConditionalExpression => {
                let test = Box::new(try!(self.extract_expr("test")));
                let cons = Box::new(try!(self.extract_expr("consequent")));
                let alt = Box::new(try!(self.extract_expr("alternate")));
                Expr::Cond(None, test, cons, alt)
            }
            Tag::ThisExpression => Expr::This(None),
            _ => { return node_type_error("expression", tag); }
        })
    }

    fn into_lit(mut self) -> Result<Expr> {
        let json = try!(self.extract_field("value").map_err(Error::Json));
        Ok(match json {
            Value::Null if !self.contains_key("regex") => Expr::Null(None),
            Value::Bool(true) => Expr::True(None),
            Value::Bool(false) => Expr::False(None),
            Value::String(value) => Expr::String(None, value.into_string_literal()),
            Value::I64(val) => Expr::Number(None, val.into_number_literal()),
            Value::U64(val) => Expr::Number(None, val.into_number_literal()),
            Value::F64(val) => Expr::Number(None, val.into_number_literal()),
            Value::Null | Value::Object(_) => {
                let mut regex = try!(self.extract_object("regex").map_err(Error::Json));
                let pattern = try!(regex.extract_string("pattern").map_err(Error::Json));
                let flags = try!(regex.extract_string("flags").map_err(Error::Json));
                Expr::RegExp(None, RegExpLiteral {
                    pattern: pattern,
                    flags: flags.chars().collect()
                })
            }
            _ => { return type_error("null, number, boolean, string, or object", json.ty()); }
        })
    }
}
