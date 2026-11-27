use std::collections::HashSet;

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

struct PlayerWrapper<'a> {
    initializer: Option<Span>,
    expression: Span,
    body: &'a FunctionBody<'a>,
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

        let wrapper = player_wrapper(&parsed.program.body)?;
        let marker = Regex::new(r#"[\"']alr[\"']\s*,\s*[\"']yes[\"']"#)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        let inheritance_marker = Regex::new(
            r"\.\s*prototype\s*=\s*[^;]+\.\s*prototype[^;]*;\s*[\w$]+\.\s*prototype\s*\.\s*constructor\s*=",
        )
        .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        let mut prepared = String::with_capacity(player_js.len() + SOLVER_SOURCE.len());
        prepared.push_str(SETUP_SOURCE);
        if let Some(initializer) = wrapper.initializer {
            prepared.push_str(source_for_span(player_js, initializer)?);
            prepared.push_str(";\n");
        }
        // The player defines helpers on the wrapper argument and reads them from its closure.
        // Keep that scope and invocation while omitting unrelated browser startup expressions.
        prepared.push_str("const __synctv_run_player = ");
        prepared.push_str(source_for_span(
            player_js,
            Span::new(wrapper.expression.start, wrapper.body.span.start),
        )?);
        prepared.push_str("{\n");
        for directive in &wrapper.body.directives {
            prepared.push_str(source_for_span(player_js, directive.span)?);
            prepared.push_str(";\n");
        }
        let mut transformers = Vec::new();
        let mut inheritance_helpers = HashSet::new();

        for statement in &wrapper.body.statements {
            if let Statement::ExpressionStatement(statement) = statement
                && let Expression::AssignmentExpression(assignment) =
                    statement.expression.without_parentheses()
                && let Expression::FunctionExpression(function) =
                    assignment.right.without_parentheses()
                && function.params.items.len() == 2
                && function_has_marker(function.body.as_deref(), player_js, &inheritance_marker)
            {
                inheritance_helpers.insert(source_for_span(player_js, assignment.left.span())?);
            }
            if keep_statement(statement, player_js, &inheritance_helpers) {
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
        prepared.push_str("return __synctv_solve;\n}");
        prepared.push_str(source_for_span(
            player_js,
            Span::new(wrapper.body.span.end, wrapper.expression.end),
        )?);
        prepared.push_str(";\n");

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
        // Boa compiles a multi-megabyte player script on the calling thread.
        stacker::maybe_grow(8 * 1024 * 1024, 16 * 1024 * 1024, || {
            self.evaluate(kind, input)
        })
    }

    fn evaluate(&self, kind: &str, input: &str) -> Result<String, ProviderClientError> {
        let mut context = Context::default();
        context
            .runtime_limits_mut()
            .set_loop_iteration_limit(100_000);
        context.runtime_limits_mut().set_recursion_limit(128);
        context.runtime_limits_mut().set_stack_size_limit(32_768);

        let kind = serde_json::to_string(kind)?;
        let input = serde_json::to_string(input)?;
        let source = format!(
            "{}\n__synctv_run_player({kind}, {input});",
            self.prepared_source
        );
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

fn player_wrapper<'a>(
    body: &'a oxc_allocator::Vec<'a, Statement<'a>>,
) -> Result<PlayerWrapper<'a>, ProviderClientError> {
    let (initializer, wrapper) = match body.as_slice() {
        [
            Statement::VariableDeclaration(initializer),
            Statement::ExpressionStatement(statement),
        ] => (Some(initializer.span), &statement.expression),
        [Statement::ExpressionStatement(statement)]
        | [_, Statement::ExpressionStatement(statement)] => (None, &statement.expression),
        _ => return unexpected_player_structure(),
    };
    let Expression::CallExpression(call) = wrapper.without_parentheses() else {
        return unexpected_player_structure();
    };
    let callee = call.callee.without_parentheses();
    let function = callee
        .as_member_expression()
        .map_or(callee, |member| member.object().without_parentheses());
    let Expression::FunctionExpression(function) = function else {
        return unexpected_player_structure();
    };
    let body = function.body.as_deref().ok_or_else(|| {
        ProviderClientError::Parse("YouTube player wrapper has no body".to_string())
    })?;
    Ok(PlayerWrapper {
        initializer,
        expression: wrapper.span(),
        body,
    })
}

fn unexpected_player_structure<T>() -> Result<T, ProviderClientError> {
    Err(ProviderClientError::Parse(
        "Unexpected YouTube player JavaScript wrapper".to_string(),
    ))
}

fn keep_statement(
    statement: &Statement<'_>,
    source: &str,
    inheritance_helpers: &HashSet<&str>,
) -> bool {
    match statement {
        Statement::ExpressionStatement(statement) => {
            match statement.expression.without_parentheses() {
                Expression::CallExpression(call) => {
                    call.arguments.len() == 2
                        && source_for_span(source, call.callee.without_parentheses().span())
                            .is_ok_and(|callee| inheritance_helpers.contains(callee))
                }
                expression => matches!(
                    expression,
                    Expression::AssignmentExpression(_)
                        | Expression::BooleanLiteral(_)
                        | Expression::NullLiteral(_)
                        | Expression::NumericLiteral(_)
                        | Expression::BigIntLiteral(_)
                        | Expression::RegExpLiteral(_)
                        | Expression::StringLiteral(_)
                        | Expression::TemplateLiteral(_)
                ),
            }
        }
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
            if function_has_marker(function.body.as_deref(), source, marker)
                && let Some(id) = &function.id
            {
                transformers.push(id.name.to_string());
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

    #[test]
    fn preserves_player_wrapper_argument() {
        let player = PLAYER
            .replacen(
                "(function(){\nvar H={};",
                "var player = {}; (function(g){var window=this;",
                1,
            )
            .replace("H.", "g.")
            .replacen("}).call(this);", "})(player);", 1);
        let solver = YoutubeChallengeSolver::prepare(&player).expect("prepare solver");
        assert_eq!(solver.solve_signature("abcd").expect("signature"), "dcba");
        assert_eq!(solver.solve_n("a1-b").expect("n"), "A1-B");
    }

    #[test]
    fn keeps_player_inheritance_initialization() {
        let player = PLAYER
            .replacen(
                "var H={};",
                r#"var H={};
H.link=function(child,parent){child.prototype=Object.create(parent.prototype);child.prototype.constructor=child};
function Base(){}
Base.prototype.setup=function(){};"#,
                1,
            )
            .replacen(
                "U.prototype.set=",
                "H.link(U,Base);var ready=new U().setup();\nU.prototype.set=",
                1,
            );
        let solver = YoutubeChallengeSolver::prepare(&player).expect("prepare solver");
        assert_eq!(solver.solve_signature("abcd").expect("signature"), "dcba");
    }
}
