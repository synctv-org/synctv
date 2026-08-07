use boa_engine::{Context, Source};
use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, FunctionBody, Statement};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::{GetSpan, SourceType, Span};
use regex::Regex;

use crate::ProviderClientError;

const SETUP_SOURCE: &str = r#"
if (typeof globalThis.XMLHttpRequest === "undefined") {
    globalThis.XMLHttpRequest = { prototype: {} };
}
globalThis.location = {
    hash: "",
    host: "www.youtube.com",
    hostname: "www.youtube.com",
    href: "https://www.youtube.com/watch?v=synctv",
    origin: "https://www.youtube.com",
    password: "",
    pathname: "/watch",
    port: "",
    protocol: "https:",
    search: "?v=synctv",
    username: ""
};
if (typeof globalThis.document === "undefined") {
    globalThis.document = Object.create(null);
}
if (typeof globalThis.navigator === "undefined") {
    globalThis.navigator = Object.create(null);
}
if (typeof globalThis.self === "undefined") {
    globalThis.self = globalThis;
}
if (typeof globalThis.window === "undefined") {
    globalThis.window = globalThis;
}
"#;

const SOLVER_SOURCE: &str = r#"
function __synctv_solve(kind, input) {
    const results = new Set();
    const errors = [];
    for (const transformer of __synctv_transformers) {
        try {
            const sig = kind === "sig" ? input : undefined;
            const n = kind === "n" ? input : undefined;
            const url = transformer(
                "https://youtube.com/watch?v=synctv",
                "s",
                sig === undefined ? undefined : encodeURIComponent(sig)
            );
            url.set("n", n);
            const proto = Object.getPrototypeOf(url);
            const keys = Object.keys(proto).concat(Object.getOwnPropertyNames(proto));
            for (const key of keys) {
                if (!["constructor", "set", "get", "clone"].includes(key)) {
                    url[key]();
                    break;
                }
            }
            const value = kind === "sig"
                ? (url.get("s") == null ? null : decodeURIComponent(url.get("s")))
                : url.get("n");
            if (value != null) {
                results.add(value);
            }
        } catch (error) {
            errors.push(String(error));
        }
    }
    if (results.size === 0) {
        throw new Error("no challenge solutions: " + errors.join(", "));
    }
    if (results.size !== 1) {
        throw new Error("ambiguous challenge solutions: " + [...results].join(", "));
    }
    return results.values().next().value;
}
"#;

#[derive(Debug, Clone)]
pub struct YoutubeChallengeSolver {
    prepared_source: String,
}

impl YoutubeChallengeSolver {
    pub fn prepare(player_js: &str) -> Result<Self, ProviderClientError> {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, player_js, SourceType::default())
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();
        if let Some(diagnostic) = parsed.diagnostics.first() {
            return Err(ProviderClientError::Parse(format!(
                "Invalid YouTube player JavaScript: {diagnostic:?}"
            )));
        }

        let statements = player_statements(&parsed.program.body)?;
        let marker = Regex::new(r#"[\"']alr[\"']\s*,\s*[\"']yes[\"']"#)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        let mut prepared = String::with_capacity(player_js.len() + SOLVER_SOURCE.len());
        prepared.push_str(SETUP_SOURCE);
        let mut transformers = Vec::new();

        for statement in statements {
            if keep_statement(statement) {
                prepared.push_str(source_for_span(player_js, statement.span())?);
                prepared.push('\n');
            }
            collect_transformers(statement, player_js, &marker, &mut transformers)?;
        }

        transformers.sort();
        transformers.dedup();
        if transformers.is_empty() {
            return Err(ProviderClientError::Parse(
                "YouTube player challenge transformer was not found".to_string(),
            ));
        }
        prepared.push_str("const __synctv_transformers = [");
        prepared.push_str(&transformers.join(","));
        prepared.push_str("];\n");
        prepared.push_str(SOLVER_SOURCE);

        Ok(Self {
            prepared_source: prepared,
        })
    }

    pub fn solve_signature(&self, input: &str) -> Result<String, ProviderClientError> {
        self.solve("sig", input)
    }

    pub fn solve_n(&self, input: &str) -> Result<String, ProviderClientError> {
        self.solve("n", input)
    }

    fn solve(&self, kind: &str, input: &str) -> Result<String, ProviderClientError> {
        let mut context = Context::default();
        context
            .runtime_limits_mut()
            .set_loop_iteration_limit(100_000);
        context.runtime_limits_mut().set_recursion_limit(128);
        context.runtime_limits_mut().set_stack_size_limit(32_768);

        let kind = serde_json::to_string(kind)?;
        let input = serde_json::to_string(input)?;
        let source = format!("{}\n__synctv_solve({kind}, {input});", self.prepared_source);
        let value = context.eval(Source::from_bytes(&source)).map_err(|error| {
            ProviderClientError::Parse(format!("YouTube player challenge failed: {error}"))
        })?;
        value
            .to_string(&mut context)
            .map(|value| value.to_std_string_escaped())
            .map_err(|error| {
                ProviderClientError::Parse(format!(
                    "YouTube player challenge returned an invalid value: {error}"
                ))
            })
    }
}

fn player_statements<'a>(
    body: &'a oxc_allocator::Vec<'a, Statement<'a>>,
) -> Result<&'a [Statement<'a>], ProviderClientError> {
    let function = match body.as_slice() {
        [Statement::ExpressionStatement(statement)] => {
            let Expression::CallExpression(call) = statement.expression.without_parentheses()
            else {
                return unexpected_player_structure();
            };
            let Some(member) = call.callee.without_parentheses().as_member_expression() else {
                return unexpected_player_structure();
            };
            let Expression::FunctionExpression(function) = member.object().without_parentheses()
            else {
                return unexpected_player_structure();
            };
            function
        }
        [_, Statement::ExpressionStatement(statement)] => {
            let Expression::CallExpression(call) = statement.expression.without_parentheses()
            else {
                return unexpected_player_structure();
            };
            let Expression::FunctionExpression(function) = call.callee.without_parentheses() else {
                return unexpected_player_structure();
            };
            function
        }
        _ => return unexpected_player_structure(),
    };
    let body = function.body.as_deref().ok_or_else(|| {
        ProviderClientError::Parse("YouTube player wrapper has no body".to_string())
    })?;
    if matches!(body.statements.first(), Some(statement) if is_window_alias(statement)) {
        Ok(&body.statements[1..])
    } else {
        Ok(body.statements.as_slice())
    }
}

fn unexpected_player_structure<T>() -> Result<T, ProviderClientError> {
    Err(ProviderClientError::Parse(
        "Unexpected YouTube player JavaScript wrapper".to_string(),
    ))
}

fn is_window_alias(statement: &Statement<'_>) -> bool {
    matches!(statement, Statement::VariableDeclaration(declaration)
        if declaration.declarations.len() == 1
            && source_pattern_is_window_alias(&declaration.declarations[0]))
}

fn source_pattern_is_window_alias(declaration: &oxc_ast::ast::VariableDeclarator<'_>) -> bool {
    matches!(
        declaration.init.as_ref(),
        Some(Expression::ThisExpression(_))
    )
}

fn keep_statement(statement: &Statement<'_>) -> bool {
    match statement {
        Statement::ExpressionStatement(statement) => matches!(
            statement.expression.without_parentheses(),
            Expression::AssignmentExpression(_)
                | Expression::BooleanLiteral(_)
                | Expression::NullLiteral(_)
                | Expression::NumericLiteral(_)
                | Expression::BigIntLiteral(_)
                | Expression::RegExpLiteral(_)
                | Expression::StringLiteral(_)
                | Expression::TemplateLiteral(_)
        ),
        _ => true,
    }
}

fn collect_transformers(
    statement: &Statement<'_>,
    source: &str,
    marker: &Regex,
    transformers: &mut Vec<String>,
) -> Result<(), ProviderClientError> {
    match statement {
        Statement::FunctionDeclaration(function) => {
            if function_has_marker(function.body.as_deref(), source, marker) {
                if let Some(id) = &function.id {
                    transformers.push(id.name.to_string());
                }
            }
        }
        Statement::ExpressionStatement(statement) => {
            let Expression::AssignmentExpression(assignment) =
                statement.expression.without_parentheses()
            else {
                return Ok(());
            };
            let Expression::FunctionExpression(function) = assignment.right.without_parentheses()
            else {
                return Ok(());
            };
            if function_has_marker(function.body.as_deref(), source, marker) {
                transformers.push(source_for_span(source, assignment.left.span())?.to_string());
            }
        }
        Statement::VariableDeclaration(declaration) => {
            for declarator in &declaration.declarations {
                let Some(Expression::FunctionExpression(function)) = declarator.init.as_ref()
                else {
                    continue;
                };
                if function_has_marker(function.body.as_deref(), source, marker) {
                    transformers.push(source_for_span(source, declarator.id.span())?.to_string());
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn function_has_marker(body: Option<&FunctionBody<'_>>, source: &str, marker: &Regex) -> bool {
    body.and_then(|body| source_for_span(source, body.span).ok())
        .is_some_and(|body| marker.is_match(body))
}

fn source_for_span(source: &str, span: Span) -> Result<&str, ProviderClientError> {
    source
        .get(span.start as usize..span.end as usize)
        .ok_or_else(|| ProviderClientError::Parse("Invalid YouTube player AST span".to_string()))
}

#[cfg(test)]
mod tests {
    use super::YoutubeChallengeSolver;

    const PLAYER: &str = r#"
(function(){
var H={};
H.mark=function(a,b){};
function U(url,key,value){this.values={v:"synctv"};if(value!==undefined)this.values[key]=value}
U.prototype.set=function(key,value){if(value!==undefined)this.values[key]=value};
U.prototype.get=function(key){return this.values[key]};
U.prototype.clone=function(){return this};
U.prototype.transform=function(){if(this.values.s!==undefined)this.values.s=this.values.s.split("").reverse().join("");if(this.values.n!==undefined)this.values.n=this.values.n.toUpperCase()};
function Transform(url,key,value){H.mark("alr","yes");return new U(url,key,value)}
H.sideEffect();
}).call(this);
"#;

    #[test]
    fn solves_signature_and_n_challenges() {
        let solver = YoutubeChallengeSolver::prepare(PLAYER).expect("prepare solver");
        assert_eq!(
            solver.solve_signature("abcdef").expect("signature"),
            "fedcba"
        );
        assert_eq!(solver.solve_n("a1-b").expect("n"), "A1-B");
    }

    #[test]
    fn supports_two_statement_player_wrapper() {
        let player = PLAYER
            .replacen(
                "(function(){",
                "var ignored=1;(function(){var window=this;",
                1,
            )
            .replacen("}).call(this);", "})();", 1);
        let solver = YoutubeChallengeSolver::prepare(&player).expect("prepare solver");
        assert_eq!(solver.solve_signature("abcd").expect("signature"), "dcba");
    }
}
