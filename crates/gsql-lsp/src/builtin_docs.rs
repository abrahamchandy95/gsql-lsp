//! The reference pages of the built-in functions and methods (syntax,
//! parameters with their types, return type, notes and examples), extracted
//! from the TigerGraph documentation by `scripts/sync_builtin_docs.py`.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::builtins::{self, Accumulator, Function, Method};

#[derive(Debug, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Deserialize)]
pub struct Doc {
    pub name: String,
    pub group: String,
    pub syntax: Vec<String>,
    pub description: Vec<String>,
    pub returns: String,
    pub parameters: Vec<Parameter>,
    #[serde(default)]
    pub notes: Vec<String>,
    pub examples: Vec<String>,
}

#[derive(Deserialize)]
struct AccumulatorMethod {
    signature: String,
    returns: String,
    mutator: bool,
    description: String,
}

#[derive(Deserialize)]
struct AccumulatorPage {
    names: Vec<String>,
    description: Vec<String>,
    methods: Vec<AccumulatorMethod>,
}

#[derive(Deserialize)]
struct Data {
    entries: Vec<Doc>,
    #[serde(default)]
    accumulators: Vec<AccumulatorPage>,
}

/// The start of the description of a method that changes its accumulator.
const MUTATOR: &str = "Modifies the accumulator. ";

fn docs() -> &'static HashMap<(String, String), Doc> {
    static DOCS: OnceLock<HashMap<(String, String), Doc>> = OnceLock::new();
    DOCS.get_or_init(|| {
        let data: Data =
            serde_json::from_str(include_str!("../data/builtin-docs.json"))
                .unwrap_or(Data {
                    entries: Vec::new(),
                    accumulators: Vec::new(),
                });
        let mut docs: HashMap<(String, String), Doc> = data
            .entries
            .into_iter()
            .map(|d| ((d.group.clone(), d.name.to_ascii_lowercase()), d))
            .collect();
        // An accumulator type is a page of its own, in the group "accumulator";
        // its methods are in the group "acc:type".
        for page in data.accumulators {
            for name in &page.names {
                let lower = name.to_ascii_lowercase();
                let make = |group: String,
                            name: &str,
                            syntax: Vec<String>,
                            description: Vec<String>,
                            returns: String| Doc {
                    name: name.to_string(),
                    group,
                    syntax,
                    description,
                    returns,
                    parameters: Vec::new(),
                    notes: Vec::new(),
                    examples: Vec::new(),
                };
                let group = "accumulator".to_string();
                docs.insert(
                    (group.clone(), lower.clone()),
                    make(
                        group,
                        name,
                        Vec::new(),
                        page.description.clone(),
                        String::new(),
                    ),
                );
                for method in &page.methods {
                    let signature = method
                        .signature
                        .trim_start_matches('.')
                        .trim()
                        .to_string();
                    let Some(method_name) = signature
                        .split('(')
                        .next()
                        .map(|n| n.trim().to_ascii_lowercase())
                    else {
                        continue;
                    };
                    let group = accumulator_group(name);
                    let kind = if method.mutator { MUTATOR } else { "" };
                    let doc = make(
                        group.clone(),
                        &method_name,
                        vec![signature],
                        vec![format!("{kind}{}", method.description)],
                        format!("`{}`", method.returns),
                    );
                    docs.entry((group, method_name))
                        .or_insert(doc);
                }
            }
        }
        docs
    })
}

/// The documentation page of a function.
pub fn function(name: &str) -> Option<&'static Doc> {
    let key = name.to_ascii_lowercase();
    ["functions", "loading"]
        .into_iter()
        .find_map(|group| docs().get(&(group.to_string(), key.clone())))
}

/// The group of the methods of an accumulator type: `acc:sumaccum`.
fn accumulator_group(name: &str) -> String {
    format!("acc:{}", name.to_ascii_lowercase())
}

/// The group of methods a method belongs to, by where it is defined.
fn group_of(method: &Method) -> Option<String> {
    builtins::METHOD_GROUPS
        .iter()
        .find(|(_, _, methods)| {
            methods
                .iter()
                .any(|m| std::ptr::eq(m, method))
        })
        .map(|(name, _, _)| name.to_ascii_lowercase())
        .or_else(|| {
            builtins::ACCUMULATORS
                .iter()
                .find(|a| {
                    a.methods
                        .iter()
                        .any(|m| std::ptr::eq(m, method))
                })
                .map(|a| accumulator_group(a.name))
        })
}

/// The documentation page of an accumulator type.
pub fn accumulator(name: &str) -> Option<&'static Doc> {
    docs().get(&("accumulator".to_string(), name.to_ascii_lowercase()))
}

/// The documentation page of a method of vertices, edges or JSON values.
pub fn method(method: &Method) -> Option<&'static Doc> {
    method_in(&group_of(method)?, method.name)
}

pub fn method_in(group: &str, name: &str) -> Option<&'static Doc> {
    docs().get(&(group.to_string(), name.to_ascii_lowercase()))
}

/// A reference page as Markdown, or `short` when the page is missing.
fn page_or(page: Option<&Doc>, short: &str) -> String {
    page.map_or_else(|| short.to_string(), Doc::markdown)
}

/// The documentation of a built-in function, as Markdown.
pub fn function_markdown(f: &Function) -> String {
    page_or(function(f.name), f.doc)
}

/// The documentation of a built-in method, as Markdown.
pub fn method_markdown(m: &Method) -> String {
    page_or(method(m), m.doc)
}

/// The documentation of an accumulator type, as Markdown.
pub fn accumulator_markdown(a: &Accumulator) -> String {
    page_or(accumulator(a.name), a.doc)
}

impl Doc {
    /// The page as Markdown lines: description, syntax, parameters, result,
    /// notes and examples.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        for paragraph in &self.description {
            out.push_str(paragraph);
            out.push_str("\n\n");
        }
        if !self.syntax.is_empty() {
            out.push_str(&format!(
                "**Syntax**\n```gsql\n{}\n```\n\n",
                self.syntax.join("\n")
            ));
        }
        if !self.parameters.is_empty() {
            out.push_str("**Parameters**\n");
            for p in &self.parameters {
                let ty = if p.ty.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", p.ty)
                };
                out.push_str(&format!(
                    "- `{}`{ty}: {}\n",
                    p.name, p.description
                ));
            }
            out.push('\n');
        }
        if !self.returns.is_empty() {
            out.push_str(&format!("**Returns** {}\n\n", self.returns));
        }
        for note in &self.notes {
            out.push_str(&format!("*Note:* {note}\n\n"));
        }
        for example in &self.examples {
            out.push_str(&format!(
                "**Example**\n```gsql\n{example}\n```\n\n"
            ));
        }
        out.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_functions_and_methods() {
        let doc = function("datetime_format").expect("documented");
        assert!(doc.syntax[0].starts_with("datetime_format("));
        assert_eq!(doc.parameters[0].name, "date");
        let outdegree =
            builtins::find_method(builtins::VERTEX_METHODS, "outdegree")
                .unwrap();
        assert!(
            method(outdegree)
                .is_some_and(|d| { d.description[0].contains("outgoing") })
        );
    }

    /// The parameters of the tables are named as in the documentation
    /// (`scripts/apply_doc_names.py` does it).
    #[test]
    fn parameters_are_named_as_in_the_docs() {
        let mut wrong = Vec::new();
        let mut check =
            |label: String, params: &[&str], doc: Option<&Doc>| {
                let Some(doc) = doc.filter(|d| !d.syntax.is_empty()) else {
                    return;
                };
                for param in params {
                    let name = param
                        .trim_matches(|c| c == '[' || c == ']' || c == ' ');
                    // (`...` stands for the repeated last parameter.)
                    if !name.is_empty()
                        && name != "..."
                        && !doc
                            .syntax
                            .iter()
                            .any(|syntax| syntax.contains(name))
                    {
                        wrong.push(format!(
                            "{label}: `{param}` is not in `{}`",
                            doc.syntax.join(" | ")
                        ));
                    }
                }
            };
        for f in builtins::FUNCTIONS {
            check(f.name.to_string(), f.params, function(f.name));
        }
        for &(_, _, methods) in builtins::METHOD_GROUPS {
            for m in methods {
                check(format!(".{}", m.name), m.params, method(m));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn methods_of_bitwise_accumulators_are_grouped_by_type() {
        for (name, group) in [
            ("BitwiseAndAccum", "acc:bitwiseandaccum"),
            ("BitwiseOrAccum", "acc:bitwiseoraccum"),
        ] {
            let methods = builtins::accumulator(name).unwrap().methods;
            for m in methods {
                assert_eq!(group_of(m).as_deref(), Some(group));
            }
        }
    }

    #[test]
    fn documents_accumulators_and_their_methods() {
        let set = builtins::accumulator("SetAccum").unwrap();
        assert!(accumulator("SetAccum").is_some_and(|d| {
            d.description[0].contains("unique elements")
        }));
        let contains =
            builtins::find_method(set.methods, "contains").unwrap();
        let page = method(contains).expect("documented");
        assert!(page.syntax[0].starts_with("contains("));
        assert!(page.description[0].contains("Returns true"));
        // The same method of another accumulator has its own page.
        let map = builtins::accumulator("MapAccum").unwrap();
        let clear = builtins::find_method(map.methods, "clear").unwrap();
        assert_ne!(
            method(clear).map(|d| d.group.as_str()),
            method(builtins::find_method(set.methods, "clear").unwrap())
                .map(|d| d.group.as_str())
        );
    }

    /// Scraper leftovers: "None" sections, footer copyright, MathJax, example backticks.
    #[test]
    fn pages_hold_no_scraping_leftovers() {
        let junk = |text: &str| {
            let text = text
                .strip_prefix(MUTATOR)
                .unwrap_or(text)
                .trim()
                .trim_matches('`');
            matches!(text, "None" | "None.") || text.contains("Copyright")
        };
        let mut found = Vec::new();
        for doc in docs().values() {
            let label = format!("{}/{}", doc.group, doc.name);
            let mut fields: Vec<(&str, &str)> =
                vec![("returns", doc.returns.as_str())];
            fields.extend(
                doc.description
                    .iter()
                    .map(|d| ("description", d.as_str())),
            );
            fields.extend(
                doc.notes
                    .iter()
                    .map(|n| ("note", n.as_str())),
            );
            for p in &doc.parameters {
                fields.extend([
                    ("parameter name", p.name.as_str()),
                    ("parameter description", p.description.as_str()),
                    ("parameter type", p.ty.as_str()),
                ]);
            }
            for (field, text) in &fields {
                if junk(text) || text.contains("\\(") {
                    found.push(format!("{label} {field}: {text:?}"));
                }
            }
            for example in &doc.examples {
                if example.contains('`') || example.contains("Copyright") {
                    found.push(format!("{label} example: {example:?}"));
                }
            }
        }
        found.sort();
        assert!(found.is_empty(), "{found:#?}");
    }

    #[test]
    fn documents_most_built_ins() {
        let missing: Vec<&str> = builtins::FUNCTIONS
            .iter()
            .filter(|f| {
                f.category != builtins::Category::Loading
                    && function(f.name).is_none()
            })
            .map(|f| f.name)
            .collect();
        assert!(missing.len() < 10, "{missing:?}");
    }
}
