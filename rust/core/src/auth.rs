//! Hashed API tokens with exact, method-specific scopes and compatible state.
use anyhow::{Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Serialize, Deserialize)]
pub struct Scope {
    pub path: String,
    pub methods: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Token {
    pub hash: String,
    pub username: String,
    pub created_at: Value,
    pub scopes: Vec<Scope>,
}
pub fn read(document: &Value) -> Result<Vec<Token>> {
    let value = &document["root"]["api_tokens"];
    if value.is_null() || value == "" {
        return Ok(vec![]);
    }
    Ok(serde_json::from_value(value.clone())?)
}
fn pattern(path: &str) -> Result<Regex> {
    if path.len() > 512 {
        bail!("API scope path is too long");
    }
    Ok(Regex::new(&format!("^(?:{path})$"))?)
}
pub fn permits(tokens: &[Token], secret: &str, username: &str, path: &str, method: &str) -> bool {
    if secret.len() > 256 || secret.is_empty() {
        return false;
    }
    let hash = hex::encode(crate::crypto::hash(secret.as_bytes()));
    tokens.iter().any(|t| {
        crate::crypto::equal(hash.as_bytes(), t.hash.to_ascii_lowercase().as_bytes())
            && t.username.eq_ignore_ascii_case(username)
            && t.scopes.iter().any(|s| {
                s.methods.iter().any(|m| m.eq_ignore_ascii_case(method))
                    && pattern(&s.path).is_ok_and(|re| re.is_match(path))
            })
    })
}
pub fn issue(
    username: String,
    mut scopes: Vec<Scope>,
    catalog: &[Scope],
) -> Result<(String, Token)> {
    if scopes.is_empty() || scopes.len() > 64 {
        bail!("select between 1 and 64 API scopes");
    }
    for scope in &mut scopes {
        let allowed = catalog
            .iter()
            .find(|s| s.path == scope.path)
            .ok_or_else(|| anyhow::anyhow!("unknown API scope"))?;
        pattern(&scope.path)?;
        if scope.methods.is_empty() {
            bail!("API scope requires a method");
        }
        for method in &mut scope.methods {
            *method = method.to_ascii_uppercase();
            if !allowed.methods.contains(method) {
                bail!("method is unavailable for this API scope");
            }
        }
        scope.methods.sort();
        scope.methods.dedup();
    }
    let secret = hex::encode(crate::crypto::random::<32>());
    let token = Token {
        hash: hex::encode(crate::crypto::hash(secret.as_bytes())),
        username,
        created_at: json!(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()),
        scopes,
    };
    Ok((secret, token))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_cannot_escalate_or_match_neighboring_routes() {
        let scope = Scope {
            path: "/api/apps".into(),
            methods: vec!["GET".into()],
        };
        let (secret, token) = issue("test".into(), vec![scope.clone()], &[scope]).unwrap();
        assert_ne!(token.hash, secret);
        let tokens = vec![token];
        assert!(permits(&tokens, &secret, "TEST", "/api/apps", "GET"));
        assert!(!permits(&tokens, &secret, "test", "/api/apps", "POST"));
        assert!(!permits(
            &tokens,
            &secret,
            "test",
            "/api/apps/delete",
            "GET"
        ));
        assert!(!permits(&tokens, &secret, "other", "/api/apps", "GET"));
        assert!(!permits(&tokens, "forged", "test", "/api/apps", "GET"));
        let re = Scope {
            path: "/api/apps/[^/]+/cover".into(),
            methods: vec!["GET".into()],
        };
        let (_, wildcard) = issue("test".into(), vec![re.clone()], &[re]).unwrap();
        assert!(
            pattern(&wildcard.scopes[0].path)
                .unwrap()
                .is_match("/api/apps/123/cover")
        );
        assert!(
            !pattern(&wildcard.scopes[0].path)
                .unwrap()
                .is_match("/api/apps/a/b/cover")
        );
        assert!(
            issue(
                "test".into(),
                vec![Scope {
                    path: "/api/token".into(),
                    methods: vec!["POST".into()]
                }],
                &[]
            )
            .is_err()
        );
    }
}
