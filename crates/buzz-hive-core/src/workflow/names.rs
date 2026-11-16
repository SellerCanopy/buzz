//! The name rules the format uses, written out so the crate needs no regex dependency.

use crate::domain::workflow::{ParamType, Parameter};

/// `^[a-z0-9][a-z0-9_-]{0,63}$`: what an artifact or input name looks like.
pub fn artifact_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// The text a check message gives for the artifact name rule.
pub const ARTIFACT_NAME_RULE: &str = "^[a-z0-9][a-z0-9_-]{0,63}$";

/// `^[a-z0-9][a-z0-9_-]{0,31}$`: what a workflow package a person saves is called. It is
/// shorter than an artifact name because it is also a folder name when the package is exported.
pub fn package_name(name: &str) -> bool {
    name.len() <= 32 && artifact_name(name)
}

/// The text a refusal gives for the package name rule.
pub const PACKAGE_NAME_RULE: &str =
    "1 to 32 lower-case letters, digits, - and _, starting with a letter or digit";

/// `^[a-z][a-z0-9_]*$`. It has no dot, so a settings key such as
/// `stage.<id>.params.<name>` splits unambiguously.
pub fn param_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    match bytes.split_first() {
        Some((first, rest)) => {
            first.is_ascii_lowercase()
                && rest
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        }
        None => false,
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]*$`: a bare plugin command name.
pub fn command_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    match bytes.split_first() {
        Some((first, rest)) => {
            first.is_ascii_alphanumeric()
                && rest
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'_' | b'-'))
        }
        None => false,
    }
}

/// An `owner/repo` as GitHub names one, whose repo part is not `.` or `..`.
pub fn repo_name(name: &str) -> bool {
    let Some((owner, repo)) = name.split_once('/') else {
        return false;
    };
    let owner_ok = owner
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let repo_ok = !repo.is_empty()
        && repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && repo != "."
        && repo != "..";
    owner_ok && repo_ok
}

/// Git's rules for a ref name, plus no leading `-`, so a name is never read as an option.
pub fn valid_branch_name(name: &str) -> bool {
    if name.is_empty()
        || name == "@"
        || name.starts_with('-')
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
    {
        return false;
    }
    if name
        .chars()
        .any(|c| (c as u32) < 0x20 || c as u32 == 0x7f || " ~^:?*[\\".contains(c))
    {
        return false;
    }
    !name
        .split('/')
        .any(|part| part.starts_with('.') || part.ends_with(".lock"))
}

/// What a Buzz agent name has to look like before the check asks Buzz whether it exists:
/// 1 to 64 characters, no control characters, no space at either end.
pub fn agent_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=64).contains(&count) && !name.chars().any(char::is_control) && name.trim() == name
}

/// Whether `value` fits the parameter's type. An empty value fits every type, since an
/// unset parameter renders as empty text.
pub fn check_param_value(parameter: &Parameter, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    let name = &parameter.name;
    match parameter.kind {
        Some(ParamType::Number) => match value.parse::<f64>() {
            Ok(number) if number.is_finite() => Ok(()),
            _ => Err(format!("parameter {name}: want a number, got {value:?}")),
        },
        Some(ParamType::Bool) => {
            if value == "true" || value == "false" {
                Ok(())
            } else {
                Err(format!(
                    "parameter {name}: want true or false, got {value:?}"
                ))
            }
        }
        Some(ParamType::Choice) => {
            if parameter.choices.iter().any(|c| c == value) {
                Ok(())
            } else {
                Err(format!(
                    "parameter {name}: want one of {}, got {value:?}",
                    go_list(&parameter.choices)
                ))
            }
        }
        Some(ParamType::Branch) => {
            if valid_branch_name(value) {
                Ok(())
            } else {
                Err(format!(
                    "parameter {name}: {value:?} is not a valid branch name"
                ))
            }
        }
        Some(ParamType::Text) | None => Ok(()),
    }
}

/// A list as Go prints it with `%v`: `[a b c]`.
pub fn go_list<S: AsRef<str>>(items: &[S]) -> String {
    let parts: Vec<&str> = items.iter().map(AsRef::as_ref).collect();
    format!("[{}]", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_names() {
        assert!(artifact_name("plan"));
        assert!(artifact_name("0-a_b"));
        assert!(!artifact_name("Bad Name"));
        assert!(!artifact_name("-a"));
        assert!(!artifact_name(""));
        assert!(artifact_name(&"a".repeat(64)));
        assert!(!artifact_name(&"a".repeat(65)));
    }

    #[test]
    fn repo_names() {
        assert!(repo_name("owner/repo"));
        assert!(repo_name("a-b/c.d_e"));
        assert!(!repo_name("owner/.."));
        assert!(!repo_name("owner"));
        assert!(!repo_name("-o/r"));
    }

    #[test]
    fn branch_names() {
        assert!(valid_branch_name("kiln/12"));
        assert!(!valid_branch_name("-x"));
        assert!(!valid_branch_name("a..b"));
        assert!(!valid_branch_name("a.lock"));
    }
}
