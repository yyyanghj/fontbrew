use std::{
    io::{self, IsTerminal, Write},
    path::Path,
};

use dialoguer::{
    console::{style, Style, Term},
    theme::ColorfulTheme,
    MultiSelect,
};
use fontbrew_core::{ExecutionPolicy, FamilyName, PlanRisk};

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

    fn confirm_self_update(
        &mut self,
        executable_path: &Path,
        target_version: &str,
        assume_yes: bool,
    ) -> CliResult<()>;

    fn select_families(&mut self, families: &[FamilyName]) -> CliResult<Vec<FamilyName>>;
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

    fn confirm_self_update(
        &mut self,
        executable_path: &Path,
        target_version: &str,
        assume_yes: bool,
    ) -> CliResult<()> {
        if assume_yes {
            return Ok(());
        }

        if !self.stdin.is_terminal() {
            return Err(CliError::SelfUpdatePromptUnavailable {
                message: format!(
                    "approval is required before replacing {} with fontbrew {target_version}; rerun with --yes or --dry-run, or use an interactive terminal",
                    executable_path.display()
                ),
            });
        }

        if self.prompt_for_self_update(executable_path, target_version)? {
            Ok(())
        } else {
            Err(CliError::Cancelled)
        }
    }

    fn select_families(&mut self, families: &[FamilyName]) -> CliResult<Vec<FamilyName>> {
        if !self.stdin.is_terminal() {
            return Err(CliError::FamilySelectionRequired {
                families: families.to_vec(),
            });
        }

        let labels = families
            .iter()
            .map(|family| family.as_str().to_string())
            .collect::<Vec<_>>();
        let theme = family_selection_theme();
        let selections = MultiSelect::with_theme(&theme)
            .with_prompt("Select font families to install")
            .items(&labels)
            .interact_on_opt(&Term::stderr())
            .map_err(io::Error::other)?;

        let Some(selections) = selections else {
            return Err(CliError::Cancelled);
        };
        if selections.is_empty() {
            return Err(CliError::Cancelled);
        }

        Ok(selections
            .into_iter()
            .map(|index| families[index].clone())
            .collect())
    }
}

fn family_selection_theme() -> ColorfulTheme {
    ColorfulTheme {
        checked_item_prefix: style("[x]".to_string()).for_stderr().green().bold(),
        unchecked_item_prefix: style("[ ]".to_string()).for_stderr().black().bright(),
        active_item_style: Style::new().for_stderr().cyan().bold(),
        values_style: Style::new().for_stderr().green().bold(),
        ..ColorfulTheme::default()
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

    fn prompt_for_self_update(
        &mut self,
        executable_path: &Path,
        target_version: &str,
    ) -> CliResult<bool> {
        {
            let mut stderr = self.stderr.lock();
            write!(
                stderr,
                "Replace {} with fontbrew {target_version}? [y/N] ",
                executable_path.display()
            )?;
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

    fn confirm_self_update(
        &mut self,
        executable_path: &Path,
        target_version: &str,
        assume_yes: bool,
    ) -> CliResult<()> {
        if assume_yes {
            return Ok(());
        }

        Err(CliError::SelfUpdateApprovalRequired {
            message: format!(
                "approval is required before replacing {} with fontbrew {target_version}; rerun with --yes or --dry-run",
                executable_path.display()
            ),
        })
    }

    fn select_families(&mut self, families: &[FamilyName]) -> CliResult<Vec<FamilyName>> {
        Err(CliError::FamilySelectionRequired {
            families: families.to_vec(),
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
