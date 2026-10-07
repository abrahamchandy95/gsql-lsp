//! Documentation and signatures for GSQL keywords, types, accumulators and
//! built-in functions.

/// Where a built-in function may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Math,
    String,
    Datetime,
    Aggregate,
    Conversion,
    List,
    Vertex,
    Json,
    Vector,
    Context,
    Loading,
    Misc,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Math => "math function",
            Category::String => "string function",
            Category::Datetime => "datetime function",
            Category::Aggregate => "aggregate function",
            Category::Conversion => "conversion function",
            Category::List => "list function",
            Category::Vertex => "vertex function",
            Category::Json => "JSON function",
            Category::Vector => "vector function",
            Category::Context => "context function",
            Category::Loading => "loading job function",
            Category::Misc => "built-in function",
        }
    }
}

#[derive(Debug)]
pub struct Function {
    pub name: &'static str,
    pub params: &'static [&'static str],
    pub returns: &'static str,
    pub doc: &'static str,
    pub category: Category,
}

/// Functions documented both for queries and for loading jobs.
const SHARED: &[&str] = &["gsql_uuid_v4", "split"];

impl Function {
    pub fn in_queries(&self) -> bool {
        self.category != Category::Loading || SHARED.contains(&self.name)
    }

    pub fn in_loading_jobs(&self) -> bool {
        self.category == Category::Loading || SHARED.contains(&self.name)
    }

    pub fn signature(&self) -> String {
        let mut signature = format!("{}({})", self.name, self.params.join(", "));
        if !self.returns.is_empty() {
            signature.push_str(" -> ");
            signature.push_str(self.returns);
        }
        signature
    }
}

#[derive(Debug)]
pub struct Method {
    pub name: &'static str,
    pub params: &'static [&'static str],
    pub returns: &'static str,
    pub doc: &'static str,
    /// Modifies its accumulator; restricted to certain clauses (see `features::rules`).
    pub mutator: bool,
}

impl Method {
    pub fn signature(&self) -> String {
        let mut signature = format!(".{}({})", self.name, self.params.join(", "));
        if !self.returns.is_empty() {
            signature.push_str(" -> ");
            signature.push_str(self.returns);
        }
        signature
    }
}

#[derive(Debug)]
pub struct Accumulator {
    pub name: &'static str,
    pub syntax: &'static str,
    pub doc: &'static str,
    pub methods: &'static [Method],
}

macro_rules! f {
    ($cat:ident, $name:literal, [$($p:literal),*], $ret:literal, $doc:literal) => {
        Function { name: $name, params: &[$($p),*], returns: $ret, doc: $doc, category: Category::$cat }
    };
}

macro_rules! m {
    ($name:literal, [$($p:literal),*], $ret:literal, $doc:literal) => {
        Method { name: $name, params: &[$($p),*], returns: $ret, doc: $doc, mutator: false }
    };
}

/// An accumulator method that modifies the accumulator.
macro_rules! mutator {
    ($name:literal, [$($p:literal),*], $ret:literal, $doc:literal) => {
        Method { name: $name, params: &[$($p),*], returns: $ret, doc: $doc, mutator: true }
    };
}

pub static FUNCTIONS: &[Function] = &[
    // Math
    f!(Math, "abs", ["num"], "number", "Absolute value of `num`."),
    f!(Math, "acos", ["num"], "FLOAT", "Arc cosine of `num`, in radians."),
    f!(Math, "asin", ["num"], "FLOAT", "Arc sine of `num`, in radians."),
    f!(Math, "atan", ["num"], "FLOAT", "Arc tangent of `num`, in radians."),
    f!(
        Math,
        "atan2",
        ["y", "x"],
        "FLOAT",
        "Arc tangent of `y / x`, using the signs of both arguments to pick the quadrant."
    ),
    f!(Math, "ceil", ["num"], "INT", "Smallest integer that is not less than `num`."),
    f!(Math, "cos", ["num"], "FLOAT", "Cosine of `num` (radians)."),
    f!(Math, "cosh", ["num"], "FLOAT", "Hyperbolic cosine of `num`."),
    f!(Math, "cot", ["num"], "DOUBLE", "Cotangent of `num` (radians)."),
    f!(Math, "degrees", ["num"], "DOUBLE", "Converts an angle from radians to degrees."),
    f!(Math, "exp", ["num"], "FLOAT", "`e` raised to the power `num`."),
    f!(Math, "floor", ["num"], "INT", "Largest integer that is not greater than `num`."),
    f!(Math, "fmod", ["numer", "denom"], "FLOAT", "Floating-point remainder of `numer / denom`."),
    f!(Math, "ldexp", ["x", "exp"], "FLOAT", "`x` multiplied by 2 raised to `exp`."),
    f!(
        Math,
        "log",
        ["num"],
        "DOUBLE",
        "Natural logarithm of `num`.\n\nAs a statement, `LOG(condition, arg, ...)` writes its arguments to the GPE log when `condition` is true."
    ),
    f!(Math, "log2", ["num"], "DOUBLE", "Base-2 logarithm of `num`."),
    f!(Math, "log10", ["num"], "FLOAT", "Base-10 logarithm of `num`."),
    f!(Math, "PI", [], "DOUBLE", "The value of π."),
    f!(Math, "pow", ["base", "exp"], "FLOAT", "`base` raised to the power `exp`."),
    f!(Math, "radians", ["num"], "DOUBLE", "Converts an angle from degrees to radians."),
    f!(Math, "rand", ["[seed]"], "DOUBLE", "A pseudo-random number between 0 and 1, optionally seeded."),
    f!(
        Math,
        "round",
        ["num", "[integer]"],
        "number",
        "Rounds `num` to the nearest integer, or to `integer` digits after the decimal point (negative places round to the left of it)."
    ),
    f!(Math, "sign", ["num"], "INT", "1, -1 or 0 for a positive, negative or zero `num`."),
    f!(Math, "sin", ["num"], "FLOAT", "Sine of `num` (radians)."),
    f!(Math, "sinh", ["num"], "FLOAT", "Hyperbolic sine of `num`."),
    f!(Math, "sqrt", ["num"], "FLOAT", "Square root of `num`."),
    f!(Math, "square", ["num"], "number", "`num` squared."),
    f!(Math, "tan", ["num"], "FLOAT", "Tangent of `num` (radians)."),
    f!(Math, "tanh", ["num"], "FLOAT", "Hyperbolic tangent of `num`."),
    f!(
        Math,
        "trunc",
        ["num", "[decimal_places]"],
        "DOUBLE",
        "Truncates `num` toward zero, keeping `decimal_places` digits after the decimal point (0 by default)."
    ),
    // Conversion
    f!(
        Conversion,
        "float_to_int",
        ["num"],
        "INT",
        "Converts a floating-point number to an integer by truncating the fractional part."
    ),
    f!(Conversion, "str_to_int", ["str"], "INT", "Parses a string as an integer."),
    f!(Conversion, "to_string", ["num"], "STRING", "Converts a value to its string representation."),
    f!(Conversion, "toBoolean", ["input"], "BOOL", "Converts a string (or boolean) to TRUE or FALSE."),
    f!(Conversion, "toFloat", ["input"], "FLOAT", "Converts a number or string to a floating-point number."),
    f!(Conversion, "toInteger", ["input"], "INT", "Converts a floating-point number or string to an integer."),
    // String
    f!(String, "lower", ["str"], "STRING", "Converts `str` to lower case."),
    f!(String, "upper", ["str"], "STRING", "Converts `str` to upper case."),
    f!(
        String,
        "trim",
        ["[ [ LEADING | TRAILING | BOTH ] [removal_char FROM] ] str"],
        "STRING",
        "Removes leading and/or trailing characters (whitespace by default) from `s`."
    ),
    f!(String, "ltrim", ["str", "[set]"], "STRING", "Removes leading characters (whitespace by default) from `str`."),
    f!(String, "rtrim", ["str", "[set]"], "STRING", "Removes trailing characters (whitespace by default) from `str`."),
    f!(String, "length", ["str"], "INT", "Number of characters in `str`."),
    f!(
        String,
        "substr",
        ["str", "start", "[length]"],
        "STRING",
        "Substring of `str` beginning at `start` (0-based), optionally limited to `length` characters."
    ),
    f!(
        String,
        "replace",
        ["str", "str_to_replace", "[replacement_str]"],
        "STRING",
        "Replaces every occurrence of `str_to_replace` in `str` with `replacement_str`, or removes it when `replacement_str` is omitted."
    ),
    f!(
        String,
        "repeat",
        ["str", "repetitions"],
        "STRING",
        "`str` repeated `repetitions` times, with nothing in between."
    ),
    f!(String, "reverse", ["str"], "STRING", "`str` with its characters in reverse order."),
    f!(
        String,
        "insert",
        ["str1", "position", "[number]", "str2"],
        "STRING",
        "Inserts `str2` into `str1` at `position` (0-based), replacing `number` characters of `str1`."
    ),
    f!(
        String,
        "find_in_set",
        ["str", "str_list"],
        "INT",
        "Position of `str` in the comma-separated string `str_list`."
    ),
    f!(String, "left", ["str", "number_of_chars"], "STRING", "The first `number_of_chars` characters of `str`."),
    f!(String, "right", ["str", "number_of_chars"], "STRING", "The last `number_of_chars` characters of `str`."),
    f!(
        String,
        "lpad",
        ["str", "padded_length", "[pad_str]"],
        "STRING",
        "Pads `str` on the left with `pad_str` (a space by default) to `padded_length` characters."
    ),
    f!(
        String,
        "rpad",
        ["str", "padded_length", "[pad_str]"],
        "STRING",
        "Pads `str` on the right with `pad_str` (a space by default) to `padded_length` characters."
    ),
    f!(
        String,
        "instr",
        ["str", "substr", "[position]", "[occurrence]"],
        "INT",
        "Position of an occurrence of `substr` in `str`, searching from `position`; 0 when not found."
    ),
    f!(String, "ascii", ["str"], "INT", "Character code of the first character of `str`."),
    f!(String, "chr", ["n"], "STRING", "The character with the given character code."),
    f!(String, "soundex", ["str"], "STRING", "Soundex code of `str`."),
    f!(String, "difference", ["str1", "str2"], "INT", "Similarity of the Soundex codes of two strings."),
    f!(String, "space", ["n"], "STRING", "A string of `n` spaces."),
    f!(
        String,
        "translate",
        ["str_origin", "characters", "translations"],
        "STRING",
        "Replaces each character of `str_origin` found in `characters` with the character at the same position in `translations`."
    ),
    // Datetime
    f!(Datetime, "now", [], "DATETIME", "The current date and time."),
    f!(
        Datetime,
        "to_datetime",
        ["str"],
        "DATETIME",
        "Parses a string such as `\"2024-01-31 12:00:00\"` as a DATETIME."
    ),
    f!(Datetime, "year", ["date"], "INT", "Year component of a DATETIME."),
    f!(Datetime, "month", ["date"], "INT", "Month component (1-12) of a DATETIME."),
    f!(Datetime, "day", ["date"], "INT", "Day-of-month component of a DATETIME."),
    f!(Datetime, "hour", ["date"], "INT", "Hour component (0-23) of a DATETIME."),
    f!(Datetime, "minute", ["date"], "INT", "Minute component of a DATETIME."),
    f!(Datetime, "second", ["date"], "INT", "Second component of a DATETIME."),
    f!(
        Datetime,
        "datetime_add",
        ["date", "INTERVAL int_value time_unit"],
        "DATETIME",
        "Adds an interval to a DATETIME, e.g. `datetime_add(d, INTERVAL 3 DAY)`. Units: YEAR, MONTH, DAY, HOUR, MINUTE, SECOND."
    ),
    f!(
        Datetime,
        "datetime_sub",
        ["date", "INTERVAL int_value time_unit"],
        "DATETIME",
        "Subtracts an interval from a DATETIME, e.g. `datetime_sub(d, INTERVAL 1 HOUR)`."
    ),
    f!(
        Datetime,
        "datetime_diff",
        ["date1", "date2"],
        "INT",
        "Number of seconds from `date2` to `date1` (`dt1 - dt2`)."
    ),
    f!(Datetime, "datetime_to_epoch", ["date"], "INT", "Seconds (not milliseconds) since the Unix epoch."),
    f!(
        Datetime,
        "epoch_to_datetime",
        ["int_value"],
        "DATETIME",
        "Converts seconds (not milliseconds) since the Unix epoch to a DATETIME."
    ),
    f!(
        Datetime,
        "datetime_format",
        ["date", "[str]"],
        "STRING",
        "Formats a DATETIME with a strftime-style format (default `\"%Y-%m-%d %H:%M:%S\"`)."
    ),
    // Aggregates and collections
    f!(Aggregate, "count", ["[DISTINCT] setExp"], "INT", "Number of elements in a set, bag or list expression."),
    f!(Aggregate, "sum", ["[DISTINCT] setExp"], "number", "Sum of the elements of a numeric collection."),
    f!(
        Aggregate,
        "min",
        ["[DISTINCT] setExp"],
        "number",
        "Smallest element of a collection.\n\nIn a loading job, `REDUCE(min(arg))` keeps the smallest value loaded."
    ),
    f!(
        Aggregate,
        "max",
        ["[DISTINCT] setExp"],
        "number",
        "Largest element of a collection.\n\nIn a loading job, `REDUCE(max(arg))` keeps the largest value loaded."
    ),
    f!(Aggregate, "avg", ["[DISTINCT] setExp"], "DOUBLE", "Average of the elements of a numeric collection."),
    f!(
        Aggregate,
        "stdev",
        ["[DISTINCT] setExp"],
        "DOUBLE",
        "Standard deviation of a numeric set or bag, treating it as a sample."
    ),
    f!(
        Aggregate,
        "stdevp",
        ["[DISTINCT] setExp"],
        "DOUBLE",
        "Standard deviation of a numeric set or bag, treating it as the whole population."
    ),
    f!(Aggregate, "isempty", ["collection"], "BOOL", "Whether a set, bag or list expression has no elements."),
    f!(Aggregate, "coalesce", ["exp", "exp ...]"], "any", "The first argument that is not NULL."),
    f!(Aggregate, "reset_collection_accum", ["accumulator"], "", "Resets a collection accumulator to its empty state."),
    // Lists
    f!(List, "head", ["list"], "element", "The first element of a ListAccum (`list.get(0)`)."),
    f!(List, "last", ["list"], "element", "The last element of a ListAccum."),
    f!(List, "tail", ["list"], "ListAccum", "A copy of a ListAccum without its first element."),
    f!(List, "size", ["list"], "UINT", "Number of elements in a ListAccum."),
    f!(
        List,
        "range",
        ["start", "end", "[step]"],
        "ListAccum<INT>",
        "Integers from `start` up to `end` (inclusive) in steps of `step` (1 by default). `RANGE[start, end]` iterates the same values in FOREACH."
    ),
    f!(
        List,
        "split",
        ["original", "splitDelimiter"],
        "ListAccum<STRING>",
        "Splits a string at each `splitDelimiter`.\n\nIn a loading job, `split(column, separator)` loads a LIST or SET attribute and `split(column, key_value_separator, separator)` a MAP attribute."
    ),
    // Vertices
    f!(Vertex, "getvid", ["v"], "INT", "The internal id of vertex `v`."),
    f!(Vertex, "to_vertex", ["id", "vertex_type"], "VERTEX", "Looks up a vertex by primary id and type name."),
    f!(
        Vertex,
        "to_vertex_set",
        ["ids", "vertex_type"],
        "vertex set",
        "Converts a collection of primary ids into a vertex set of the given type."
    ),
    f!(
        Vertex,
        "selectVertex",
        ["file_path", "id_column", "type_column", "separator", "header"],
        "vertex set",
        "Reads vertex ids from a file to build a seed set, e.g. `{SelectVertex(\"f.csv\", $0, Person, \",\", true)}`."
    ),
    f!(
        Vertex,
        "evaluate",
        ["expressionStr", "[typeStr]"],
        "any",
        "Evaluates an expression given as a string at run time, returning a value of the named type."
    ),
    f!(Vertex, "elementId", ["vertex_or_edge"], "STRING", "An internal string id of a vertex or an edge."),
    f!(
        Vertex,
        "vectorSearch",
        ["vertex_attributes", "query_vector", "k", "[options]"],
        "vertex set",
        "Top-k vector similarity search over embedding attributes (TigerGraph 4.2+)."
    ),
    f!(
        Vector,
        "tg_similarity_accum",
        ["vector_a", "vector_b", "metric"],
        "DOUBLE",
        "Similarity or distance of two ListAccum vectors; `metric` is \"COSINE\", \"EUCLIDEAN\", \"JACCARD\", \"OVERLAP\" or \"PEARSON\" (Graph Data Science Library)."
    ),
    f!(Context, "current_roles", [], "SetAccum<STRING>", "The names of the roles granted to the current user."),
    f!(
        Context,
        "is_granted_to_current_roles",
        ["roleName"],
        "BOOL",
        "Whether the current user holds the role `roleName`."
    ),
    // JSON
    f!(Json, "parse_json_object", ["str"], "JSONOBJECT", "Parses a string as a JSON object."),
    f!(Json, "parse_json_array", ["str"], "JSONARRAY", "Parses a string as a JSON array."),
    // Loading jobs
    f!(Loading, "gsql_concat", ["string1", "string2", "..."], "STRING", "Concatenates its arguments."),
    f!(
        Loading,
        "gsql_to_bool",
        ["in_string"],
        "BOOL",
        "TRUE if the token is \"t\" or \"true\" (case-insensitive), FALSE otherwise."
    ),
    f!(
        Loading,
        "gsql_to_uint",
        ["in_string"],
        "UINT",
        "Converts an unsigned integer token (or a non-negative float, truncated) to UINT."
    ),
    f!(Loading, "gsql_to_int", ["in_string"], "INT", "Converts an integer token (or a float, truncated) to INT."),
    f!(
        Loading,
        "gsql_ts_to_epoch_seconds",
        ["timestamp"],
        "UINT",
        "Converts a timestamp (`%Y-%m-%d %H:%M:%S`, `%Y/%m/%d %H:%M:%S` or `%Y-%m-%dT%H:%M:%S.000z`) to epoch seconds."
    ),
    f!(
        Loading,
        "gsql_ts_to_epoch_seconds_legacy",
        ["timestamp", "[defaultTimestamp]"],
        "UINT",
        "Like `gsql_ts_to_epoch_seconds`, returning `defaultTimestamp` (0 unless given) for invalid timestamps and those before 1970."
    ),
    f!(
        Loading,
        "gsql_ts_to_epoch_seconds_signed",
        ["timestamp", "[defaultTimestamp]"],
        "INT",
        "Epoch seconds of a timestamp between years 1 and 9999 (negative before 1970), or `defaultTimestamp` (0 unless given) when invalid."
    ),
    f!(
        Loading,
        "gsql_current_time_epoch",
        ["0"],
        "UINT",
        "The current time in epoch seconds. It takes one INT argument that is ignored; by convention it is 0."
    ),
    f!(
        Loading,
        "gsql_current_datetime",
        [],
        "DATETIME",
        "The time at which loading of the current vertex or edge starts."
    ),
    f!(
        Loading,
        "gsql_split_by_space",
        ["token"],
        "STRING",
        "Replaces each space in a token with ASCII 30, the GSQL list separator."
    ),
    f!(Loading, "gsql_upper", ["in_string"], "STRING", "Converts a token to upper case."),
    f!(Loading, "gsql_lower", ["in_string"], "STRING", "Converts a token to lower case."),
    f!(Loading, "gsql_trim", ["in_string"], "STRING", "Removes leading and trailing whitespace."),
    f!(Loading, "gsql_ltrim", ["in_string"], "STRING", "Removes leading whitespace."),
    f!(Loading, "gsql_rtrim", ["in_string"], "STRING", "Removes trailing whitespace."),
    f!(Loading, "gsql_reverse", ["in_string"], "STRING", "The token with its characters in reverse order."),
    f!(
        Loading,
        "gsql_substring",
        ["str", "begin_index", "[length]"],
        "STRING",
        "The substring starting at `begin_index` (0-based), optionally limited to `length` characters."
    ),
    f!(Loading, "gsql_find", ["str", "substr"], "INT", "Start index of `substr` in the token, or -1."),
    f!(Loading, "gsql_length", ["str"], "INT", "Length of the token."),
    f!(
        Loading,
        "gsql_replace",
        ["str", "old_token", "new_token", "[max]"],
        "STRING",
        "Replaces occurrences of `old_token` with `new_token`, at most `max` times when given."
    ),
    f!(
        Loading,
        "gsql_regex_replace",
        ["str", "regex", "replace_substr"],
        "STRING",
        "Replaces every match of `regex` in the token with `replace_substr`."
    ),
    f!(Loading, "gsql_regex_match", ["str", "regex"], "BOOL", "Whether a token matches a regular expression."),
    f!(Loading, "gsql_year", ["timestamp"], "INT", "Four-digit year of a timestamp."),
    f!(Loading, "gsql_month", ["timestamp"], "INT", "Month (1-12) of a timestamp."),
    f!(Loading, "gsql_day", ["timestamp"], "INT", "Day of the month (1-31) of a timestamp."),
    f!(Loading, "gsql_year_epoch", ["epoch"], "INT", "Four-digit year of a time in epoch seconds."),
    f!(Loading, "gsql_month_epoch", ["epoch"], "INT", "Month (1-12) of a time in epoch seconds."),
    f!(Loading, "gsql_day_epoch", ["epoch"], "INT", "Day of the month (1-31) of a time in epoch seconds."),
    f!(Loading, "gsql_uuid_v4", [], "STRING", "A random version-4 UUID. Also available in queries."),
    f!(Loading, "gsql_is_true", ["token"], "BOOL", "Whether a token is \"true\" or \"t\" (case-insensitive)."),
    f!(Loading, "gsql_is_false", ["token"], "BOOL", "Whether a token is \"false\" or \"f\" (case-insensitive)."),
    f!(
        Loading,
        "gsql_is_not_empty_string",
        ["token"],
        "BOOL",
        "Whether a token is non-empty after removing whitespace (WHERE clauses)."
    ),
    f!(
        Loading,
        "gsql_is_not_empty",
        ["token"],
        "BOOL",
        "Whether a token is non-empty after removing whitespace (WHERE clauses)."
    ),
    f!(Loading, "gsql_token_equal", ["string1", "string2"], "BOOL", "Case-sensitive token comparison."),
    f!(Loading, "gsql_token_ignore_case_equal", ["string1", "string2"], "BOOL", "Case-insensitive token comparison."),
    f!(Loading, "to_int", ["token"], "INT", "Converts a token to an integer (WHERE clauses)."),
    f!(Loading, "to_float", ["token"], "FLOAT", "Converts a token to a floating-point number (WHERE clauses)."),
    f!(Loading, "concat", ["string1", "string2"], "STRING", "Concatenates two tokens (WHERE clauses)."),
    f!(Loading, "token_len", ["token"], "INT", "Length of a token (WHERE clauses)."),
    f!(
        Loading,
        "flatten",
        ["column_to_be_split", "group_separator", "[sub_field_separator]", "number_of_sub_fields"],
        "",
        "Splits a multi-value column into rows of a TEMP_TABLE."
    ),
    f!(
        Loading,
        "flatten_json_array",
        ["array_name", "[sub_obj_1]", "..."],
        "",
        "Splits a JSON array column into rows of a TEMP_TABLE, optionally extracting fields of each element."
    ),
    f!(
        Loading,
        "reduce",
        ["reducer(expression)"],
        "",
        "Combines the loaded value with the existing attribute value, e.g. `REDUCE(add($2))`. Reducers: add, max, min, and, or, overwrite, ignore_if_exists."
    ),
    f!(
        Loading,
        "add",
        ["arg"],
        "",
        "Reducer: sums numbers, concatenates strings, and adds elements to LIST, SET and MAP attributes."
    ),
    f!(Loading, "and", ["arg"], "", "Reducer: logical AND of BOOL values, bitwise AND of integers."),
    f!(Loading, "or", ["arg"], "", "Reducer: logical OR of BOOL values, bitwise OR of integers."),
    f!(Loading, "overwrite", ["arg"], "", "Reducer: replaces the existing value with the loaded one."),
    f!(Loading, "ignore_if_exists", ["arg"], "", "Reducer: keeps an existing value and loads only missing ones."),
];

pub static VERTEX_METHODS: &[Method] = &[
    m!(
        "outdegree",
        ["[edgeType]"],
        "INT",
        "Number of outgoing edges, optionally of one edge type (a type name string or a set of names)."
    ),
    m!(
        "neighbors",
        ["[edgeType]"],
        "BagAccum<VERTEX>",
        "The vertices connected by outgoing edges, optionally of one edge type."
    ),
    m!(
        "neighborAttribute",
        ["edgeType", "targetVertexType", "attrName"],
        "BagAccum",
        "An attribute of the neighbors reached through `edgeType`."
    ),
    m!("edgeAttribute", ["edgeType", "attrName"], "BagAccum", "An attribute of the outgoing edges of `edgeType`."),
    m!(
        "getAttr",
        ["attrName", "attrType"],
        "any",
        "Reads an attribute whose name is only known at run time, e.g. `v.getAttr(attr, \"INT\")`."
    ),
    m!("setAttr", ["attrName", "newValue"], "", "Writes an attribute whose name is only known at run time."),
];

pub static EDGE_METHODS: &[Method] = &[
    m!("isDirected", [], "BOOL", "Whether the edge type is directed."),
    m!("getAttr", ["attrName", "attrType"], "any", "Reads an attribute whose name is only known at run time."),
    m!("setAttr", ["attrName", "attrNewValue"], "", "Writes an attribute whose name is only known at run time."),
];

pub static VERTEX_SET_METHODS: &[Method] = &[m!("size", [], "INT", "Number of vertices in the set.")];

/// Methods of LIST variables and parameters (the accumulators have their own).
pub static LIST_METHODS: &[Method] = &[
    m!("size", [], "INT", "Number of elements."),
    m!("contains", ["value"], "BOOL", "Whether the list has an element equal to `value`."),
    m!("get", ["idx"], "element", "The element at the zero-based position `idx`."),
];

/// Methods of SET and BAG variables and parameters.
pub static SET_METHODS: &[Method] = &[
    m!("size", [], "INT", "Number of elements."),
    m!("contains", ["value"], "BOOL", "Whether the collection has an element equal to `value`."),
];

/// Methods of MAP variables and parameters.
pub static MAP_METHODS: &[Method] = &[
    m!("size", [], "INT", "Number of entries."),
    m!("containsKey", ["key"], "BOOL", "Whether the map has an entry for `key`."),
    m!("get", ["key"], "value", "The value of `key`."),
];

pub static JSON_OBJECT_METHODS: &[Method] = &[
    m!("containsKey", ["keyStr"], "BOOL", "Whether the object has `keyStr`."),
    m!("getInt", ["keyStr"], "INT", "The integer value of `keyStr`."),
    m!("getDouble", ["keyStr"], "DOUBLE", "The floating-point value of `keyStr`."),
    m!("getString", ["keyStr"], "STRING", "The string value of `keyStr`."),
    m!("getBool", ["keyStr"], "BOOL", "The boolean value of `keyStr`."),
    m!("getJsonObject", ["keyStr"], "JSONOBJECT", "The object value of `keyStr`."),
    m!("getJsonArray", ["keyStr"], "JSONARRAY", "The array value of `keyStr`."),
];

pub static JSON_ARRAY_METHODS: &[Method] = &[
    m!("size", [], "INT", "Number of elements."),
    m!("getInt", ["idx"], "INT", "The integer element at `idx`."),
    m!("getDouble", ["idx"], "DOUBLE", "The floating-point element at `idx`."),
    m!("getString", ["idx"], "STRING", "The string element at `idx`."),
    m!("getBool", ["idx"], "BOOL", "The boolean element at `idx`."),
    m!("getJsonObject", ["idx"], "JSONOBJECT", "The object element at `idx`."),
    m!("getJsonArray", ["idx"], "JSONARRAY", "The array element at `idx`."),
];

pub static FILE_METHODS: &[Method] =
    &[m!("println", ["value", "..."], "", "Writes the values, separated by commas, as one line of the file.")];

const SIZE: Method = m!("size", [], "INT", "Number of elements.");

/// The methods of the bitwise accumulators. Each type gets an array of its
/// own (the last doc differs, so the arrays are never merged): entries are
/// told apart by address.
macro_rules! bitwise_methods {
    ($clear:literal) => {
        &[
            mutator!("reset", [], "", "Sets all bits to 0."),
            m!("cardinality", [], "INT", "Number of bits set to 1."),
            m!("get", ["index"], "INT", "The bit (1 or 0) at `index`."),
            mutator!("set", ["[index, value]"], "", "Sets every bit to 1, or the bit at `index` to `value`."),
            mutator!("flip", ["from", "[to]"], "", "Flips the bit at `from`, or the bits from `from` to `to`."),
            mutator!("xor", ["accumulator"], "", "XORs the bits with another bitwise accumulator of the same length."),
            mutator!("and", ["accumulator"], "", "ANDs the bits with another bitwise accumulator of the same length."),
            mutator!("or", ["accumulator"], "", "ORs the bits with another bitwise accumulator of the same length."),
            mutator!("clear", [], "", $clear),
        ]
    };
}
const CLEAR: Method = mutator!("clear", [], "", "Removes all elements.");

pub static ACCUMULATORS: &[Accumulator] = &[
    Accumulator {
        name: "SumAccum",
        syntax: "SumAccum<INT | UINT | FLOAT | DOUBLE | STRING>",
        doc: "Running sum of numbers, or concatenation of strings. `+=` adds; `=` resets the value.",
        methods: &[],
    },
    Accumulator {
        name: "MaxAccum",
        syntax: "MaxAccum<type>",
        doc: "Keeps the largest value accumulated so far.",
        methods: &[],
    },
    Accumulator {
        name: "MinAccum",
        syntax: "MinAccum<type>",
        doc: "Keeps the smallest value accumulated so far.",
        methods: &[],
    },
    Accumulator {
        name: "AvgAccum",
        syntax: "AvgAccum",
        doc: "Running average of the accumulated numbers (a DOUBLE).",
        methods: &[],
    },
    Accumulator {
        name: "AndAccum",
        syntax: "AndAccum",
        doc: "Logical AND of the accumulated BOOL values; starts as TRUE.",
        methods: &[],
    },
    Accumulator {
        name: "OrAccum",
        syntax: "OrAccum",
        doc: "Logical OR of the accumulated BOOL values; starts as FALSE.",
        methods: &[],
    },
    Accumulator {
        name: "BitwiseAndAccum",
        syntax: "BitwiseAndAccum[<bits>]",
        doc: "Bitwise AND of the accumulated integers; 64 bits unless a length (a constant or a parameter) is given. Supports `&`, `|`, `^` and `~`.",
        methods: bitwise_methods!("Frees the memory of long and dynamic-length BitwiseAndAccum accumulators."),
    },
    Accumulator {
        name: "BitwiseOrAccum",
        syntax: "BitwiseOrAccum[<bits>]",
        doc: "Bitwise OR of the accumulated integers; 64 bits unless a length (a constant or a parameter) is given. Supports `&`, `|`, `^` and `~`.",
        methods: bitwise_methods!("Frees the memory of long and dynamic-length BitwiseOrAccum accumulators."),
    },
    Accumulator {
        name: "DeviationAccum",
        syntax: "DeviationAccum",
        doc: "Running standard deviation of the accumulated numbers, treating them as a sample (N - 1 denominator). `=` restarts it with one value.",
        methods: &[],
    },
    Accumulator {
        name: "DeviationPAccum",
        syntax: "DeviationPAccum",
        doc: "Running standard deviation of the accumulated numbers, treating them as the whole population (N denominator).",
        methods: &[],
    },
    Accumulator {
        name: "ListAccum",
        syntax: "ListAccum<type>",
        doc: "Ordered collection that keeps duplicates. `+=` appends an element or a list.",
        methods: &[
            SIZE,
            m!("contains", ["value"], "BOOL", "Whether the list contains `value`."),
            m!("get", ["index"], "element", "The element at `index` (0-based)."),
            mutator!("update", ["index", "value"], "", "Replaces the element at `index`."),
            mutator!("remove", ["index"], "", "Removes the element at `index`."),
            mutator!("removeOne", ["value"], "", "Removes the first occurrence of `value`."),
            mutator!("removeAll", ["value"], "", "Removes every occurrence of `value`."),
            CLEAR,
        ],
    },
    Accumulator {
        name: "SetAccum",
        syntax: "SetAccum<type>",
        doc: "Unordered collection of distinct elements. `+=` adds an element or a set; supports UNION, INTERSECT and MINUS.",
        methods: &[
            SIZE,
            m!("contains", ["value"], "BOOL", "Whether the set contains `value`."),
            mutator!("remove", ["value"], "", "Removes `value` from the set."),
            CLEAR,
        ],
    },
    Accumulator {
        name: "BagAccum",
        syntax: "BagAccum<type>",
        doc: "Unordered collection that keeps duplicates.",
        methods: &[
            SIZE,
            m!("contains", ["value"], "BOOL", "Whether the bag contains `value`."),
            mutator!("remove", ["value"], "", "Removes one occurrence of `value`."),
            mutator!("removeAll", ["value"], "", "Removes every occurrence of `value`."),
            m!(
                "filter",
                ["condition"],
                "BagAccum",
                "Keeps the elements for which `condition` holds, e.g. `v.neighbors().filter(...)`."
            ),
            CLEAR,
        ],
    },
    Accumulator {
        name: "MapAccum",
        syntax: "MapAccum<key_type, value_type>",
        doc: "Key-value map. The value may itself be an accumulator, so `+= (k -> v)` accumulates `v` into the value stored for `k`.",
        methods: &[
            SIZE,
            m!("get", ["key"], "value", "The value stored for `key`."),
            m!("containsKey", ["key"], "BOOL", "Whether the map has `key`."),
            mutator!("remove", ["key"], "", "Removes `key` and its value."),
            CLEAR,
        ],
    },
    Accumulator {
        name: "HeapAccum",
        syntax: "HeapAccum<tuple_type>([capacity,] field [ASC | DESC], ...)",
        doc: "Priority queue of tuples, ordered by the listed fields and holding at most `capacity` elements (unbounded without one).",
        methods: &[
            SIZE,
            m!("top", [], "tuple", "The first tuple in sort order, without removing it."),
            mutator!("pop", [], "tuple", "Removes and returns the first tuple in sort order."),
            mutator!("resize", ["capacity"], "", "Changes the capacity, dropping tuples that no longer fit."),
            CLEAR,
        ],
    },
    Accumulator {
        name: "GroupByAccum",
        syntax: "GroupByAccum<type key, ..., accumulator_type field, ...>",
        doc: "Groups by one or more keys, keeping a set of accumulators per group. Add with `+= (k1, k2 -> v1, v2)`.",
        methods: &[
            SIZE,
            m!("get", ["key", "..."], "group", "The accumulators of a group."),
            m!("containsKey", ["key", "..."], "BOOL", "Whether a group exists."),
            mutator!("remove", ["key", "..."], "", "Removes a group."),
            CLEAR,
        ],
    },
    Accumulator {
        name: "ArrayAccum",
        syntax: "ArrayAccum<accumulator_type> @@name[dim1][dim2]...",
        doc: "Fixed-size (multi-dimensional) array of accumulators, indexed with `@@name[i][j]`.",
        methods: &[
            SIZE,
            mutator!("reallocate", ["dim1", "..."], "", "Changes the array dimensions, discarding its contents."),
        ],
    },
];

pub static PRIMITIVE_TYPES: &[(&str, &str)] = &[
    ("INT", "Signed 64-bit integer."),
    ("UINT", "Unsigned 64-bit integer."),
    ("FLOAT", "Single-precision floating-point number."),
    ("DOUBLE", "Double-precision floating-point number."),
    ("BOOL", "Boolean: TRUE or FALSE."),
    ("STRING", "Character string. (`STRING COMPRESS` is deprecated.)"),
    ("DATETIME", "Date and time with one-second precision."),
    ("VERTEX", "A vertex. `VERTEX<Type>` restricts it to one vertex type."),
    ("EDGE", "An edge."),
    ("JSONOBJECT", "A JSON object, usually from `parse_json_object`."),
    ("JSONARRAY", "A JSON array, usually from `parse_json_array`."),
    ("LIST", "Ordered collection attribute: `LIST<type>`."),
    ("SET", "Collection of distinct values: `SET<type>`."),
    ("BAG", "Unordered collection with duplicates: `BAG<type>` (query parameters)."),
    ("MAP", "Key-value attribute: `MAP<key_type, value_type>`."),
    ("FILE", "Output file object: `FILE f (\"/path/out.csv\");`, written with `f.println(...)`."),
];

pub static CONSTANTS: &[(&str, &str)] = &[
    ("GSQL_INT_MAX", "Largest INT value."),
    ("GSQL_INT_MIN", "Smallest INT value."),
    ("GSQL_UINT_MAX", "Largest UINT value."),
];

pub static KEYWORDS: &[(&str, &str)] = &[
    (
        "ACCUM",
        "Clause of a SELECT block executed once for every matching edge (or path), in parallel. Statements are comma-separated.",
    ),
    (
        "POST-ACCUM",
        "Clause of a SELECT block executed once for every distinct vertex after ACCUM. Also spelled `POST_ACCUM`.",
    ),
    ("SELECT", "Starts a SELECT block: `Result = SELECT t FROM Start:s -(E:e)- T:t WHERE ... ACCUM ...;`"),
    ("FROM", "The pattern a SELECT block traverses, e.g. `Start:s -(Knows:e)- Person:t`."),
    ("WHERE", "Filter condition."),
    ("HAVING", "Filters the vertices of the result set after ACCUM and POST-ACCUM."),
    ("ORDER", "`ORDER BY expr [ASC | DESC], ...` sorts the result set."),
    ("LIMIT", "Limits the number of results (or bounds the iterations of a WHILE loop)."),
    ("SAMPLE", "Samples edges or targets: `SAMPLE 10 EDGE WHEN s.outdegree() > 100`."),
    ("CREATE", "Creates a schema object, query, job, graph, user or role."),
    ("QUERY", "A named, parameterized GSQL procedure."),
    ("INSTALL", "`INSTALL QUERY name` compiles queries into REST endpoints."),
    ("RUN", "Runs an installed query or a job."),
    ("INTERPRET", "Runs a query in interpreted mode without installing it."),
    ("DISTRIBUTED", "Runs the query in distributed mode across the cluster."),
    ("VERTEX", "A vertex type (in DDL) or the vertex value type `VERTEX<Type>`."),
    ("EDGE", "An edge type (in DDL) or the edge value type."),
    ("DIRECTED", "An edge type with a direction from source to target."),
    ("UNDIRECTED", "An edge type without a direction."),
    ("PRIMARY_ID", "Declares the primary id of a vertex type."),
    ("GRAPH", "A graph: a named set of vertex and edge types."),
    ("USE", "`USE GRAPH name` sets the graph for subsequent commands; `USE GLOBAL` returns to the global scope."),
    ("TYPEDEF", "`TYPEDEF TUPLE <type field, ...> Name` defines a tuple type."),
    ("TUPLE", "A user-defined record type."),
    ("IF", "`IF cond THEN ... ELSE IF cond THEN ... ELSE ... END`."),
    ("CASE", "`CASE WHEN cond THEN ... ELSE ... END` or `CASE expr WHEN value THEN ... END`."),
    ("WHILE", "`WHILE cond [LIMIT n] DO ... END` loop."),
    (
        "FOREACH",
        "`FOREACH x IN collection DO ... END` loop; also `FOREACH (k, v) IN @@map` and `FOREACH i IN RANGE[a, b]`.",
    ),
    ("BREAK", "Exits the innermost loop."),
    ("CONTINUE", "Starts the next iteration of the innermost loop."),
    ("RETURN", "Returns a value from a subquery declared with RETURNS."),
    ("RETURNS", "Declares the return type of a query called from other queries."),
    (
        "PRINT",
        "Adds values to the JSON output of the query. `PRINT expr AS key`, `PRINT S[S.attr]`, `PRINT ... TO_CSV file`.",
    ),
    ("LOG", "`LOG(condition, args...)` writes to the GPE log when the condition is true."),
    ("INSERT", "`INSERT INTO Type VALUES (...)` adds a vertex or an edge."),
    ("DELETE", "Deletes vertices or edges: `DELETE s FROM Start:s WHERE ...` or `DELETE (e)` inside ACCUM."),
    ("UPDATE", "`UPDATE s FROM Start:s SET s.attr = value WHERE ...`"),
    ("UNION", "Set union of vertex sets or collections."),
    ("INTERSECT", "Set intersection of vertex sets or collections."),
    ("MINUS", "Set difference of vertex sets or collections."),
    ("LOADING", "`CREATE LOADING JOB` defines how files are loaded into a graph."),
    ("LOAD", "`LOAD file TO VERTEX|EDGE Type VALUES (...)` maps input columns to a vertex or edge type."),
    ("DEFINE", "`DEFINE FILENAME`, `DEFINE HEADER` or `DEFINE INPUT_LINE_FILTER` in a loading job."),
    (
        "USING",
        "Loading options such as `SEPARATOR`, `HEADER`, `EOL` and `QUOTE`, or file arguments for RUN LOADING JOB.",
    ),
    ("SCHEMA_CHANGE", "`CREATE [GLOBAL] SCHEMA_CHANGE JOB` changes the schema of a graph."),
    ("TRY", "`TRY ... EXCEPTION WHEN ex THEN ... END` handles exceptions raised with RAISE."),
    ("RAISE", "Raises a user-defined exception: `RAISE ex(\"message\")`."),
    ("EXCEPTION", "Declares an exception (`EXCEPTION ex (40001);`) or starts the handlers of a TRY block."),
    ("SYNTAX", "Selects the query syntax version: `SYNTAX v1`, `v2` (pattern matching) or `v3` (openCypher-style)."),
    ("TO_CSV", "Writes PRINT output to a file as CSV."),
    ("INTERVAL", "A time interval for datetime_add/datetime_sub: `INTERVAL 3 DAY`."),
    ("RANGE", "`RANGE[start, end]` iterates integers in FOREACH; `.STEP(n)` sets the step."),
    ("STATIC", "A global accumulator whose value persists across query runs."),
    ("GRANT", "Grants a role or privileges to users or roles."),
    ("REVOKE", "Revokes a role or privileges."),
    ("SHOW", "Lists catalog objects."),
    ("DROP", "Removes a schema object, query, job, graph, user or role."),
    ("LS", "Lists the catalog of the current graph."),
    // Schema definition and schema change jobs
    (
        "ADD",
        "Adds a vertex or edge type, an attribute (`ALTER ... ADD ATTRIBUTE`), an edge pair or an index; in a global schema change job, `ADD VERTEX v TO GRAPH g` adds types to a graph.",
    ),
    (
        "ALTER",
        "Changes a vertex or edge type (`ALTER VERTEX v ADD ATTRIBUTE (...)`) or a graph (`ALTER GRAPH g ADD VERTEX v`) in a schema change job.",
    ),
    ("ATTRIBUTE", "An attribute of a vertex or edge type: `ADD ATTRIBUTE (name type)`, `DROP ATTRIBUTE (name)`."),
    ("DEFAULT", "Default value of an attribute or parameter: `age INT DEFAULT 0`."),
    ("NULLABLE", "Lets an attribute hold NULL: `age INT NULLABLE`."),
    ("PRIMARY", "`PRIMARY KEY` makes an attribute (or a list of attributes) the primary key of a vertex type."),
    ("KEY", "`PRIMARY KEY`: the primary key of a vertex type."),
    (
        "DISCRIMINATOR",
        "Attributes that tell apart several edges between the same two vertices (multi-edges): `DISCRIMINATOR(ts DATETIME)`.",
    ),
    ("PAIR", "A FROM/TO pair of an edge type: `ALTER EDGE e ADD PAIR (FROM A, TO B)`."),
    ("INDEX", "A secondary index on vertex attributes: `ALTER VERTEX v ADD INDEX name ON (attr)`."),
    (
        "WITH",
        "Options of a definition (`WITH STATS=\"none\"`, `WITH REVERSE_EDGE=\"name\"`), `CREATE GRAPH ... WITH ADMIN user`, or `PRINT ... WITH VECTOR`.",
    ),
    ("ADMIN", "`CREATE GRAPH g (...) WITH ADMIN user` makes `user` an administrator of the new graph."),
    (
        "AS",
        "Names a result (`PRINT x AS key`, `SELECT COUNT(p) AS n`), or derives a tag-based graph: `CREATE GRAPH g AS base:tag`.",
    ),
    ("CASCADE", "`DROP GRAPH g CASCADE` also drops the types used only by `g`."),
    ("GLOBAL", "The global scope, outside any graph: `USE GLOBAL`, `CREATE GLOBAL SCHEMA_CHANGE JOB`."),
    ("TAG", "Tag-based access control (deprecated): `ADD TAG name` declares a tag."),
    ("TAGS", "Tags applied by a LOAD statement (deprecated): `TAGS (t1, t2) BY OR`."),
    ("OVERWRITE", "`TAGS (...) BY OVERWRITE` replaces the existing tags of loaded vertices."),
    (
        "VECTOR",
        "A vector attribute (`ALTER VERTEX v ADD VECTOR ATTRIBUTE emb (DIMENSION=3)`), loading to one (`TO VECTOR ATTRIBUTE`), or `PRINT ... WITH VECTOR` to output vectors.",
    ),
    (
        "VIRTUAL",
        "`CREATE DIRECTED VIRTUAL EDGE name (FROM A, TO B, ...)` declares an in-memory edge type for the duration of a query; declare it at the top level of the query body.",
    ),
    ("DESCRIPTION", "Attaches a description: `ADD TAG t DESCRIPTION \"...\"`."),
    (
        "DATA_SOURCE",
        "An external data source (S3, Kafka, ...) for loading jobs: `CREATE DATA_SOURCE S3 name = \"{...}\"`.",
    ),
    ("PACKAGE", "A namespace for queries: `CREATE PACKAGE lib`; queries in it are called as `lib.query(...)`."),
    ("TEMPLATE", "`CREATE TEMPLATE QUERY pkg.name(...)` defines a query in a package."),
    ("FUNCTION", "`CREATE FUNCTION pkg.name(...)` defines a user-defined function in a package."),
    ("OPENCYPHER", "`CREATE OPENCYPHER QUERY` defines a query written in openCypher."),
    ("JOB", "A loading or schema change job."),
    ("REPLACE", "`CREATE OR REPLACE QUERY` replaces an existing query of the same name."),
    ("API", "`API(\"v2\")` selects the JSON output format of a query."),
    ("ALL", "Every query or job (`INSTALL QUERY ALL`, `DROP QUERY ALL`), or every item of a kind."),
    // Loading jobs
    ("FILENAME", "`DEFINE FILENAME f [= \"path\"];` declares a file variable of a loading job."),
    ("HEADER", "`DEFINE HEADER h = \"col1\", \"col2\";` names the columns of a file without a header line."),
    (
        "INPUT_LINE_FILTER",
        "`DEFINE INPUT_LINE_FILTER f = condition;` skips input lines (with `USING REJECT_LINE_RULE=f`).",
    ),
    (
        "TEMP_TABLE",
        "A temporary table of a loading job, filled by `LOAD ... TO TEMP_TABLE t (cols) VALUES (...)` and read by `LOAD TEMP_TABLE t`.",
    ),
    ("VALUES", "The values of a loaded or inserted vertex or edge: `VALUES ($0, $1)`."),
    ("OPTION", "Options of a LOAD destination: `OPTION (...)`."),
    // Queries
    ("FOR", "`FOR GRAPH g`: the graph a query or job works on."),
    (
        "PER",
        "`PER (alias, ...)` runs ACCUM (or POST-ACCUM) once per distinct binding of the listed aliases instead of once per match.",
    ),
    ("TARGET", "`SAMPLE n TARGET WHEN condition` samples target vertices."),
    ("PINNED", "`SAMPLE n% TARGET PINNED WHEN condition` keeps the sample fixed for each source vertex."),
    ("DISTINCT", "Removes duplicates: `SELECT DISTINCT ...`, `COUNT(DISTINCT x)`."),
    ("INTO", "`INSERT INTO Type VALUES (...)`, or `SELECT ... INTO Table` to store a SQL-like result table."),
    ("GROUP", "`GROUP BY expr, ...` groups the rows of a SQL-like SELECT."),
    ("OFFSET", "`LIMIT n OFFSET m` skips the first `m` results."),
    ("ASC", "Ascending sort order (the default) in ORDER BY and HeapAccum."),
    ("DESC", "Descending sort order in ORDER BY and HeapAccum."),
    ("THEN", "Starts the branch of IF ... THEN or WHEN ... THEN."),
    ("ELSE", "The fallback branch of IF or CASE; `ELSE IF` continues an IF."),
    (
        "WORKLOAD",
        "Workload queues limit the queries that run at once: `LIST WORKLOAD QUEUE`, `SHOW WORKLOAD QUEUE name`, `GRANT WORKLOAD QUEUE name TO USER u`.",
    ),
    (
        "QUEUE",
        "A workload queue: `LIST`, `GET` or `PUT ... FROM \"file.json\"` the queue configuration, `SHOW WORKLOAD QUEUE name`, `GRANT`/`REVOKE WORKLOAD QUEUE`.",
    ),
    ("GET", "`GET WORKLOAD QUEUE` prints the workload queue configuration."),
    ("PUT", "`PUT WORKLOAD QUEUE FROM \"file.json\"` replaces the workload queue configuration."),
    ("PROXY", "`SHOW PROXY USER name` shows a user that signs in through single sign-on (a proxy user)."),
    ("ELSE IF", "Another condition of an IF: `IF a THEN ... ELSE IF b THEN ... END` (one END for the whole IF)."),
    ("WHEN", "A branch of CASE (`WHEN condition THEN ...`), an exception handler of TRY, or the condition of SAMPLE."),
    ("END", "Closes IF, CASE, WHILE, FOREACH and TRY; in the shell, ends a BEGIN ... END multi-line block."),
    ("DO", "Starts the body of WHILE ... DO and FOREACH ... DO."),
    (
        "TO",
        "The target of a LOAD (`TO VERTEX v`), of an edge pair (`FROM A, TO B`), of a grant, or of `ADD VERTEX v TO GRAPH g`.",
    ),
    // Operators and functions
    ("AND", "Logical AND."),
    ("OR", "Logical OR."),
    ("NOT", "Logical negation; also `NOT IN`, `NOT LIKE`, `NOT BETWEEN` and `IS NOT`."),
    ("IN", "`x IN collection` tests membership; `FOREACH x IN collection` iterates."),
    ("IS", "`x IS NULL`, `x IS NOT NULL`, and in loading jobs `token IS EMPTY` / `token IS NUMERIC`."),
    ("BETWEEN", "`x BETWEEN low AND high` (inclusive)."),
    (
        "LIKE",
        "`s LIKE pattern` matches `%` (any characters) and `_` (one character); `ESCAPE` sets an escape character.",
    ),
    ("ESCAPE", "`s LIKE pattern ESCAPE \"\\\\\"` sets the escape character of a LIKE pattern."),
    ("EMPTY", "`token IS EMPTY` in a loading job WHERE clause."),
    ("NUMERIC", "`token IS NUMERIC` in a loading job WHERE clause."),
    ("LEADING", "`trim(LEADING chars FROM s)` removes characters at the start."),
    ("TRAILING", "`trim(TRAILING chars FROM s)` removes characters at the end."),
    ("BOTH", "`trim(BOTH chars FROM s)` removes characters at both ends (the default)."),
    ("YEAR", "An INTERVAL unit: `INTERVAL 1 YEAR`."),
    ("MONTH", "An INTERVAL unit: `INTERVAL 1 MONTH`."),
    ("DAY", "An INTERVAL unit: `INTERVAL 1 DAY`."),
    ("HOUR", "An INTERVAL unit: `INTERVAL 1 HOUR`."),
    ("MINUTE", "An INTERVAL unit: `INTERVAL 1 MINUTE`."),
    ("SECOND", "An INTERVAL unit: `INTERVAL 1 SECOND`."),
    // Shell and administration
    ("BEGIN", "Starts a multi-line block in the GSQL shell, ended with END (or abandoned with ABORT)."),
    ("ABORT", "Abandons a BEGIN ... END block in the shell; `ABORT LOADING JOB id` stops a loading job."),
    ("RESUME", "`RESUME LOADING JOB id` continues a stopped loading job."),
    ("CLEAR", "`CLEAR GRAPH STORE` deletes all data (`-HARD` also removes the data files)."),
    ("STORE", "`CLEAR GRAPH STORE` deletes all graph data."),
    ("EXPORT", "`EXPORT GRAPH ALL TO \"path\"` exports schemas, queries and data."),
    ("IMPORT", "`IMPORT GRAPH ALL FROM \"path\"` imports an exported graph."),
    ("USER", "A database user: `CREATE USER`, `SHOW USER`, `DROP USER name`."),
    ("ROLE", "A role grouping privileges: `CREATE ROLE r`, `GRANT ROLE r TO user`."),
    ("PRIVILEGE", "A privilege: `GRANT PRIVILEGE READ_DATA ON GRAPH g TO role`."),
    ("SECRET", "A secret for token-based authentication: `CREATE SECRET`."),
    ("TOKEN", "An authentication token: `SHOW TOKEN`."),
    ("PASSWORD", "`ALTER PASSWORD` changes the password of the current user."),
    ("SCHEMA", "`SHOW SCHEMA` (or `LS`) shows the schema of the current graph."),
    ("VERSION", "`VERSION` prints the GSQL version."),
    ("HELP", "`HELP` lists the shell commands."),
    ("QUIT", "Leaves the GSQL shell."),
    ("EXIT", "Leaves the GSQL shell."),
    ("BY", "Part of `ORDER BY`, `GROUP BY` and `TAGS (...) BY OR`."),
    ("ON", "`ON GRAPH g` in grants, `ADD INDEX i ON (attr)`, and `TO VECTOR ATTRIBUTE a ON VERTEX v` in loading jobs."),
    (
        "COMPRESS",
        "`STRING COMPRESS`: a deprecated string type that stored repeated values compactly; new schemas cannot use it.",
    ),
];

pub fn function(name: &str) -> Option<&'static Function> {
    FUNCTIONS.iter().find(|f| f.name.eq_ignore_ascii_case(name))
}

pub fn accumulator(name: &str) -> Option<&'static Accumulator> {
    ACCUMULATORS.iter().find(|a| a.name.eq_ignore_ascii_case(name))
}

pub fn keyword(name: &str) -> Option<&'static str> {
    // `ELSE  IF` is one token, spaces and all.
    let upper =
        name.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_uppercase().replace("POST_ACCUM", "POST-ACCUM");
    KEYWORDS.iter().find(|(k, _)| *k == upper).map(|(_, doc)| *doc)
}

pub fn primitive_type(name: &str) -> Option<&'static str> {
    PRIMITIVE_TYPES.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, doc)| *doc)
}

pub fn constant(name: &str) -> Option<&'static str> {
    CONSTANTS.iter().find(|(k, _)| *k == name).map(|(_, doc)| *doc)
}

/// How many arguments a signature takes, as (required, most): plain names are
/// required, `[name]` and `[a, b]` are optional. `None` when the signature is
/// variadic or not a plain list of names (`[DISTINCT] setExp`, `INTERVAL ...`).
pub fn arity(params: &[&str]) -> Option<(usize, usize)> {
    let name = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
    let (mut required, mut most) = (0, 0);
    for param in params {
        match param.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
            Some(group) => {
                let names: Vec<_> = group.split(',').map(str::trim).collect();
                names.iter().all(|n| name(n)).then_some(())?;
                most += names.len();
            }
            None => {
                name(param).then_some(())?;
                required += 1;
                most += 1;
            }
        }
    }
    Some((required, most))
}

pub fn find_method<'a>(methods: &'a [Method], name: &str) -> Option<&'a Method> {
    methods.iter().find(|m| m.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_are_case_insensitive() {
        assert_eq!(function("COUNT").unwrap().name, "count");
        assert_eq!(accumulator("sumaccum").unwrap().name, "SumAccum");
        assert!(keyword("post_accum").is_some());
        assert!(primitive_type("int").is_some());
    }

    #[test]
    fn arity_of_signatures() {
        assert_eq!(arity(&[]), Some((0, 0)));
        assert_eq!(arity(&["num", "[integer]"]), Some((1, 2)));
        assert_eq!(arity(&["[index, value]"]), Some((0, 2)));
        assert_eq!(arity(&["key", "..."]), None);
        assert_eq!(arity(&["[DISTINCT] setExp"]), None);
        assert_eq!(arity(&["date", "INTERVAL int_value time_unit"]), None);
        assert_eq!(arity(&["exp", "exp ...]"]), None);
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = FUNCTIONS.iter().map(|f| f.name.to_ascii_lowercase()).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate built-in function");
    }

    #[test]
    fn methods_are_told_apart_by_address() {
        let mut tables: Vec<&[Method]> = vec![
            VERTEX_METHODS,
            EDGE_METHODS,
            VERTEX_SET_METHODS,
            LIST_METHODS,
            SET_METHODS,
            MAP_METHODS,
            JSON_OBJECT_METHODS,
            JSON_ARRAY_METHODS,
            FILE_METHODS,
        ];
        tables.extend(ACCUMULATORS.iter().map(|a| a.methods));
        let mut addresses: Vec<*const Method> =
            tables.iter().flat_map(|t| t.iter().map(|m| m as *const Method)).collect();
        let before = addresses.len();
        addresses.sort();
        addresses.dedup();
        assert_eq!(before, addresses.len(), "two tables share a method entry");
    }
}
