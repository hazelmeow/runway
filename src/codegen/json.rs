use super::{CodegenError, Object, Value};

pub(super) fn generate_json(tree: &Value) -> Result<String, CodegenError> {
    let Value::Object(root) = tree else { panic!() };
    Ok(format_object(root, 0))
}

fn format_object(obj: &Object, indent_level: usize) -> String {
    let indent = "\t".repeat(indent_level);
    let indent_plus1 = "\t".repeat(indent_level + 1);

    let mut s = String::new();
    s.push_str("{\n");

    let mut iter = obj.0.iter().peekable();

    while let Some((k, v)) = iter.next() {
        s.push_str(&(indent_plus1.clone() + &format_string(k) + ": "));

        match v {
            Value::Object(subobj) => {
                s.push_str(&format_object(&subobj, indent_level + 1));

                if iter.peek().is_some() {
                    s.push_str(",");
                }
                s.push_str("\n");
            }
            Value::Id(id) => {
                s.push_str(&format_string(id));

                if iter.peek().is_some() {
                    s.push_str(",");
                }
                s.push_str("\n");
            }
        }
    }

    s.push_str(&(indent + "}"));

    s
}

fn format_string<S: AsRef<str>>(s: S) -> String {
    "\"".to_string() + s.as_ref() + "\""
}
