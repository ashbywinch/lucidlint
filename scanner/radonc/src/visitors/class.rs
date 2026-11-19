//! One class block — mirroring radon's `Class` namedtuple.

/// One class block — mirroring radon's `Class` namedtuple.
#[derive(Clone, Debug)]
pub struct Class {
    pub name: String,
    pub lineno: i32,
    pub col_offset: i32,
    pub endline: i32,
    pub inner_classes: Vec<Class>,
    pub methods: Vec<super::Function>,
    pub complexity: i32,
}

impl Class {
    /// radon's `Class.letter`: always 'C'.
    // the mirror keeps letter as a method to match radon's property (R26) —
    // structure over instance-state purity
    // lucidlint: ignore detached-method radon mirror keeps letter a method
    pub fn letter(&self) -> char {
        'C'
    }

    /// radon's `Class.fullname`: the class name.
    pub fn fullname(&self) -> &str {
        &self.name
    }

    /// radon's `Class.complexity`: the class's own complexity. Note the real
    /// complexity (which includes nested classes) is exposed separately.
    pub fn real_complexity(&self) -> i32 {
        self.complexity
    }
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "C {}:{}->{} {} - {}",
            self.lineno, self.col_offset, self.endline, self.name, self.complexity
        )
    }
}
