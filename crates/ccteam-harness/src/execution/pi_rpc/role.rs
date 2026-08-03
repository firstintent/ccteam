//! Role metadata selection and immutable Pi system-prompt projection.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{ccteam_root_from_env, HarnessError};

#[derive(Debug, Clone)]
pub struct PiRoleDocument {
    pub frontmatter: Value,
    pub body: String,
}

pub type PiRoleReader =
    Arc<dyn Fn(&Path, &str) -> Result<Option<PiRoleDocument>, String> + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct PiRoleSelection {
    pub role: String,
    pub prompt_path: Option<PathBuf>,
    pub prompt_sha: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub fn resolve_role(
    reader: &PiRoleReader,
    project_dir: &Path,
    sid: &str,
    role: &str,
) -> Result<PiRoleSelection, HarnessError> {
    if role.is_empty() {
        return Ok(PiRoleSelection {
            role: String::new(),
            prompt_path: None,
            prompt_sha: None,
            model: None,
            effort: None,
        });
    }

    let document = reader(project_dir, role)
        .map_err(|error| HarnessError::SpawnFailed(format!("read Pi role `{role}`: {error}")))?
        .ok_or_else(|| HarnessError::SpawnFailed(format!("Pi role `{role}` does not exist")))?;
    if document.body.trim().is_empty() {
        return Err(HarnessError::SpawnFailed(format!(
            "Pi role `{role}` has an empty markdown body"
        )));
    }

    let model = portable_model(&document.frontmatter, role)?;
    let effort = metadata_string(&document.frontmatter, "effort")?;
    let (prompt_path, prompt_sha) = project_body(sid, document.body.as_bytes())?;
    Ok(PiRoleSelection {
        role: role.to_string(),
        prompt_path: Some(prompt_path),
        prompt_sha: Some(prompt_sha),
        model,
        effort,
    })
}

fn portable_model(frontmatter: &Value, role: &str) -> Result<Option<String>, HarnessError> {
    if let Some(value) = frontmatter.get("pi").and_then(|pi| pi.get("model")) {
        let value = value.as_str().ok_or_else(|| {
            HarnessError::SpawnFailed("Pi role metadata `model` must be a string".to_string())
        })?;
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        if !value.contains('/') {
            return Err(HarnessError::SpawnFailed(format!(
                "Pi role `{role}` model `{value}` must be canonical provider/model"
            )));
        }
        return Ok(Some(value.to_string()));
    }
    Ok(frontmatter
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.contains('/'))
        .map(str::to_string))
}

fn metadata_string(frontmatter: &Value, field: &str) -> Result<Option<String>, HarnessError> {
    let nested = frontmatter.get("pi").and_then(|pi| pi.get(field));
    let generic = frontmatter.get(field);
    let Some(value) = nested.or(generic) else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(HarnessError::SpawnFailed(format!(
            "Pi role metadata `{field}` must be a string"
        )));
    };
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
}

fn project_body(sid: &str, body: &[u8]) -> Result<(PathBuf, String), HarnessError> {
    let root = ccteam_root_from_env()
        .ok_or_else(|| HarnessError::SpawnFailed("cannot resolve CCTEAM_HOME".into()))?;
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    };
    let roles_dir = root.join("runtime").join("pi").join("roles");
    std::fs::create_dir_all(&roles_dir)?;
    let sha = format!("{:x}", Sha256::digest(body));
    let path = roles_dir.join(format!("{sid}-{sha}.md"));
    if path.exists() {
        let existing = std::fs::read(&path)?;
        if existing != body {
            return Err(HarnessError::Io(format!(
                "Pi role projection hash collision at {}",
                path.display()
            )));
        }
        set_private_permissions(&path)?;
        return Ok((path, sha));
    }

    let tmp = roles_dir.join(format!(".{sid}-{sha}-{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
    set_private_permissions(&path)?;
    if let Ok(dir) = std::fs::File::open(&roles_dir) {
        let _ = dir.sync_all();
    }
    Ok((path, sha))
}

fn set_private_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_specific_metadata_precedes_generic_metadata() {
        let value = serde_json::json!({
            "model": "generic/model",
            "effort": "low",
            "pi": {"model": "pi/model", "effort": "high"}
        });
        assert_eq!(
            metadata_string(&value, "model").unwrap().as_deref(),
            Some("pi/model")
        );
        assert_eq!(
            metadata_string(&value, "effort").unwrap().as_deref(),
            Some("high")
        );
    }

    #[test]
    fn noncanonical_generic_model_is_not_guessed_for_pi() {
        let value = serde_json::json!({"model":"sonnet"});
        assert_eq!(portable_model(&value, "reviewer").unwrap(), None);
    }
}
