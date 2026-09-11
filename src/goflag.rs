//! Stand-in for Go's `flag` package.
//!
//! hey's CLI is Go-flag shaped, not GNU shaped, and the difference is
//! user-visible: single dash long names (`-disable-keepalive`), `-n=5` and
//! `-n 5` both accepted, booleans that never consume the next argument,
//! parsing that stops at the first non-flag argument, and `-h` bound to a
//! *string* flag so it is not help. Error text and the exit code (2) are
//! reproduced as well.

use crate::gotime::{parse_duration, GoDuration};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Dur(GoDuration),
    /// Go: a `flag.Value` implementation that appends, i.e. `headerSlice`.
    Slice(Vec<String>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Str,
    Int,
    Float,
    Bool,
    Dur,
    Slice,
}

struct FlagDef {
    name: String,
    kind: Kind,
}

pub struct FlagSet {
    order: Vec<FlagDef>,
    values: HashMap<String, Value>,
    args: Vec<String>,
    /// Go: `flag.Usage`
    usage: Box<dyn Fn()>,
}

/// What `parseOne` can produce.
enum ParseOutcome {
    Continue,
    Stop,
    Help,
}

impl FlagSet {
    pub fn new(usage: Box<dyn Fn()>) -> FlagSet {
        FlagSet {
            order: Vec::new(),
            values: HashMap::new(),
            args: Vec::new(),
            usage,
        }
    }

    pub fn string(&mut self, name: &str, default: &str) {
        self.define(name, Kind::Str, Value::Str(default.to_string()));
    }
    pub fn int(&mut self, name: &str, default: i64) {
        self.define(name, Kind::Int, Value::Int(default));
    }
    pub fn float64(&mut self, name: &str, default: f64) {
        self.define(name, Kind::Float, Value::Float(default));
    }
    pub fn bool(&mut self, name: &str, default: bool) {
        self.define(name, Kind::Bool, Value::Bool(default));
    }
    pub fn duration(&mut self, name: &str, default: GoDuration) {
        self.define(name, Kind::Dur, Value::Dur(default));
    }
    /// Go: `flag.Var(&hs, "H", "")`
    pub fn var_slice(&mut self, name: &str) {
        self.define(name, Kind::Slice, Value::Slice(Vec::new()));
    }

    fn define(&mut self, name: &str, kind: Kind, default: Value) {
        self.order.push(FlagDef {
            name: name.to_string(),
            kind,
        });
        self.values.insert(name.to_string(), default);
    }

    pub fn get_str(&self, name: &str) -> String {
        match self.values.get(name) {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        }
    }
    pub fn get_int(&self, name: &str) -> i64 {
        match self.values.get(name) {
            Some(Value::Int(i)) => *i,
            _ => 0,
        }
    }
    pub fn get_float(&self, name: &str) -> f64 {
        match self.values.get(name) {
            Some(Value::Float(f)) => *f,
            _ => 0.0,
        }
    }
    pub fn get_bool(&self, name: &str) -> bool {
        match self.values.get(name) {
            Some(Value::Bool(b)) => *b,
            _ => false,
        }
    }
    pub fn get_dur(&self, name: &str) -> GoDuration {
        match self.values.get(name) {
            Some(Value::Dur(d)) => *d,
            _ => GoDuration::ZERO,
        }
    }
    pub fn get_slice(&self, name: &str) -> Vec<String> {
        match self.values.get(name) {
            Some(Value::Slice(v)) => v.clone(),
            _ => Vec::new(),
        }
    }

    /// Go: `flag.Args()`
    pub fn args(&self) -> &[String] {
        &self.args
    }
    /// Go: `flag.NArg()`
    pub fn narg(&self) -> usize {
        self.args.len()
    }

    fn kind_of(&self, name: &str) -> Option<Kind> {
        self.order.iter().find(|d| d.name == name).map(|d| d.kind)
    }

    /// The parse loop without Go's `ExitOnError` side effects, so the error
    /// text and the help signal can be asserted on directly.
    ///
    /// `Ok(true)` means `-h`/`-help` was seen.
    pub fn try_parse(&mut self, arguments: Vec<String>) -> Result<bool, String> {
        self.args = arguments;
        loop {
            match self.parse_one()? {
                ParseOutcome::Continue => continue,
                ParseOutcome::Stop => return Ok(false),
                ParseOutcome::Help => return Ok(true),
            }
        }
    }

    /// Go: `flag.Parse()` with `ExitOnError`.
    pub fn parse(&mut self, arguments: Vec<String>) {
        match self.try_parse(arguments) {
            Ok(false) => {}
            Ok(true) => {
                // Go: ExitOnError treats ErrHelp as a clean exit.
                (self.usage)();
                std::process::exit(0);
            }
            Err(msg) => {
                // Go: failf prints the message then the usage, exit 2.
                eprintln!("{}", msg);
                (self.usage)();
                std::process::exit(2);
            }
        }
    }

    fn parse_one(&mut self) -> Result<ParseOutcome, String> {
        if self.args.is_empty() {
            return Ok(ParseOutcome::Stop);
        }
        let s = self.args[0].clone();
        if s.len() < 2 || !s.starts_with('-') {
            return Ok(ParseOutcome::Stop);
        }
        let mut num_minuses = 1;
        if s.as_bytes()[1] == b'-' {
            num_minuses = 2;
            if s.len() == 2 {
                // "--" terminates the flags.
                self.args.remove(0);
                return Ok(ParseOutcome::Stop);
            }
        }
        let mut name = s[num_minuses..].to_string();
        if name.is_empty() || name.starts_with('-') || name.starts_with('=') {
            return Err(format!("bad flag syntax: {}", s));
        }

        self.args.remove(0);

        let mut has_value = false;
        let mut value = String::new();
        if let Some(i) = name.find('=') {
            value = name[i + 1..].to_string();
            has_value = true;
            name = name[..i].to_string();
        }

        let kind = match self.kind_of(&name) {
            Some(k) => k,
            None => {
                if name == "help" || name == "h" {
                    return Ok(ParseOutcome::Help);
                }
                return Err(format!("flag provided but not defined: -{}", name));
            }
        };

        if kind == Kind::Bool {
            let v = if has_value {
                parse_bool(&value).map_err(|_| {
                    format!(
                        "invalid boolean value {:?} for -{}: parse error",
                        value, name
                    )
                })?
            } else {
                true
            };
            self.values.insert(name, Value::Bool(v));
            return Ok(ParseOutcome::Continue);
        }

        if !has_value && !self.args.is_empty() {
            has_value = true;
            value = self.args.remove(0);
        }
        if !has_value {
            return Err(format!("flag needs an argument: -{}", name));
        }

        let parsed = match kind {
            Kind::Str => Value::Str(value.clone()),
            Kind::Int => Value::Int(
                parse_int(&value)
                    .map_err(|e| format!("invalid value {:?} for flag -{}: {}", value, name, e))?,
            ),
            Kind::Float => Value::Float(
                parse_float(&value)
                    .map_err(|e| format!("invalid value {:?} for flag -{}: {}", value, name, e))?,
            ),
            // Go's durationValue.Set discards time.ParseDuration's message
            // and reports the generic errParse, so "-z 3" says "parse error"
            // rather than "missing unit in duration".
            Kind::Dur => Value::Dur(parse_duration(&value).map_err(|_| {
                format!("invalid value {:?} for flag -{}: parse error", value, name)
            })?),
            Kind::Slice => {
                let mut cur = self.get_slice(&name);
                cur.push(value.clone());
                Value::Slice(cur)
            }
            Kind::Bool => unreachable!(),
        };
        self.values.insert(name, parsed);
        Ok(ParseOutcome::Continue)
    }
}

/// Go: `strconv.ParseBool`
fn parse_bool(s: &str) -> Result<bool, ()> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        _ => Err(()),
    }
}

/// Go: `strconv.ParseInt(s, 0, 64)` -- base 0 means 0x/0o/0b prefixes and
/// underscores are honoured. `flag` maps a syntax error to "parse error" and
/// an overflow to "value out of range".
fn parse_int(s: &str) -> Result<i64, &'static str> {
    let t = s.replace('_', "");
    let (neg, rest) = if let Some(r) = t.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = t.strip_prefix('+') {
        (false, r)
    } else {
        (false, t.as_str())
    };
    if rest.is_empty() {
        return Err("parse error");
    }
    let (radix, digits) =
        if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            (16, r)
        } else if let Some(r) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
            (2, r)
        } else if let Some(r) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O")) {
            (8, r)
        } else if rest.len() > 1 && rest.starts_with('0') {
            (8, &rest[1..])
        } else {
            (10, rest)
        };
    if digits.is_empty() {
        return Err("parse error");
    }
    match i128::from_str_radix(digits, radix) {
        Ok(v) => {
            let v = if neg { -v } else { v };
            if v > i64::MAX as i128 || v < i64::MIN as i128 {
                Err("value out of range")
            } else {
                Ok(v as i64)
            }
        }
        Err(e) => {
            if matches!(
                e.kind(),
                std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow
            ) {
                Err("value out of range")
            } else {
                Err("parse error")
            }
        }
    }
}

/// Go: `strconv.ParseFloat(s, 64)`
fn parse_float(s: &str) -> Result<f64, &'static str> {
    let t = s.replace('_', "");
    match t.to_ascii_lowercase().as_str() {
        "inf" | "+inf" | "infinity" | "+infinity" => return Ok(f64::INFINITY),
        "-inf" | "-infinity" => return Ok(f64::NEG_INFINITY),
        "nan" => return Ok(f64::NAN),
        _ => {}
    }
    t.parse::<f64>().map_err(|_| "parse error")
}

#[cfg(test)]
#[path = "goflag_test.rs"]
mod goflag_test;
