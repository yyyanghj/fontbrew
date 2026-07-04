# Confirmation Boundaries Research

Date: 2026-07-04

## Question

Should Fontbrew's core layer enforce confirmation, or should confirmation live only in the CLI?

## Findings

Git `clean` refuses to delete files unless `-f` is provided when `clean.requireForce` is true. It also has an interactive mode, but the safety rule is modeled as an explicit force requirement, not just as a prompt. Source: https://git-scm.com/docs/git-clean

`kubectl drain` refuses to proceed in specific unsafe cases unless explicit flags are provided. It does not delete unmanaged pods unless `--force` is used, and it does not proceed with DaemonSet-managed pods unless `--ignore-daemonsets` is used. Source: https://kubernetes.io/docs/reference/kubectl/generated/kubectl_drain/

Terraform separates planning from applying. `terraform apply` prompts for approval in automatic plan mode, `-auto-approve` skips approval, and JSON mode implies non-interactive input and requires either `-auto-approve` or a saved plan. A saved plan file itself is treated as approval of the already-inspected plan. Source: https://developer.hashicorp.com/terraform/cli/commands/apply

npm `uninstall` removes only what npm installed on the package's behalf and updates dependency metadata without an interactive confirmation prompt. Its safety boundary is package ownership and command scope rather than a prompt. Source: https://docs.npmjs.com/cli/v8/commands/npm-uninstall/

## Conclusion

The common pattern is not "domain/core asks for confirmation." The better pattern is:

- UI layer owns prompts and human interaction.
- Core owns operation invariants and refuses unsafe operations unless the request carries explicit intent.
- Destructive or risky operations use explicit flags, saved plans, dry-run modes, or force/approve options.
- Machine-readable/non-interactive modes do not prompt.

For Fontbrew, `fontbrew-core` should not model "user confirmation" as a UI concept. It should validate an `ExecutionPolicy` or `ApplyOptions` that states whether risk acceptance is allowed. The CLI or future GUI obtains that acceptance from the user and passes the policy into core.
