use crate::ir::{ClassMethod, Expression, Statement};

// Detected class information during analysis.
#[derive(Debug, Clone, Default)]
pub struct ClassBuilder {
    pub name: String,
    pub constructor: Option<Expression>,
    pub constructor_body: Option<Vec<Statement>>,
    pub constructor_params: Vec<String>,
    pub super_class: Option<Expression>,
    pub methods: Vec<ClassMethod>,
}
