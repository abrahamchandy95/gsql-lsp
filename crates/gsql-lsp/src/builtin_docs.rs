//! The reference pages of the built-in functions and methods (syntax,
//! parameters with their types, return type, notes and examples), extracted
//! from the TigerGraph documentation by `scripts/sync_builtin_docs.py`.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::builtins::{self, Method};

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

fn docs() -> &'static HashMap<(String, String), Doc> {
    static DOCS: OnceLock<HashMap<(String, String), Doc>> = OnceLock::new();
    DOCS.get_or_init(|| {
        let data: Data = serde_json::from_str(include_str!("../data/builtin-docs.json"))
            .unwrap_or(Data { entries: Vec::new(), accumulators: Vec::new() });
        let mut docs: HashMap<(String, String), Doc> =
            data.entries.into_iter().map(|d| ((d.group.clone(), d.name.to_ascii_lowercase()), d)).collect();
        // An accumulator type is a page of its own, in the group "accumulator";
        // its methods are in the group "acc:type".
        for page in data.accumulators {
            for name in &page.names {
                let lower = name.to_ascii_lowercase();
                let make =
                    |group: String, name: &str, syntax: Vec<String>, description: Vec<String>, returns: String| Doc {
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
                    make(group, name, Vec::new(), page.description.clone(), String::new()),
                );
                for method in &page.methods {
                    let signature = method.signature.trim_start_matches('.').trim().to_string();
                    let Some(method_name) = signature.split('(').next().map(|n| n.trim().to_ascii_lowercase()) else {
                        continue;
                    };
                    let group = format!("acc:{lower}");
                    let kind = if method.mutator { "Modifies the accumulator. " } else { "" };
                    let doc = make(
                        group.clone(),
                        &method_name,
                        vec![signature],
                        vec![format!("{kind}{}", method.description)],
                        format!("`{}`", method.returns),
                    );
                    docs.entry((group, method_name)).or_insert(doc);
                }
            }
        }
        docs
    })
}

/// The documentation page of a function.
pub fn function(name: &str) -> Option<&'static Doc> {
    let key = name.to_ascii_lowercase();
    ["functions", "loading"].into_iter().find_map(|group| docs().get(&(group.to_string(), key.clone())))
}

/// The group of methods a method belongs to, by where it is defined.
fn group_of(method: &Method) -> Option<String> {
    [
        ("vertex", builtins::VERTEX_METHODS),
        ("edge", builtins::EDGE_METHODS),
        ("jsonobject", builtins::JSON_OBJECT_METHODS),
        ("jsonarray", builtins::JSON_ARRAY_METHODS),
    ]
    .into_iter()
    .find(|(_, methods)| methods.iter().any(|m| std::ptr::eq(m, method)))
    .map(|(group, _)| group.to_string())
    .or_else(|| {
        builtins::ACCUMULATORS
            .iter()
            .find(|a| a.methods.iter().any(|m| std::ptr::eq(m, method)))
            .map(|a| format!("acc:{}", a.name.to_ascii_lowercase()))
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
            out.push_str(&format!("**Syntax**\n```gsql\n{}\n```\n\n", self.syntax.join("\n")));
        }
        if !self.parameters.is_empty() {
            out.push_str("**Parameters**\n");
            for p in &self.parameters {
                let ty = if p.ty.is_empty() { String::new() } else { format!(" ({})", p.ty) };
                out.push_str(&format!("- `{}`{ty}: {}\n", p.name, p.description));
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
            out.push_str(&format!("**Example**\n```gsql\n{example}\n```\n\n"));
        }
        out.trim_end().to_string()
    }

    /// The page as plain text for a comment block.
    pub fn plain(&self) -> String {
        self.markdown().replace("```gsql\n", "").replace("```", "").replace("**", "").replace('`', "")
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
        let outdegree = builtins::find_method(builtins::VERTEX_METHODS, "outdegree").unwrap();
        assert!(method(outdegree).is_some_and(|d| d.description[0].contains("outgoing")));
    }

    /// The parameters of the tables are named as in the documentation
    /// (`scripts/apply_doc_names.py` does it).
    #[test]
    fn parameters_are_named_as_in_the_docs() {
        let mut wrong = Vec::new();
        let mut check = |label: String, params: &[&str], doc: Option<&Doc>| {
            let Some(doc) = doc.filter(|d| !d.syntax.is_empty()) else { return };
            for param in params {
                let name = param.trim_matches(|c| c == '[' || c == ']' || c == ' ');
                // (`...` stands for the repeated last parameter.)
                if !name.is_empty() && name != "..." && !doc.syntax.iter().any(|syntax| syntax.contains(name)) {
                    wrong.push(format!("{label}: `{param}` is not in `{}`", doc.syntax.join(" | ")));
                }
            }
        };
        for f in builtins::FUNCTIONS {
            check(f.name.to_string(), f.params, function(f.name));
        }
        for (group, methods) in [
            ("vertex", builtins::VERTEX_METHODS),
            ("edge", builtins::EDGE_METHODS),
            ("jsonobject", builtins::JSON_OBJECT_METHODS),
            ("jsonarray", builtins::JSON_ARRAY_METHODS),
        ] {
            for m in methods {
                check(format!(".{}", m.name), m.params, method_in(group, m.name));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn methods_of_bitwise_accumulators_are_grouped_by_type() {
        for (name, group) in [("BitwiseAndAccum", "acc:bitwiseandaccum"), ("BitwiseOrAccum", "acc:bitwiseoraccum")] {
            let methods = builtins::accumulator(name).unwrap().methods;
            for m in methods {
                assert_eq!(group_of(m).as_deref(), Some(group));
            }
        }
    }

    #[test]
    fn documents_accumulators_and_their_methods() {
        let set = builtins::accumulator("SetAccum").unwrap();
        assert!(accumulator("SetAccum").is_some_and(|d| d.description[0].contains("unique elements")));
        let contains = builtins::find_method(set.methods, "contains").unwrap();
        let page = method(contains).expect("documented");
        assert!(page.syntax[0].starts_with("contains("));
        assert!(page.description[0].contains("Returns true"));
        // The same method of another accumulator has its own page.
        let map = builtins::accumulator("MapAccum").unwrap();
        let clear = builtins::find_method(map.methods, "clear").unwrap();
        assert_ne!(
            method(clear).map(|d| d.group.as_str()),
            method(builtins::find_method(set.methods, "clear").unwrap()).map(|d| d.group.as_str())
        );
    }

    #[test]
    fn documents_most_built_ins() {
        let missing: Vec<&str> = builtins::FUNCTIONS
            .iter()
            .filter(|f| f.category != builtins::Category::Loading && function(f.name).is_none())
            .map(|f| f.name)
            .collect();
        assert!(missing.len() < 10, "{missing:?}");
    }
}
