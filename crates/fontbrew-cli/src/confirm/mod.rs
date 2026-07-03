use std::io::{self, IsTerminal, Write};

use fontbrew_core::{ExecutionPolicy, PlanRisk};

use crate::exit::{CliError, CliResult};

#[derive(Debug, Clone, Copy)]
pub struct ConfirmationOptions {
    pub assume_yes: bool,
    pub dry_run: bool,
}

pub trait Confirmer {
    fn execution_policy(
        &mut self,
        risks: &[PlanRisk],
        options: ConfirmationOptions,
    ) -> CliResult<ExecutionPolicy>;
}

pub struct HumanConfirmer {
    stdin: io::Stdin,
    stderr: io::Stderr,
}

impl HumanConfirmer {
    pub fn new() -> Self {
        Self {
            stdin: io::stdin(),
            stderr: io::stderr(),
        }
    }
}

impl Confirmer for HumanConfirmer {
    fn execution_policy(
        &mut self,
        risks: &[PlanRisk],
        options: ConfirmationOptions,
    ) -> CliResult<ExecutionPolicy> {
        if options.dry_run {
            return Ok(ExecutionPolicy::DryRun);
        }

        if risks.is_empty() {
            return Ok(ExecutionPolicy::SafeOnly);
        }

        if options.assume_yes {
            return Ok(ExecutionPolicy::AssumeYes);
        }

        if !self.stdin.is_terminal() {
            return Err(CliError::PromptUnavailable {
                risks: risks.to_vec(),
            });
        }

        let approved = self.prompt_for_approval(risks)?;
        if approved {
            Ok(ExecutionPolicy::AllowUserApprovedRisk)
        } else {
            Err(CliError::Cancelled)
        }
    }
}

impl HumanConfirmer {
    fn prompt_for_approval(&mut self, risks: &[PlanRisk]) -> CliResult<bool> {
        {
            let mut stderr = self.stderr.lock();
            writeln!(stderr, "The plan has {} risk(s):", risks.len())?;
            for risk in risks {
                writeln!(stderr, "- {risk:?}")?;
            }
            write!(stderr, "Continue? [y/N] ")?;
            stderr.flush()?;
        }

        let mut answer = String::new();
        self.stdin.read_line(&mut answer)?;
        let answer = answer.trim().to_ascii_lowercase();

        Ok(answer == "y" || answer == "yes")
    }
}

pub struct JsonConfirmer;

impl JsonConfirmer {
    pub fn new() -> Self {
        Self
    }
}

impl Confirmer for JsonConfirmer {
    fn execution_policy(
        &mut self,
        risks: &[PlanRisk],
        options: ConfirmationOptions,
    ) -> CliResult<ExecutionPolicy> {
        if options.dry_run {
            return Ok(ExecutionPolicy::DryRun);
        }

        if risks.is_empty() {
            return Ok(ExecutionPolicy::SafeOnly);
        }

        if options.assume_yes {
            return Ok(ExecutionPolicy::AssumeYes);
        }

        Err(CliError::ApprovalRequired {
            risks: risks.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use fontbrew_core::{FamilyName, PackageId, PlanRisk};

    use super::*;

    fn package_id(id: &str) -> PackageId {
        PackageId::parse(id).expect("test package id should parse")
    }

    fn risk() -> PlanRisk {
        PlanRisk::UnmanagedFontOverlap {
            family_name: FamilyName::new("Source Code Pro"),
            description: "unmanaged same-family font".to_string(),
        }
    }

    #[test]
    fn human_confirmer_assume_yes_maps_risky_plan_to_approved_policy() {
        let mut confirmer = HumanConfirmer::new();

        let policy = confirmer
            .execution_policy(
                &[risk()],
                ConfirmationOptions {
                    assume_yes: true,
                    dry_run: false,
                },
            )
            .expect("assume yes should approve risks");

        assert_eq!(policy, ExecutionPolicy::AssumeYes);
    }

    #[test]
    fn json_confirmer_requires_explicit_approval_for_risky_apply() {
        let mut confirmer = JsonConfirmer::new();

        let error = confirmer
            .execution_policy(
                &[PlanRisk::Conflict {
                    package_id: package_id("source-code-pro"),
                    description: "activation conflict".to_string(),
                }],
                ConfirmationOptions {
                    assume_yes: false,
                    dry_run: false,
                },
            )
            .expect_err("JSON mode should require --yes or --dry-run");

        assert!(matches!(error, CliError::ApprovalRequired { .. }));
    }
}
