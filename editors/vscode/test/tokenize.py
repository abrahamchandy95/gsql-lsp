#!/usr/bin/env python3
"""A minimal TextMate tokenizer for checking syntaxes/gsql.tmLanguage.json.

It implements the parts of the TextMate algorithm the grammar uses (match,
begin/end, captures, includes; earliest match wins, ties go to the first
pattern) with Python regexes, which accept the constructs used in the grammar.

    python3 test/tokenize.py file.gsql      print tokens with scopes
    python3 test/tokenize.py --test         run the built-in assertions
"""

import json
import re
import sys
from pathlib import Path

GRAMMAR = json.loads((Path(__file__).parent.parent / "syntaxes" / "gsql.tmLanguage.json").read_text())


def compile_rule(rule):
    for key in ("match", "begin", "end"):
        if key in rule and not isinstance(rule[key], re.Pattern):
            rule[key + "_re"] = re.compile(rule[key])
    return rule


def resolve(patterns):
    out = []
    for p in patterns:
        if "include" in p:
            name = p["include"]
            if name == "$self":
                target = GRAMMAR["patterns"]
            else:
                # A repository rule (match or begin/end) is included itself;
                # an entry with only patterns is a group of rules.
                entry = GRAMMAR["repository"][name[1:]]
                target = [entry] if "match" in entry or "begin" in entry else entry.get("patterns", [])
            out.extend(resolve(target))
        else:
            out.append(compile_rule(p))
    return out


def tokenize_line(line, stack):
    tokens = []
    pos = 0
    while pos <= len(line):
        top = stack[-1] if stack else None
        patterns = resolve(top.get("patterns", [])) if top else resolve(GRAMMAR["patterns"])
        best = None
        if top is not None:
            m = top["end_re"].search(line, pos)
            if m:
                best = (m.start(), -1, "end", top, m)
        for index, rule in enumerate(patterns):
            regex = rule.get("match_re") or rule.get("begin_re")
            m = regex.search(line, pos)
            if m and (best is None or m.start() < best[0]):
                best = (m.start(), index, "begin" if "begin" in rule else "match", rule, m)
        if best is None:
            if pos < len(line):
                tokens.append((line[pos:], [s["name"] for s in stack if "name" in s]))
            break
        start, _, kind, rule, m = best
        if start > pos:
            tokens.append((line[pos:start], [s.get("name") for s in stack if s.get("name")]))
        outer = [s.get("name") for s in stack if s.get("name")]
        if kind == "match":
            add_captures(tokens, line, m, rule.get("captures", {}), outer + ([rule["name"]] if "name" in rule else []))
        elif kind == "begin":
            add_captures(tokens, line, m, rule.get("beginCaptures", rule.get("captures", {})), outer + ([rule["name"]] if "name" in rule else []))
            stack.append(rule)
        else:
            add_captures(tokens, line, m, rule.get("endCaptures", rule.get("captures", {})), outer)
            stack.pop()
        if m.end() == pos and kind == "match":
            pos += 1
        else:
            pos = m.end()
        if m.end() == len(line) and kind != "begin":
            break
    return tokens


def add_captures(tokens, line, m, captures, scopes):
    if not captures:
        if m.group(0):
            tokens.append((m.group(0), scopes))
        return
    pos = m.start()
    groups = sorted((int(k), v) for k, v in captures.items())
    if "0" in captures:
        scopes = scopes + [captures["0"]["name"]]
    for number, capture in groups:
        if number == 0 or m.group(number) is None or not m.group(number):
            continue
        s, e = m.span(number)
        if s > pos:
            tokens.append((line[pos:s], scopes))
        tokens.append((line[s:e], scopes + [capture["name"]]))
        pos = e
    if pos < m.end():
        tokens.append((line[pos:m.end()], scopes))


def tokenize(text):
    stack = []
    result = []
    for line in text.split("\n"):
        result.append(tokenize_line(line, stack))
    return result


def scope_of(text, token):
    for line in tokenize(text):
        for tok, scopes in line:
            if tok == token:
                return scopes[-1] if scopes else None
    return "<missing>"


ASSERTIONS = [
    ("CREATE QUERY pr(VERTEX<Person> p) FOR GRAPH Social {", "pr", "entity.name.function.gsql"),
    ("CREATE QUERY pr(VERTEX<Person> p) FOR GRAPH Social {", "Person", "entity.name.type.gsql"),
    ("CREATE QUERY pr(VERTEX<Person> p) FOR GRAPH Social {", "Social", "entity.name.namespace.graph.gsql"),
    ("  SumAccum<INT> @@total = 0;", "SumAccum", "support.type.accumulator.gsql"),
    ("  SumAccum<INT> @@total = 0;", "INT", "storage.type.gsql"),
    ("  SumAccum<INT> @@total = 0;", "@@total", "variable.other.accumulator.global.gsql"),
    ("  R = SELECT t FROM Start:s -(Knows>:e)- Person:t", "Start", "entity.name.type.gsql"),
    ("  R = SELECT t FROM Start:s -(Knows>:e)- Person:t", "s", "variable.other.alias.gsql"),
    ("  R = SELECT t FROM Start:s -(Knows>:e)- Person:t", "SELECT", "keyword.other.gsql"),
    ("      ACCUM t.@score += abs(s.@score'), @@n += s.outdegree()", "@score", "variable.other.accumulator.local.gsql"),
    ("      ACCUM t.@score += abs(s.@score'), @@n += s.outdegree()", "abs", "support.function.builtin.gsql"),
    ("      ACCUM t.@score += abs(s.@score'), @@n += s.outdegree()", "outdegree", "entity.name.function.member.gsql"),
    ("      POST-ACCUM s.name = \"x\\\"y\"", "POST-ACCUM", "keyword.other.gsql"),
    ("      POST-ACCUM s.name = \"x\\\"y\"", "name", "variable.other.property.gsql"),
    ("      POST-ACCUM s.name = \"x\\\"y\"", "\\\"", "constant.character.escape.gsql"),
    ("  IF x > 1 THEN PRINT 1.5; ELSE PRINT TRUE; END;", "IF", "keyword.control.gsql"),
    ("  IF x > 1 THEN PRINT 1.5; ELSE PRINT TRUE; END;", "1.5", "constant.numeric.float.gsql"),
    ("  IF x > 1 THEN PRINT 1.5; ELSE PRINT TRUE; END;", "TRUE", "constant.language.boolean.gsql"),
    ("  WHERE x NOT IN @@s AND y IS NULL // done", "NOT", "keyword.operator.word.gsql"),
    ("  WHERE x NOT IN @@s AND y IS NULL // done", "// done", "comment.line.double-slash.gsql"),
    ("# hash comment", "# hash comment", "comment.line.number-sign.gsql"),
    ("LOAD f TO VERTEX Person VALUES ($0, $\"name\") USING SEPARATOR=\",\";", "$\"name\"", "variable.language.column.gsql"),
    ("CREATE DIRECTED EDGE Follows (FROM Person, TO Person)", "Follows", "entity.name.type.gsql"),
    ("CREATE DIRECTED EDGE Follows (FROM Person, TO Person)", "DIRECTED", "storage.modifier.gsql"),
    ("INSTALL QUERY -OPTIMIZE pr", "pr", "entity.name.function.gsql"),
    ("TYPEDEF TUPLE <INT score, VERTEX v> Rec;", "Rec", "entity.name.type.tuple.gsql"),
    ("  S = {Person.*};", "Person", "entity.name.type.gsql"),
    ("/* block", "/*", "punctuation.definition.comment.begin.gsql"),
    ("  R = SELECT t FROM Start:s -(Knows>:e)- Person:t", "Knows", "entity.name.type.edge.gsql"),
    ("  R = SELECT t FROM Start:s -(<Likes)- Post:t", "Likes", "entity.name.type.edge.gsql"),
    ("  DeviationAccum @@spread;", "DeviationAccum", "support.type.accumulator.gsql"),
    ("CREATE VERTEX U (PRIMARY_ID id UINT, age UINT NULLABLE)", "NULLABLE", "keyword.other.gsql"),
    ("LOAD f TO VERTEX E VALUES ($\"indent\":\"length\");", "$\"indent\":\"length\"", "variable.language.column.gsql"),
    ("  PRINT round(x, 2), @@a ^ @@b;", "round", "support.function.builtin.gsql"),
    ("  PRINT round(x, 2), @@a ^ @@b;", "^", "keyword.operator.gsql"),
    # openCypher-style patterns put the alias first.
    ("  MATCH (s:Person)-[e:Knows]->(t:Person)", "s", "variable.other.alias.gsql"),
    ("  MATCH (s:Person)-[e:Knows]->(t:Person)", "Person", "entity.name.type.gsql"),
    ("  MATCH (s:Person)-[e:Knows]->(t:Person)", "e", "variable.other.alias.gsql"),
    ("  MATCH (s:Person)-[e:Knows]->(t:Person)", "Knows", "entity.name.type.edge.gsql"),
    ("  MATCH (s:Person)-[e:Knows]->(t:Person)", "t", "variable.other.alias.gsql"),
    ("  R = SELECT t FROM (s:Person)-[:Knows]-(t:Person);", "s", "variable.other.alias.gsql"),
    # Attributes named like keywords.
    ("  PRINT t.description, s.order, t.year;", "description", "variable.other.property.gsql"),
    ("  PRINT t.description, s.order, t.year;", "order", "variable.other.property.gsql"),
    ("  PRINT t.description, s.order, t.year;", "year", "variable.other.property.gsql"),
    ("  PRINT @@s.size(), s.count();", "count", "entity.name.function.member.gsql"),
    # A negative value is not a command option.
    ("  x = -y;", "-", "keyword.operator.gsql"),
    ("  RETURN -y;", "-", "keyword.operator.gsql"),
    ("RUN QUERY -d q(1)", "-d", "variable.parameter.option.gsql"),
    ("CLEAR GRAPH STORE -HARD", "-HARD", "variable.parameter.option.gsql"),
    ("  R = SELECT t FROM Me:m -(Friendship:e)- Person:t", "Friendship", "entity.name.type.edge.gsql"),
    ("  R = SELECT t FROM Me:m -(Friendship:e)- Person:t", "e", "variable.other.alias.gsql"),
    # Nested type arguments in a tuple.
    ("TYPEDEF TUPLE<VERTEX<Person> v, FLOAT score> Rec;", "Rec", "entity.name.type.tuple.gsql"),
    ("TYPEDEF TUPLE<VERTEX<Person> v, FLOAT score> Rec;", "FLOAT", "storage.type.gsql"),
]


def run_tests():
    failures = 0
    for line, token, expected in ASSERTIONS:
        actual = scope_of(line, token)
        if actual != expected:
            failures += 1
            print(f"FAIL {token!r} in {line!r}: expected {expected}, got {actual}")
    # Multi-line block comments carry their scope across lines.
    lines = tokenize("/* a\n b */ SELECT")
    if lines[1][0][1][-1] != "comment.block.gsql" or lines[1][-1][1][-1] != "keyword.other.gsql":
        failures += 1
        print("FAIL multi-line block comment:", lines)
    print(f"{len(ASSERTIONS) + 1 - failures} passed, {failures} failed")
    return failures


if __name__ == "__main__":
    if sys.argv[1:] == ["--test"]:
        sys.exit(1 if run_tests() else 0)
    for path in sys.argv[1:]:
        for number, line in enumerate(tokenize(Path(path).read_text()), 1):
            print(f"{number:4}: " + "  ".join(f"{t!r}={s[-1] if s else '-'}" for t, s in line if t.strip()))
