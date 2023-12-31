use super::{CodegenError, Object, Value};

const HEADER_COMMENT: &str =
    "// This file was @generated by Runway. It is not intended for manual editing.";

pub(super) fn generate_typescript(tree: &Value) -> Result<String, CodegenError> {
    let Value::Object(root) = tree else { panic!() };

    let mut s = String::new();
    s.push_str(HEADER_COMMENT);
    s.push_str("\nexport default ");
    s.push_str(&format_object(root, 0, false));
    s.push_str(" as const;\n");

    Ok(s)
}

pub(super) fn generate_typescript_declaration(tree: &Value) -> Result<String, CodegenError> {
    let Value::Object(root) = tree else { panic!() };

    let mut s = String::new();
    s.push_str(HEADER_COMMENT);
    s.push_str("\ndeclare const assets: ");
    s.push_str(&format_object(root, 0, true));
    s.push_str(";\n\nexport = assets;\n");

    Ok(s)
}

fn format_object(obj: &Object, indent_level: usize, declaration: bool) -> String {
    let indent = "\t".repeat(indent_level);
    let indent_plus1 = "\t".repeat(indent_level + 1);

    let line_ending = match declaration {
        true => ';',
        false => ',',
    };

    let mut s = String::new();
    s.push_str("{\n");

    let iter = obj.0.iter().peekable();

    for (k, v) in iter {
        s.push_str(&(indent_plus1.clone() + &format_key(k) + ": "));

        match v {
            Value::Object(subobj) => {
                s.push_str(&format_object(subobj, indent_level + 1, declaration));
                s.push(line_ending);
                s.push('\n');
            }
            Value::Id(id) => {
                if declaration {
                    s.push_str("string");
                } else {
                    s.push_str(&format_string(id));
                }
                s.push(line_ending);
                s.push('\n');
            }
        }
    }

    s.push_str(&(indent + "}"));

    s
}

fn is_id_start(c: char) -> bool {
    unicode_ident::is_xid_start(c) || c == '$' || c == '_'
}
fn is_id_part(c: char) -> bool {
    unicode_ident::is_xid_continue(c) || c == '$'
}
fn is_id<S: AsRef<str>>(s: S) -> bool {
    !s.as_ref().is_empty()
        && is_id_start(s.as_ref().chars().next().unwrap())
        && s.as_ref().chars().skip(1).all(is_id_part)
}

fn format_key<S: AsRef<str>>(s: S) -> String {
    if is_id(&s) {
        s.as_ref().to_string()
    } else {
        format_string(s)
    }
}

fn format_string<S: AsRef<str>>(s: S) -> String {
    "\"".to_string() + s.as_ref() + "\""
}
