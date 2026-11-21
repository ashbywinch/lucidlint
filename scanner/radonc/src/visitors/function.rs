//! One function or method entry, mirroring radon's `Function` namedtuple.

/// One function or method entry, mirroring radon's `Function` namedtuple.
#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub lineno: i32,
    pub col_offset: i32,
    pub endline: i32,
    pub is_method: bool,
    pub classname: Option<String>,
    pub closures: Vec<Function>,
    pub inner_classes: Vec<super::Class>,
    pub complexity: i32,
}

impl Function {
    /// radon's `Function.letter`: 'F' for functions, 'M' for methods.
    pub fn letter(&self) -> char {
        if self.is_method {
            'M'
        } else {
            'F'
        }
    }

    /// radon's `Function.fullname`: `cls.method` for methods, name otherwise.
    pub fn fullname(&self) -> String {
        match &self.classname {
            Some(c) => format!("{}.{}", c, self.name),
            None => self.name.clone(),
        }
    }
}

impl std::fmt::Display for Function {
    /// radon's `Function.__str__`: `F name:start->end cls.name - complexity`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}:{}->{} {} - {}",
            self.letter(),
            self.lineno,
            self.col_offset,
            self.endline,
            self.fullname(),
            self.complexity
        )
    }
}
