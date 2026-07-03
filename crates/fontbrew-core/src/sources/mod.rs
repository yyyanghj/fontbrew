use crate::error::{FontbrewError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepo {
    pub owner: String,
    pub repo: String,
}

impl GitHubRepo {
    pub fn parse(input: impl AsRef<str>) -> Result<Self> {
        let input = input.as_ref();
        let parts = input.split('/').collect::<Vec<_>>();

        if parts.len() != 2 {
            return invalid_github_repo(input, "expected owner/repo");
        }

        let owner = parts[0];
        let repo = parts[1];
        validate_owner(owner, input)?;
        validate_repo_name(repo, input)?;

        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

fn validate_owner(owner: &str, full_input: &str) -> Result<()> {
    if owner.is_empty() {
        return invalid_github_repo(full_input, "owner cannot be empty");
    }

    validate_segment_edges(owner, full_input, "owner")?;

    if owner
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Ok(());
    }

    invalid_github_repo(
        full_input,
        "owner may only contain ASCII letters, digits, and hyphens",
    )
}

fn validate_repo_name(repo: &str, full_input: &str) -> Result<()> {
    if repo.is_empty() {
        return invalid_github_repo(full_input, "repo cannot be empty");
    }

    validate_segment_edges(repo, full_input, "repo")?;

    if repo.contains("..") {
        return invalid_github_repo(full_input, "repo cannot contain consecutive dots");
    }

    if repo
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Ok(());
    }

    invalid_github_repo(
        full_input,
        "repo may only contain ASCII letters, digits, dots, underscores, and hyphens",
    )
}

fn validate_segment_edges(segment: &str, full_input: &str, label: &str) -> Result<()> {
    let bytes = segment.as_bytes();

    if bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        return Ok(());
    }

    invalid_github_repo(
        full_input,
        &format!("{label} must start and end with an ASCII letter or digit"),
    )
}

fn invalid_github_repo<T>(input: &str, reason: &str) -> Result<T> {
    Err(FontbrewError::RegistryValidationFailed {
        message: format!("invalid GitHub repository {input:?}: {reason}"),
    })
}

#[cfg(test)]
mod tests {
    use crate::sources::GitHubRepo;

    #[test]
    fn github_repo_parse_accepts_owner_repo_syntax() {
        for input in ["rsms/inter", "JetBrains/JetBrainsMono", "owner/repo.name_2"] {
            let repo = GitHubRepo::parse(input).expect("repo should parse");

            assert_eq!(repo.label(), input);
        }
    }

    #[test]
    fn github_repo_parse_rejects_unsafe_syntax() {
        for input in [
            "",
            "rsms",
            "rsms/",
            "/inter",
            "rsms//inter",
            "rsms/inter/archive",
            "rsms/inter..mono",
            "rsms/-inter",
            "-rsms/inter",
            "rsms/inter/",
        ] {
            assert!(
                GitHubRepo::parse(input).is_err(),
                "{input:?} should be rejected"
            );
        }
    }
}
